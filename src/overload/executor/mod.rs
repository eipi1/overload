#![allow(clippy::upper_case_acronyms)]

#[cfg(feature = "cluster")]
pub mod cluster;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;

use hyper::client::{Client, HttpConnector, ResponseFuture};
use hyper::{Body, Error, Request};

use lazy_static::lazy_static;
use prometheus::{
    // labels,
    linear_buckets,
    // opts, register_counter, register_gauge_vec,
    register_histogram_vec,
    register_int_counter_vec,
    // TextEncoder,
    // Counter, Encoder, exponential_buckets, GaugeVec,
    HistogramVec,
    IntCounterVec,
};

use tokio::sync::RwLock;
use tokio_stream::StreamExt;
use tracing::{error, info, trace};

use super::HttpReq;
use crate::generator::{request_generator_stream, RequestGenerator};
use crate::JobStatus;
use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::FutureExt;
use http::header::HeaderName;
use http::HeaderValue;
use std::str::FromStr;

#[allow(dead_code)]
enum HttpRequestState {
    INIT,
    InProgress,
    ResponseReceived,
    DONE,
}

#[must_use = "futures do nothing unless polled"]
struct HttpRequestFuture<'a> {
    state: HttpRequestState,
    timer: Option<Instant>,
    job_id: String,
    request: Pin<Box<ResponseFuture>>,
    body: Option<BoxFuture<'a, Result<Bytes, Error>>>,
    status: Option<String>,
}

impl Future for HttpRequestFuture<'_> {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _e = "ERROR".to_string();
        if let HttpRequestState::INIT = self.state {
            //todo move outside
            // METRIC_QPS_COUNTER
            //     .with_label_values(&[self.job_id.as_str()])
            //     .inc_by(1);
            self.timer = Some(Instant::now());
            self.state = HttpRequestState::InProgress;
        }

        if self.body.is_some() {
            let fut = self.body.as_mut().unwrap();
            return match Pin::new(fut).poll(cx) {
                Poll::Ready(_) => {
                    let elapsed = self.timer.unwrap().elapsed().as_millis() as f64;
                    trace!(
                        "HttpRequestFuture [{}] - body - Ready, elapsed={}",
                        &self.job_id,
                        &elapsed
                    );
                    //todo move outside
                    // METRIC_RESPONSE_TIME
                    //     .with_label_values(&[
                    //         self.job_id.as_str(),
                    //         self.status.as_ref().unwrap_or(&e),
                    //     ])
                    //     .observe(elapsed);
                    Poll::Ready(())
                }
                Poll::Pending => {
                    trace!("HttpRequestFuture [{}] - body - Pending", &self.job_id);
                    Poll::Pending
                }
            };
        } else {
            trace!("HttpRequestFuture [{}] - body - None", &self.job_id);
        }

        trace!("HttpRequestFuture [{}] - request - polling", &self.job_id);
        match Pin::new(&mut self.request).poll(cx) {
            Poll::Pending => {
                trace!("HttpRequestFuture [{}] - request - Pending", &self.job_id);
                Poll::Pending
            }
            Poll::Ready(val) => {
                self.state = HttpRequestState::ResponseReceived;
                trace!("HttpRequestFuture [{}] - request - Ready", &self.job_id);
                match val {
                    Ok(response) => {
                        self.status = Some(response.status().to_string());
                        let elapsed = self.timer.unwrap().elapsed().as_millis() as f64;
                        trace!(
                            "HttpRequestFuture [{}] - request - Ready - Ok, status: {:?}, elapsed={}",
                            &self.job_id,
                            &self.status,
                            &elapsed
                        );
                        //todo move outside
                        // METRIC_HEADER_TIME
                        //     .with_label_values(&[
                        //         self.job_id.as_str(),
                        //         self.status.as_ref().unwrap_or(&e).as_str(),
                        //     ])
                        //     .observe(elapsed);
                        let bytes = hyper::body::to_bytes(response.into_body());
                        self.body = Some(bytes.boxed());
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                    Err(err) => {
                        trace!(
                            "HttpRequestFuture [{}] - request - Ready - ERROR: {}",
                            &self.job_id,
                            err
                        );
                        //todo move outside
                        // METRIC_HEADER_TIME
                        //     .with_label_values(&[self.job_id.as_str(), err.to_string().as_str()])
                        //     .observe(self.timer.unwrap().elapsed().as_millis() as f64);
                        Poll::Ready(())
                    }
                }
            }
        }
    }
}

lazy_static! {
    pub static ref METRIC_QPS_COUNTER: IntCounterVec =
        register_int_counter_vec!("overload_qps_counter", "qps counter", &["request_id"]).unwrap();
    pub static ref METRIC_RESPONSE_TIME: HistogramVec = register_histogram_vec!(
        "overload_response_time",
        "Keeps track of response time",
        &["request_id", "status"],
        // exponential_buckets(50.0,0.5,10).unwrap()
        linear_buckets(50.0, 100.0, 10).unwrap()
    )
    .unwrap();

    pub static ref METRIC_HEADER_TIME: HistogramVec = register_histogram_vec!(
        "overload_header_time",
        "Time between request sent and got the first header, optionally equal to overload_response_time",
        &["request_id", "status"],
        // exponential_buckets(50.0,0.9,10).unwrap()
        linear_buckets(50.0, 100.0, 10).unwrap()
    )
    // static ref RESPONSE_TIME_PERCENTILE_P99: GaugeVec = register_gauge_vec!(
    //     "overload_response_time_p99",
    //     "response time = 99th percentile",
    //     &["request_id"]
    // )
    .unwrap();

    pub static ref JOB_STATUS:RwLock<HashMap<String, JobStatus>> = RwLock::new(HashMap::new());
}

pub async fn init() {
    //start JOB_STATUS clean up task
    let mut interval = tokio::time::interval(Duration::from_secs(1800));
    loop {
        interval.tick().await;

        let indices = {
            let mut indices = vec![];
            let read_guard = JOB_STATUS.read().await;
            for (id, status) in read_guard.iter() {
                if matches!(
                    status,
                    JobStatus::Failed | JobStatus::Stopped | JobStatus::Completed
                ) {
                    indices.push(id.clone());
                }
            }
            indices
        };
        {
            let mut write_guard = JOB_STATUS.write().await;
            for id in indices {
                write_guard.remove(&id);
            }
        }
    }
}

pub async fn execute_request_generator(request: RequestGenerator, job_id: String) {
    let stream = request_generator_stream(request);
    tokio::pin!(stream);
    let client = Arc::new(Client::new());
    {
        let mut write_guard = JOB_STATUS.write().await;
        write_guard.insert(job_id.clone(), JobStatus::InProgress);
    }
    while let Some((qps, requests)) = stream.next().await {
        let stop = {
            let read_guard = JOB_STATUS.read().await;
            matches!(read_guard.get(&job_id), Some(JobStatus::Stopped))
        };
        if stop {
            break;
        }
        if qps != 0 {
            send_multiple_requests(requests, client.clone(), qps, job_id.clone()).await;
        }
    }
    {
        let mut write_guard = JOB_STATUS.write().await;
        if let Some(status) = write_guard.get(&job_id) {
            let job_finished = matches!(
                status,
                JobStatus::Stopped | JobStatus::Failed | JobStatus::Completed
            );
            if !job_finished {
                write_guard.insert(job_id.clone(), JobStatus::Completed);
            }
        } else {
            error!("Job Status should be present")
        }
    }
}

async fn send_requests(
    req: HttpReq,
    client: Arc<Client<HttpConnector, Body>>,
    count: i32,
    job_id: String,
) {
    info!("sending {} requests", count);
    let body = if let Some(body) = req.body {
        Bytes::from(body)
    } else {
        Bytes::new()
    };
    let mut request_futures = FuturesUnordered::new();
    for _ in 1..=count {
        //todo remove unwrap
        let mut request = Request::builder()
            .uri(req.url.as_str())
            .method(req.method.clone());
        let headers = request.headers_mut().unwrap();
        for (k, v) in req.headers.iter() {
            headers.insert(
                HeaderName::from_str(k.clone().as_str()).unwrap(),
                HeaderValue::from_str(v.clone().as_str()).unwrap(),
            );
        }
        let request = request.body(body.clone().into()).unwrap();
        let request = client.request(request);
        // request_futures.push(request);
        request_futures.push(HttpRequestFuture {
            state: HttpRequestState::INIT,
            timer: None,
            job_id: job_id.clone(),
            request: Pin::new(Box::new(request)),
            body: None,
            status: None,
        });
    }
    // QPS_COUNTER
    //     .with_label_values(&[job_id.as_str()])
    //     .inc_by(count as i64);
    // let instant = std::time::Instant::now();
    while let Some(_resp) = request_futures.next().await {
        trace!("Request completed");
    }
    // loop {
    //     let option = request_futures.next().await;
    //     if option.is_some(){
    //         debug!("Request completed");
    //         // break;
    //     }else {
    //         debug!("Request returned None");
    //         break;
    //     }
    // }
    // while request_futures.next().await.is_some() {}
    // request_futures.next().await;
}

async fn send_multiple_requests(
    requests: Vec<HttpReq>,
    client: Arc<Client<HttpConnector, Body>>,
    count: u32,
    job_id: String,
) {
    let n_req = requests.len();
    let qps_per_req = count / n_req as u32;
    let remainder = count % n_req as u32;

    if remainder > 0 {
        if let Some(req) = requests.get(0) {
            //0 is getting preference. Choosing random index instead of 0 should be better
            tokio::spawn(send_requests(
                req.clone(),
                client.clone(),
                remainder as i32,
                job_id.clone(),
            ));
        }
    }
    let mut requests = requests;
    if qps_per_req > 0 {
        let drain = requests.drain(..);
        for req in drain {
            tokio::spawn(send_requests(
                req,
                client.clone(),
                qps_per_req as i32,
                job_id.clone(),
            ));
        }
    }
}

pub(crate) async fn send_stop_signal(job_id: String) -> String {
    info!("Stopping the job {}", &job_id);
    //todo don't stop if already done
    let current_status = {
        let mut guard = JOB_STATUS.write().await;
        guard.insert(job_id.clone(), JobStatus::Stopped)
    };
    if let Some(status) = current_status {
        match status {
            JobStatus::Starting | JobStatus::InProgress => "Job stopped",
            JobStatus::Stopped => "Job already stopped",
            JobStatus::Completed => "Job already completed",
            JobStatus::Failed => "Job failed",
            _ => {
                error!("Invalid job status for job: {}", &job_id);
                "Invalid job status"
            }
        }
        .to_string()
    } else {
        "Job Not Found".to_string()
    }
}

pub(crate) async fn get_job_status(offset: usize, limit: usize) -> HashMap<String, JobStatus> {
    let mut job_status = HashMap::new();
    let guard = JOB_STATUS.read().await;
    let mut count = 0;
    for (id, status) in guard.iter() {
        count += 1;
        if count - offset > limit {
            break;
        }
        if count >= offset {
            job_status.insert(id.clone(), *status);
        }
    }
    job_status
}

#[cfg(test)]
mod test {
    #![allow(unused_imports)]

    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures_util::stream::Stream;
    use tokio::time::timeout;

    // use futures_util::core_reexport::time::Duration;
    use crate::executor::execute_request_generator;
    use crate::generator::{request_generator_stream, ConstantQPS, RequestGenerator};

    #[tokio::test]
    #[ignore]
    async fn test_execute_gen_req() {
        let generator = RequestGenerator::new(3, Vec::new(), Box::new(ConstantQPS { qps: 3 }));
        let stream = request_generator_stream(generator);
        tokio::pin!(stream);
        // execute_request_generator_stream(stream);
    }
}
