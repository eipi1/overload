#![allow(clippy::upper_case_acronyms)]

#[cfg(feature = "cluster")]
pub mod cluster;
pub mod connection;

use super::HttpReq;
use crate::executor::connection::HttpConnection;
use crate::executor::connection::QueuePool;
use crate::generator::{request_generator_stream, RequestGenerator};
use crate::metrics::Metrics;
use crate::{ErrorCode, JobStatus, METRICS_FACTORY};
use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::FutureExt;
use http::header::HeaderName;
use http::{HeaderMap, HeaderValue, StatusCode, Uri};
use hyper::client::conn::ResponseFuture;
use hyper::{Error, Request};
use lazy_static::lazy_static;
use log::{debug, warn};
use std::cmp::min;
use std::collections::{BTreeMap, HashMap};
use std::convert::TryFrom;
use std::future::Future;
use std::option::Option::Some;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tokio_stream::StreamExt;
use tracing::{error, info, trace};

use response_assert::ResponseAssertion;
#[cfg(feature = "cluster")]
use tokio::sync::oneshot::{channel, Receiver};
#[cfg(feature = "cluster")]
use tokio::time::timeout;

type ConnectionCount = u32;
type QPS = u32;

const CYCLE_LENGTH_IN_MILLIS: i128 = 1_000; //1 seconds

#[allow(dead_code)]
enum HttpRequestState {
    INIT,
    InProgress,
    ResponseReceived,
    DONE,
}

enum ReturnableConnection {
    PlaceHolder,
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
    connection: ReturnableConnection,
    assertion: &'a ResponseAssertion,
    request_uri: Uri,
}

impl Future for HttpRequestFuture<'_> {
    type Output = ReturnableConnection;
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
                Poll::Ready(body) => {
                    let elapsed = self.timer.unwrap().elapsed().as_millis() as f64;
                    trace!(
                        "HttpRequestFuture [{}] - body - Ready, elapsed={}",
                        &self.job_id,
                        &elapsed
                    );
                    if let Ok(body) = body {
                        if !self.assertion.is_empty() {
                            let _ = serde_json::from_slice(body.as_ref())
                                .map_err(|e| {
                                    self.metrics.assertion_parse_failure(&e.to_string());
                                    vec![]
                                })
                                .and_then(|json_resp| {
                                    response_assert::assert(
                                        self.assertion,
                                        &self.request_uri,
                                        None,
                                        &json_resp,
                                    )
                                })
                                .map_err(|errors| {
                                    for err in errors {
                                        info!("assertion error - {:?}", &err);
                                        self.metrics.assertion_failure(err.get_id(), err.into());
                                    }
                                });
                        }
                    };
                    //add metrics here if it's necessary to include time to fetch body
                    Poll::Ready(std::mem::replace(
                        &mut self.connection,
                        ReturnableConnection::PlaceHolder,
                    ))
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
            return Poll::Ready(std::mem::replace(
                &mut self.connection,
                ReturnableConnection::PlaceHolder,
            ));
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
                        Poll::Ready(std::mem::replace(
                            &mut self.connection,
                            ReturnableConnection::PlaceHolder,
                        ))
                    }
                }
            }
        }
    }
}

lazy_static! {
    pub static ref JOB_STATUS: RwLock<BTreeMap<String, JobStatus>> = RwLock::new(BTreeMap::new());
}

#[cfg(feature = "cluster")]
lazy_static! {
    //in cluster mode, secondary nodes receive workloads in small requests and new pools are created everytime.
    //That'll lead to inconsistent number of connections. To avoid that, use global pool collection as hack.
    //Need to find a better way to divide workloads to secondaries, maybe using websocket?
    pub(crate) static ref CONNECTION_POOLS: RwLock<HashMap<String, QueuePool>> = RwLock::new(HashMap::new());
    pub(crate) static ref CONNECTION_POOLS_USAGE_LISTENER: RwLock<HashMap<String, Receiver<()>>> = RwLock::new(HashMap::new());
}

pub async fn init() {
    #[cfg(feature = "cluster")]
    tokio::spawn(async {
        //If the pool hasn't been used for 30 second, drop it
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            interval.tick().await;
            let removable = {
                CONNECTION_POOLS
                    .read()
                    .await
                    .iter()
                    .filter(|v| v.1.last_use.elapsed() > Duration::from_secs(30))
                    .map(|v| v.0.clone())
                    .collect::<Vec<String>>()
            };
            for job_id in removable {
                CONNECTION_POOLS.write().await.remove(&job_id);
                METRICS_FACTORY.remove_metrics(&job_id).await;
                CONNECTION_POOLS_USAGE_LISTENER
                    .write()
                    .await
                    .remove(&job_id);
            }
        }
    });

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
    let response_assertion = Arc::new(request.response_assertion.clone().unwrap_or_default());
    let stream = request_generator_stream(request);
    tokio::pin!(stream);

    let host_port = format!("{}:{}", target.host, target.port);

    #[cfg(not(feature = "cluster"))]
    let mut queue_pool = get_new_queue_pool(host_port.clone()).await;
    #[cfg(feature = "cluster")]
    let mut queue_pool = {
        //there's a possibility that pool already exists for this job,
        // but didn't finish the previous batch and pool hasn't returned to CONNECTION_POOLS
        // so we need to try a few times

        let pool_usage_notification_rx = {
            CONNECTION_POOLS_USAGE_LISTENER
                .write()
                .await
                .remove(&job_id)
        };
        if let Some(rx) = pool_usage_notification_rx {
            let mut rx = rx;
            let result = timeout(Duration::from_millis(9_700), async {
                loop {
                    // this loop isn't necessary. added to breakdown time only for debugging purpose
                    if let Ok(notification) = timeout(Duration::from_millis(100), &mut rx).await {
                        match notification {
                            Ok(_) => break Ok(()),
                            Err(e) => {
                                error!("Error from notification receiver: {}", e);
                                break Err(());
                            }
                        }
                    } else {
                        debug!("pool not found. trying again");
                    }
                }
            })
            .await;
            if result.is_err() {
                error!("pool not found within limit, no request will be sent, returning");
                //return tx back to container
                CONNECTION_POOLS_USAGE_LISTENER
                    .write()
                    .await
                    .insert(job_id.clone(), rx);
                return;
            }
            // if received notification
            get_existing_queue_pool(&job_id).await.unwrap()
        } else {
            get_new_queue_pool(host_port.clone()).await
        }
    };

    {
        let mut write_guard = JOB_STATUS.write().await;
        write_guard.insert(job_id.clone(), JobStatus::InProgress);
    }
    #[cfg(feature = "cluster")]
    let tx = {
        let (tx, rx) = channel::<()>();
        CONNECTION_POOLS_USAGE_LISTENER
            .write()
            .await
            .insert(job_id.clone(), rx);
        tx
    };

    let mut prev_connection_count = 0;

    debug!("[{}] - Starting request generation...", &job_id);
    let mut time_offset: i128 = 0;
    while let Some((qps, requests, connection_count)) = stream.next().await {
        debug!("[{}] - starting a cycle", &job_id);
        let start_of_cycle = Instant::now();
        let stop = should_stop(&job_id).await;
        if stop {
            break;
        }
        if prev_connection_count != connection_count {
            // customizer.update(connection_count);
            queue_pool
                .set_connection_count(connection_count as usize, &metrics)
                .await;
            metrics.pool_size(connection_count as f64);
            prev_connection_count = connection_count;
        }
        queue_pool.last_use = Instant::now();

        // let state = pool.state();
        // metrics.pool_state((state.connections, state.idle_connections));
        let mut corrected_with_offset = start_of_cycle;
        if time_offset > 0 {
            //the previous cycle took longer than cycle duration
            // use less time for next one
            corrected_with_offset = start_of_cycle - Duration::from_millis(time_offset as u64);
        }

        match requests {
            Ok(result) => {
                send_multiple_requests(
                    result,
                    qps,
                    job_id.clone(),
                    metrics.clone(),
                    &mut queue_pool,
                    connection_count,
                    corrected_with_offset,
                    &response_assertion,
                )
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
        time_offset = start_of_cycle.elapsed().as_millis() as i128 - CYCLE_LENGTH_IN_MILLIS;
        debug!("[{}] - time offset in the cycle: {}", &job_id, time_offset);
    }

    set_job_status(&job_id, JobStatus::Completed).await;
    #[cfg(feature = "cluster")]
    {
        CONNECTION_POOLS
            .write()
            .await
            .insert(job_id.clone(), queue_pool);
        let _ = tx.send(());
    }
    #[cfg(not(feature = "cluster"))]
    {
        METRICS_FACTORY.remove_metrics(&job_id).await;
    }
    debug!("[{}] - finished request generation", &job_id);
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

async fn send_single_requests(
    req: HttpReq,
    job_id: String,
    metrics: &Arc<Metrics>,
    mut connection: HttpConnection,
    assertion: &ResponseAssertion,
) -> HttpConnection {
    let body = if let Some(body) = req.body {
        Bytes::from(body)
    } else {
        Bytes::new()
    };
    let request_uri = Uri::try_from(req.url).unwrap();
    //todo remove unwrap
    let mut request = Request::builder()
        .uri(request_uri.clone())
        .method(req.method.clone());
    let headers = request.headers_mut().unwrap();
    for (k, v) in req.headers.iter() {
        try_add_header(headers, k, v);
    }
    let request = request.body(body.into()).unwrap();
    trace!("sending request: {:?}", &request.uri());

    let request = connection.request_handle.send_request(request);
    let request_future = HttpRequestFuture {
        state: HttpRequestState::INIT,
        timer: None,
        job_id,
        request: Pin::new(Box::new(request)),
        body: None,
        status: None,
        metrics,
        connection: ReturnableConnection::PlaceHolder,
        assertion,
        request_uri,
    };
    request_future.await;

    //todo handle cancelled, failed, time out futures
    // match request_future.await {
    //     ReturnableConnection::Connection(connection) => {
    //         // conn_return_pool.write().await.push(connection);
    //         QueuePool::return_connection(&conn_return_pool, connection).await;
    //     }
    //     ReturnableConnection::PlaceHolder => {}
    // }
    connection
}

fn try_add_header(headers: &mut HeaderMap, k: &str, v: &str) -> Option<HeaderName> {
    let header_name = HeaderName::from_str(k).ok()?;
    let header_value = HeaderValue::from_str(v).ok()?;
    headers.insert(header_name.clone(), header_value);
    Some(header_name)
}

#[allow(clippy::too_many_arguments)]
async fn send_multiple_requests(
    requests: Vec<HttpReq>,
    count: QPS,
    job_id: String,
    metrics: Arc<Metrics>,
    queue_pool: &mut QueuePool,
    connection_count: ConnectionCount,
    start_of_cycle: Instant,
    response_assertion: &Arc<ResponseAssertion>,
) {
    debug!(
        "[{}] [send_multiple_requests] - size: {}, count: {}, connection count: {}",
        &job_id,
        &requests.len(),
        &count,
        &connection_count
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
    let return_pool = queue_pool.get_return_pool();

    // let qps_per_req = count / n_req as u32;
    // let remainder = count % n_req as u32;

    let time_remaining =
        move || CYCLE_LENGTH_IN_MILLIS - start_of_cycle.elapsed().as_millis() as i128;
    let interval_between_requests = |remaining_requests: QPS| {
        let remaining_time_in_cycle = time_remaining();
        trace!(
            "remaining time in cycle:{}, remaining requests: {}",
            remaining_time_in_cycle,
            remaining_requests
        );
        if remaining_time_in_cycle < 1 {
            warn!(
                "no time remaining for {} requests in this cycle",
                remaining_requests
            );
            return 0_f32;
        }
        remaining_time_in_cycle as f32 / remaining_requests as f32
    };
    let mut total_remaining_qps = count;
    const REQUEST_BUNDLE_SIZE: u32 = 10;
    let bundle_size = if connection_count == 0 {
        //elastic pool
        REQUEST_BUNDLE_SIZE
    } else {
        min(count, min(connection_count, REQUEST_BUNDLE_SIZE))
    };
    let mut sleep_time = interval_between_requests(count) * bundle_size as f32;
    //reserve 10% of the time for the internal logic
    sleep_time = (sleep_time - (sleep_time * 0.1)).floor();

    let mut req_iterator = requests.iter().cycle();
    loop {
        let remaining_t = time_remaining();
        if remaining_t < 0 {
            debug!(
                "stopping the cycle. remaining time: {}, remaining request: {}",
                remaining_t, total_remaining_qps
            );
            break; // give up
        }
        let requested_connection = min(bundle_size, total_remaining_qps);
        let mut connections = queue_pool
            .get_connections(requested_connection, &metrics)
            .await;
        let available_connection = connections.len();
        debug!("connection requested:{}, available connection: {}, request remaining: {}, remaining duration: {}, sleep duration: {}",
            requested_connection, available_connection, total_remaining_qps, remaining_t, sleep_time);
        if available_connection < requested_connection as usize {
            if available_connection < 1 {
                //adjust sleep time
                sleep(Duration::from_millis(10)).await; //retry every 10 ms
                sleep_time =
                    (interval_between_requests(total_remaining_qps) * bundle_size as f32).floor();
                continue;
            }
            //adjust sleep time
            sleep_time =
                (interval_between_requests(total_remaining_qps) * bundle_size as f32).floor();
            debug!("resetting sleep time: after: {}", sleep_time);
        }

        {
            let mut requests_to_send = Vec::with_capacity(available_connection);
            for _ in 0..available_connection {
                requests_to_send.push(req_iterator.next().cloned().unwrap());
            }
            let id = job_id.clone();
            let m = metrics.clone();
            let rp = return_pool.clone();
            let assertion = response_assertion.clone();
            tokio::spawn(async move {
                let mut requests: FuturesUnordered<_> = connections
                    .drain(..)
                    .map(|connection| {
                        send_single_requests(
                            requests_to_send.pop().unwrap(),
                            id.clone(),
                            &m,
                            connection,
                            &assertion,
                        )
                    })
                    .collect();
                while let Some(connection) = requests.next().await {
                    QueuePool::return_connection(&rp, connection, &m).await;
                }
            });
        }
        total_remaining_qps -= available_connection as u32;
        if time_remaining() < 0 {
            debug!(
                "stopping the cycle after sending req. remaining time: {}, remaining request: {}",
                remaining_t, total_remaining_qps
            );
            break; // give up
        }
        if total_remaining_qps == 0 {
            debug!("sent all the request of the cycle");
            break; //done
        }
        sleep(Duration::from_millis(sleep_time as u64)).await;
    }
}

pub(crate) async fn send_stop_signal(job_id: &str) -> Vec<(String, Option<JobStatus>)> {
    info!("Stopping the jobs {}", job_id);
    let mut write = JOB_STATUS.write().await;
    write
        .iter_mut()
        .filter(|key_val| key_val.0.starts_with(job_id))
        .map(|key_val| {
            let prev_status = std::mem::replace(key_val.1, JobStatus::Stopped);
            (key_val.0.clone(), Some(prev_status))
        })
        .collect::<Vec<_>>()
}

/// return status of test jobs.
/// job_id has higher priority and will return status of the the job. Otherwise will
/// return status of `limit` jobs, starting at `offset`
pub(crate) async fn get_job_status(
    job_id: Option<String>,
    offset: usize,
    limit: usize,
) -> HashMap<String, JobStatus> {
    let guard = JOB_STATUS.read().await;

    if let Some(job_id) = job_id {
        return guard
            .iter()
            .filter(|status| status.0.starts_with(&job_id))
            .map(|status| (status.0.clone(), *status.1))
            .collect();
    }

    let mut job_status = HashMap::new();
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

async fn get_new_queue_pool(host_port: String) -> QueuePool {
    debug!("Creating new pool for: {}", &host_port);
    QueuePool::new(host_port)
}

#[cfg(feature = "cluster")]
async fn get_existing_queue_pool(job_id: &str) -> Option<QueuePool> {
    CONNECTION_POOLS.write().await.remove(job_id)
}
