#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::FutureExt;
use hyper::client::{Client, HttpConnector, ResponseFuture};
use hyper::{Body, Error, Method, Request};

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
use tracing::{debug, error, info, trace};

use super::{HttpReq, ReqMethod};
use crate::generator::{request_generator_stream, RequestGenerator};
use crate::JobStatus;

lazy_static! {
    static ref METRIC_QPS_COUNTER: IntCounterVec =
        register_int_counter_vec!("overload_qps_counter", "qps counter", &["request_id"]).unwrap();
    static ref METRIC_RESPONSE_TIME: HistogramVec = register_histogram_vec!(
        "overload_response_time",
        "Keeps track of response time",
        &["request_id", "status"],
        // exponential_buckets(50.0,0.5,10).unwrap()
        linear_buckets(50.0, 100.0, 10).unwrap()
    )
    .unwrap();

    static ref METRIC_HEADER_TIME: HistogramVec = register_histogram_vec!(
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

    static ref JOB_STATUS:RwLock<HashMap<String, JobStatus>> = RwLock::new(HashMap::new());
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

// pub async fn execute<R, Q>(mut overload_req: OverloadRequest<R, Q>)
//     where
//         R: ReqSpec + Iterator<Item=HttpReq> + Clone,
//         Q: QPSSpec + Iterator<Item=i32> + Copy,
// {
//     debug!("Processing request");
//     let duration = overload_req.duration();
//     let qps_spec = overload_req.get_qps_spec();
//     let request_id = overload_req.get_request_id().clone();
//
//     let mut qps_stream = {
//         let qps_spec = *qps_spec;
//         // throttle(
//         //     Duration::from_secs(1),
//         //     futures::stream::iter(qps_spec.take(*duration as usize)),
//         // )
//         // futures::stream::iter(qps_spec.take(*duration as usize)).throttle(Duration::from_secs(1))
//         tokio_stream::iter(qps_spec.take(*duration as usize)).throttle(Duration::from_secs(1))
//     };
//     tokio::pin!(qps_stream);
//     let req_spec = overload_req.get_req_spec_mut();
//
//     let client = Arc::new(Client::new());
//     // let t_digest = Arc::new(TDigest::new_with_size(100));
//     // let t_digest = ArcSwap::new(Arc::new(TDigest::new_with_size(100)));
//     while let Some(qps) = qps_stream.next().await {
//         // let mut http_req: Option<HttpReq> = tmp_req_spec.next();
//         let mut reqs = req_spec.by_ref();
//         let mut http_req: Option<HttpReq> = reqs.next();
//         if http_req.is_none() {
//             reqs = req_spec.by_ref();
//             http_req = reqs.next();
//         }
//         if let Some(req) = http_req {
//             tokio::spawn(send_requests(req, client.clone(), qps, request_id.clone()));
//         }
//     }
// }

pub async fn execute_request_generator(
    // mut stream: impl Stream<Item=(u32, Vec<HttpReq>)> + Unpin,
    request: RequestGenerator,
    job_id: String,
) {
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
            if let Some(status) = read_guard.get(&job_id) {
                matches!(status, JobStatus::Stopped)
            } else {
                false
            }
        };
        if stop {
            break;
        }
        send_multiple_requests(requests, client.clone(), qps, job_id.clone()).await;
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
        let request = Request::builder()
            .uri(req.url.as_str())
            .method(req.method.clone())
            .body(body.clone().into())
            .unwrap();
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
        debug!("Request completed");
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
        guard.insert(job_id, JobStatus::Stopped)
    };
    if let Some(status) = current_status {
        match status {
            JobStatus::Starting | JobStatus::InProgress => "Job stopped",
            JobStatus::Stopped => "Job already stopped",
            JobStatus::Completed => "Job already completed",
            JobStatus::Failed => "Job failed",
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
        let e = "ERROR".to_string();
        if let HttpRequestState::INIT = self.state {
            METRIC_QPS_COUNTER
                .with_label_values(&[self.job_id.as_str()])
                .inc_by(1);
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
                    METRIC_RESPONSE_TIME
                        .with_label_values(&[
                            self.job_id.as_str(),
                            self.status.as_ref().unwrap_or(&e),
                        ])
                        .observe(elapsed);
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
                            "HttpRequestFuture [{}] - request - Ready - Ok, elapsed={}",
                            &self.job_id,
                            &elapsed
                        );
                        METRIC_HEADER_TIME
                            .with_label_values(&[
                                self.job_id.as_str(),
                                self.status.as_ref().unwrap_or(&e).as_str(),
                            ])
                            .observe(elapsed);
                        let bytes = hyper::body::to_bytes(response.into_body());
                        self.body = Some(bytes.boxed());
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                    Err(err) => {
                        trace!(
                            "HttpRequestFuture [{}] - request - Ready - ERROR",
                            &self.job_id
                        );
                        METRIC_HEADER_TIME
                            .with_label_values(&[self.job_id.as_str(), err.to_string().as_str()])
                            .observe(self.timer.unwrap().elapsed().as_millis() as f64);
                        Poll::Ready(())
                    }
                }
            }
        }
    }
}

// #[derive(Debug, Serialize, Deserialize)]
// pub struct OverloadRequest<R: ReqSpec + Iterator, Q: QPSSpec + Iterator> {
//     #[serde(
//     default = "default_request_id",
//     deserialize_with = "deserialize_request_id"
//     )]
//     request_id: String,
//     pub req_spec: R,
//     pub qps_spec: Q,
//     pub duration: i32,
// }

// #[allow(dead_code)]
// impl<R: ReqSpec + Iterator, Q: QPSSpec + Iterator> OverloadRequest<R, Q> {
//     pub fn get_req_spec(&self) -> &R {
//         &self.req_spec
//     }
//
//     pub fn get_req_spec_mut(&mut self) -> &mut R {
//         &mut self.req_spec
//     }
//
//     pub fn get_qps_spec(&self) -> &Q {
//         &self.qps_spec
//     }
//
//     pub fn duration(&self) -> &i32 {
//         &self.duration
//     }
//
//     pub fn get_request_id(&self) -> &String {
//         &self.request_id
//     }
// }

impl Into<Method> for ReqMethod {
    fn into(self) -> Method {
        match self {
            ReqMethod::POST => Method::POST,
            _ => Method::GET,
        }
    }
}

#[cfg(test)]
mod test {
    #![allow(unused_imports)]

    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures::Stream;
    use tokio::time::timeout;

    // use futures_util::core_reexport::time::Duration;
    use crate::executor::execute_request_generator;
    use crate::generator::{request_generator_stream, ConstantQPS, RequestGenerator};

    #[tokio::test]
    async fn test_execute_gen_req() {
        let generator = RequestGenerator::new(3, Vec::new(), Box::new(ConstantQPS { qps: 3 }));
        let stream = request_generator_stream(generator);
        tokio::pin!(stream);
        // execute_request_generator_stream(stream);
    }
}
