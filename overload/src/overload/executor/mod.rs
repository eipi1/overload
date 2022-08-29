#![allow(clippy::upper_case_acronyms)]

#[cfg(feature = "cluster")]
pub mod cluster;
pub mod connection;

use super::HttpReq;
use crate::executor::connection::ConcurrentConnectionCountManager;
use crate::executor::connection::HttpConnectionPool;
use crate::generator::{request_generator_stream, RequestGenerator};
use crate::metrics::Metrics;
use crate::{ErrorCode, JobStatus};
use bb8::Pool;
use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::FutureExt;
use http::header::HeaderName;
use http::{HeaderValue, StatusCode};
use hyper::client::conn::ResponseFuture;
use hyper::{Error, Request};
use lazy_static::lazy_static;
use log::{debug, warn};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::option::Option::Some;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_stream::StreamExt;
use tower::ServiceExt;
use tracing::{error, info, trace};

#[allow(dead_code)]
enum HttpRequestState {
    INIT,
    InProgress,
    ResponseReceived,
    DONE,
}

//todo status fields seems unnecessary
#[must_use = "futures do nothing unless polled"]
struct HttpRequestFuture<'a> {
    state: HttpRequestState,
    timer: Option<Instant>,
    job_id: String,
    request: Pin<Box<ResponseFuture>>,
    body: Option<BoxFuture<'a, Result<Bytes, Error>>>,
    status: Option<StatusCode>,
    metrics: &'a Metrics,
}

impl Future for HttpRequestFuture<'_> {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _e = "ERROR".to_string();
        if let HttpRequestState::INIT = self.state {
            self.metrics.upstream_request_count(1);
            self.timer = Some(Instant::now());
            self.state = HttpRequestState::InProgress;
            trace!("HttpRequestFuture [{}] - Begin", &self.job_id);
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
                    //add metrics here if it's necessary to include time to fetch body
                    Poll::Ready(())
                }
                Poll::Pending => {
                    trace!("HttpRequestFuture [{}] - body - Pending", &self.job_id);
                    Poll::Pending
                }
            };
        } else if matches!(self.state, HttpRequestState::ResponseReceived) {
            trace!(
                "HttpRequestFuture [{}] - body: None - completing",
                &self.job_id
            );
            return Poll::Ready(());
        }

        match Pin::new(&mut self.request).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(val) => {
                self.state = HttpRequestState::ResponseReceived;
                match val {
                    Ok(response) => {
                        let status = response.status();
                        self.status = Some(status);
                        let elapsed = self.timer.unwrap().elapsed().as_millis() as f64;
                        trace!(
                            "HttpRequestFuture [{}] - status: {:?}, elapsed: {}",
                            &self.job_id,
                            &self.status,
                            &elapsed
                        );
                        let status_str = status.as_str();
                        self.metrics.upstream_response_time(status_str, elapsed);
                        self.metrics.upstream_request_status_count(1, status_str);
                        let bytes = hyper::body::to_bytes(response.into_body());
                        self.body = Some(bytes.boxed());
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                    Err(err) => {
                        trace!("HttpRequestFuture [{}] - ERROR: {}", &self.job_id, &err);
                        self.metrics
                            .upstream_request_status_count(1, err.to_string().as_str());
                        Poll::Ready(())
                    }
                }
            }
        }
    }
}

lazy_static! {
    pub static ref JOB_STATUS: RwLock<BTreeMap<String, JobStatus>> = RwLock::new(BTreeMap::new());
    //in cluster mode, secondary nodes receive workloads in small requests and new pools are created everytime.
    //That'll lead to inconsistent number of connections. To avoid that, use global pool collection as hack.
    //Need to find a better way to divide workloads to secondaries, maybe using websocket?
    pub(crate) static ref CONNECTION_POOLS: RwLock<HashMap<String, Arc<Pool<HttpConnectionPool>>>> = RwLock::new(HashMap::new());
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

        //remove unused pool
        {
            let mut write_guard = CONNECTION_POOLS.write().await;
            // for pool in read_guard.iter() {

            // }
            write_guard.retain(|_, pool| {
                let state = pool.state();
                let remove = state.connections == state.idle_connections;
                trace!("removing pool: {}", remove);
                remove
            })
        }
    }
}

pub async fn execute_request_generator(
    request: RequestGenerator,
    job_id: String,
    metrics: Arc<Metrics>,
    init: BoxFuture<'_, Result<(), anyhow::Error>>,
) {
    if let Err(e) = init.await {
        error!("Error during preparation: {:?}", e);
        return;
    }
    let target = request.target.clone();
    let stream = request_generator_stream(request);
    tokio::pin!(stream);

    let host_port = format!("{}:{}", target.host, target.port);

    let pool = get_pool(&host_port, &job_id).await;

    {
        let mut write_guard = JOB_STATUS.write().await;
        write_guard.insert(job_id.clone(), JobStatus::InProgress);
    }

    let customizer = pool.get_pool_customizer().unwrap();

    let mut prev_connection_count = 1;

    while let Some((qps, requests, connection_count)) = stream.next().await {
        let stop = should_stop(&job_id).await;
        if stop {
            delete_pool(&job_id).await;
            break;
        }
        if prev_connection_count != connection_count {
            customizer.update(connection_count);
            prev_connection_count = connection_count;
        }
        let state = pool.state();
        metrics.pool_state((state.connections, state.idle_connections));

        match requests {
            Ok(result) => {
                send_multiple_requests(result, pool.clone(), qps, job_id.clone(), metrics.clone())
                    .await;
            }
            Err(err) => {
                if let Some(sqlx::Error::Database(_)) = err.downcast_ref::<sqlx::Error>() {
                    error!(
                        "[{}] sqlx::Error::Database: {}. Unrecoverable error; stopping the job.",
                        &job_id,
                        &err.to_string()
                    );
                    set_job_status(&job_id, JobStatus::Error(ErrorCode::SqliteOpenFailed)).await;
                    break;
                }
                error!("[{}] - Error: {}", &job_id, &err.to_string());
            }
        };
    }
    delete_pool(&job_id).await;
    set_job_status(&job_id, JobStatus::Completed).await;
}

async fn should_stop(job_id: &str) -> bool {
    let read_guard = JOB_STATUS.read().await;
    matches!(
        read_guard.get(job_id),
        Some(JobStatus::Stopped) | Some(JobStatus::Error(_))
    )
}

async fn set_job_status(job_id: &str, new_status: JobStatus) {
    let mut write_guard = JOB_STATUS.write().await;
    if let Some(status) = write_guard.get(job_id) {
        let job_finished = matches!(
            status,
            JobStatus::Stopped | JobStatus::Failed | JobStatus::Completed | JobStatus::Error(_)
        );
        if !job_finished {
            write_guard.insert(job_id.to_string(), new_status);
        }
    } else {
        error!("Job Status should be present")
    }
}

async fn send_requests(
    req: HttpReq,
    pool: Arc<Pool<HttpConnectionPool>>,
    count: i32,
    job_id: String,
    metrics: Arc<Metrics>,
) {
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
        trace!("sending request: {:?}", &request.uri());
        match pool.get().await {
            Ok(mut con) => {
                // let mut handle = con.request_handle;
                trace!("readying connection");
                let handle = con.request_handle.ready().await;
                if handle.is_err() {
                    warn!("error - connection not ready for {:?}", &request.uri());
                    continue;
                }
                trace!("connection ready");
                let request = handle.unwrap().send_request(request);
                request_futures.push(HttpRequestFuture {
                    state: HttpRequestState::INIT,
                    timer: None,
                    job_id: job_id.clone(),
                    request: Pin::new(Box::new(request)),
                    body: None,
                    status: None,
                    metrics: &metrics,
                });
            }
            Err(err) => {
                warn!("error while getting connection from pool: {:?}", err);
                continue;
            }
        }
    }
    while let Some(_resp) = request_futures.next().await {}
}

async fn send_multiple_requests(
    requests: Vec<HttpReq>,
    pool: Arc<Pool<HttpConnectionPool>>,
    count: u32,
    job_id: String,
    metrics: Arc<Metrics>,
) {
    debug!(
        "[{}] [send_multiple_requests] - size: {}, count: {}",
        &job_id,
        &requests.len(),
        &count,
    );
    let n_req = requests.len();
    if count == 0 || n_req == 0 {
        trace!(
            "no requests are sent, either qps({}) or request size({}) is zero",
            count,
            n_req
        );
        return;
    }
    let qps_per_req = count / n_req as u32;
    let remainder = count % n_req as u32;

    if remainder > 0 {
        if let Some(req) = requests.get(0) {
            //0 is getting preference. Choosing random index instead of 0 should be better
            tokio::spawn(send_requests(
                req.clone(),
                pool.clone(),
                remainder as i32,
                job_id.clone(),
                metrics.clone(),
            ));
        }
    }
    let mut requests = requests;
    if qps_per_req > 0 {
        let drain = requests.drain(..);
        for req in drain {
            tokio::spawn(send_requests(
                req,
                pool.clone(),
                qps_per_req as i32,
                job_id.clone(),
                metrics.clone(),
            ));
        }
    }
}

pub(crate) async fn send_stop_signal(job_id: &str) -> String {
    info!("Stopping the job {}", job_id);
    //todo don't stop if already done
    let current_status = {
        let mut guard = JOB_STATUS.write().await;
        guard.insert(job_id.to_string(), JobStatus::Stopped)
    };
    if let Some(status) = current_status {
        match status {
            JobStatus::Starting | JobStatus::InProgress => "Job stopped",
            JobStatus::Stopped => "Job already stopped",
            JobStatus::Completed => "Job already completed",
            JobStatus::Failed => "Job failed",
            _ => {
                error!("Invalid job status for job: {}", job_id);
                "Invalid job status"
            }
        }
        .to_string()
    } else {
        "Job Not Found".to_string()
    }
}

/// return status of test jobs.
/// job_id has higher priority and will return status of the the job. Otherwise will
/// return status of `limit` jobs, starting at `offset`
pub(crate) async fn get_job_status(
    job_id: Option<String>,
    offset: usize,
    limit: usize,
) -> HashMap<String, JobStatus> {
    let mut job_status = HashMap::new();
    let guard = JOB_STATUS.read().await;

    if let Some(job_id) = job_id {
        if let Some(status) = guard.get(&job_id) {
            job_status.insert(job_id, *status);
        }
        return job_status;
    }

    let mut count = 0;
    let len = guard.len();
    for (id, status) in guard.iter() {
        if count > len {
            break;
        }
        count += 1;
        if count > offset && count - offset > limit {
            break;
        }
        if count > offset {
            job_status.insert(id.clone(), *status);
        }
    }
    job_status
}

/// Get from global pool collection or create a new pool and put into the collection
async fn get_pool(host_port: &str, job_id: &str) -> Arc<Pool<HttpConnectionPool>> {
    let pool = {
        let read_guard = CONNECTION_POOLS.read().await;
        read_guard.get(host_port).cloned()
    };
    if pool.is_none() {
        let pool = HttpConnectionPool::new(host_port);
        let customizer = Arc::new(ConcurrentConnectionCountManager::new(1));
        let pool = bb8::Pool::builder()
            .pool_customizer(customizer.clone())
            .reaper_rate(Duration::from_millis(1000))
            .idle_timeout(Some(Duration::from_millis(1000)))
            .build(pool)
            .await
            .unwrap();
        let pool = Arc::new(pool);
        let mut write_guard = CONNECTION_POOLS.write().await;
        write_guard.insert(job_id.to_string(), pool.clone());
        pool
    } else {
        pool.unwrap()
    }
}

async fn delete_pool(job_id: &str) {
    let mut write_guard = CONNECTION_POOLS.write().await;
    write_guard.remove(job_id);
}
