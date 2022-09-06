#![allow(clippy::upper_case_acronyms)]

#[cfg(feature = "cluster")]
pub mod cluster;
pub mod connection;

use super::HttpReq;
use crate::executor::connection::QueuePool;
use crate::executor::connection::{HttpConnection, HttpConnectionPool};
use crate::generator::{request_generator_stream, RequestGenerator};
use crate::metrics::Metrics;
use crate::{ErrorCode, JobStatus};
use bb8::Pool;
use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::FutureExt;
use http::header::HeaderName;
use http::{HeaderMap, HeaderValue, StatusCode};
use hyper::client::conn::ResponseFuture;
use hyper::{Error, Request};
use lazy_static::lazy_static;
use log::{debug, warn};
use std::cmp::min;
use std::collections::{BTreeMap, HashMap};
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

type ConnectionCount = u32;
type QPS = u32;

const CYCLE_LENGTH_IN_MILLIS:i128 = 1_000; //1 seconds


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
                Poll::Ready(_) => {
                    let elapsed = self.timer.unwrap().elapsed().as_millis() as f64;
                    trace!(
                        "HttpRequestFuture [{}] - body - Ready, elapsed={}",
                        &self.job_id,
                        &elapsed
                    );
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

    let mut queue_pool = get_queue_pool(host_port.clone(), &job_id).await;

    {
        let mut write_guard = JOB_STATUS.write().await;
        write_guard.insert(job_id.clone(), JobStatus::InProgress);
    }

    let mut prev_connection_count = 0;

    let mut time_offset:i128=0;
    while let Some((qps, requests, connection_count)) = stream.next().await {
        let start_of_cycle = Instant::now();
        let stop = should_stop(&job_id).await;
        if stop {
            delete_pool(&job_id).await;
            break;
        }
        if prev_connection_count != connection_count {
            // customizer.update(connection_count);
            queue_pool.set_connection_count(connection_count as usize);
            prev_connection_count = connection_count;
        }

        // let state = pool.state();
        // metrics.pool_state((state.connections, state.idle_connections));
        let mut corrected_with_offset = start_of_cycle;
        if time_offset > 0 {
            //the previous cycle took longer than cycle duration
            // use less time for next one
            corrected_with_offset=start_of_cycle-Duration::from_millis(time_offset as u64);
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
        debug!("time offset: {}", time_offset);
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

/*
#[allow(dead_code)]
async fn send_requests(
    req: HttpReq,
    pool: Arc<Pool<HttpConnectionPool>>,
    count: i32,
    job_id: String,
    metrics: Arc<Metrics>,
    conn_return_pool: Arc<RwLock<Vec<HttpConnection>>>,
    mut connections: Vec<HttpConnection>,
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
        let _host_header_added = false;
        for (k, v) in req.headers.iter() {
            try_add_header(headers, k, v);
            // if let Some(header_name) =  try_add_header(headers, k, v) {
            //     if header_name == http::header::HOST {
            //         host_header_added = true;
            //     }
            // }
        }
        let request = request.body(body.clone().into()).unwrap();
        trace!("sending request: {:?}", &request.uri());
        match connections.pop() {
            Some(mut con) => {
                let request = con.request_handle.send_request(request);
                request_futures.push(HttpRequestFuture {
                    state: HttpRequestState::INIT,
                    timer: None,
                    job_id: job_id.clone(),
                    request: Pin::new(Box::new(request)),
                    body: None,
                    status: None,
                    metrics: &metrics,
                    connection: ReturnableConnection::Connection(con),
                });
            }
            None => {
                warn!("error while getting connection from pool");
                continue;
            }
        }
    }
    while let Some(conn) = request_futures.next().await {
        match conn {
            ReturnableConnection::Connection(connection) => {
                // conn_return_pool.write().await.push(connection);
                QueuePool::return_connection(&conn_return_pool, connection).await;
            }
            ReturnableConnection::PlaceHolder => {}
        }
    }
}
 */

async fn send_single_requests(
    req: HttpReq,
    job_id: String,
    metrics: &Arc<Metrics>,
    mut connection: HttpConnection,
) -> HttpConnection {
    let body = if let Some(body) = req.body {
        Bytes::from(body)
    } else {
        Bytes::new()
    };
    //todo remove unwrap
    let mut request = Request::builder()
        .uri(req.url.as_str())
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
        min(connection_count, REQUEST_BUNDLE_SIZE)
    };
    let mut sleep_time = interval_between_requests(count) * bundle_size as f32;
    //reserve 10% of the time for the internal logic
    sleep_time = (sleep_time-(sleep_time*0.1)).floor();

    let mut req_iterator = requests.iter().cycle();
    loop {
        let remaining_t = time_remaining();
        if remaining_t < 0 {
            debug!("stopping the cycle. remaining time: {}, remaining request: {}", remaining_t, total_remaining_qps);
            break; // give up
        }
        let requested_connection = min(bundle_size, total_remaining_qps);
        let mut connections = queue_pool.get_connections(requested_connection).await;
        let available_connection = connections.len();
        debug!("connection requested:{}, available connection: {}, request remaining: {}, remaining duration: {}, sleep duration: {}",
            requested_connection, available_connection, total_remaining_qps, remaining_t, sleep_time);
        if available_connection < 1 {
            sleep(Duration::from_millis(10)).await; //retry every 10 ms
                                                    //adjust sleep time
            sleep_time = (interval_between_requests(count) * bundle_size as f32).floor();
            debug!("resetting sleep time: after: {}", sleep_time);
            continue;
        }

        {
            let mut requests_to_send = Vec::with_capacity(available_connection);
            for _ in 0..available_connection {
                requests_to_send.push(req_iterator.next().cloned().unwrap());
            }
            let id = job_id.clone();
            let m = metrics.clone();
            let rp = return_pool.clone();
            tokio::spawn(async move {
                let mut requests: FuturesUnordered<_> = connections
                    .drain(..)
                    .map(|connection| {
                        send_single_requests(
                            requests_to_send.pop().unwrap(),
                            id.clone(),
                            &m,
                            connection,
                        )
                    })
                    .collect();
                while let Some(connection) = requests.next().await {
                    QueuePool::return_connection(&rp, connection).await;
                }
            });
        }
        total_remaining_qps -= available_connection as u32;
        if time_remaining() < 0 {
            debug!("stopping the cycle after sending req. remaining time: {}, remaining request: {}", remaining_t, total_remaining_qps);
            break; // give up
        }
        if total_remaining_qps == 0 {
            debug!("sent all the request of the cycle");
            break; //done
        }
        sleep(Duration::from_millis(sleep_time as u64)).await;
    }

    /*
    // handle reminder first so that we can drain the requests later
    if remainder > 0 {
        //todo 0 is getting preference. Choosing random index instead of 0 should be better
        if let Some(req) = requests.get(0) {
            let mut to_be_sent = remainder;
            loop {
                let conn_count = if connection_count == 0 {
                    to_be_sent
                } else {
                    min(to_be_sent, connection_count)
                };
                let connections = queue_pool.get_connections(conn_count).await;
                let request_sent = connections.len() as u32;
                if time_remaining() < 0 {
                    // give up
                    return;
                }
                if request_sent == 0 {
                    sleep(Duration::from_millis(10)).await; //retry every 10 ms
                    continue;
                }
                tokio::spawn(send_requests(
                    req.clone(),
                    pool.clone(),
                    request_sent as i32,
                    job_id.clone(),
                    metrics.clone(),
                    return_pool.clone(),
                    connections,
                ));
                // total_remaining_qps -= request_sent;
                to_be_sent -= request_sent;
                // let between_requests = interval_between_requests(total_remaining_qps);
                // let sleep_for = (between_requests * request_sent as f32).floor() as u64;
                // Sep 04 02:39:40.932 TRACE overload::executor: sleeping for 18446744073709551615 ms after sending 3 request
                // trace!(
                //     "sleeping for {} ms after sending {} request from reminder, between requests: {}",
                //     sleep_for,
                //     request_sent, between_requests
                // );
                // sleep(Duration::from_millis(sleep_for)).await;
                if to_be_sent < 1 {
                    break;
                }
            }
        }
    }
    // let mut first=true;
    // let request_interval = interval_between_requests(count);
    // let iteration_required = count.div_ceil(connection_count);
    // let sleep_between_iteration = (request_interval*iteration_required as f32).floor() as u64;
    let mut requests = requests;
    if qps_per_req > 0 {
        let drain = requests.drain(..);
        for req in drain {
            let mut to_be_sent = qps_per_req;
            loop {
                let conn_count = if connection_count == 0 {
                    to_be_sent
                } else {
                    min(to_be_sent, connection_count)
                };
                let connections = queue_pool
                    .get_connections(min(qps_per_req, conn_count))
                    .await;
                let request_sent = connections.len() as u32;
                if time_remaining() < 0 {
                    return;
                }
                // let between_requests = interval_between_requests(count);
                if request_sent == 0 {
                    //sleep for 10ms or 10 requests duration and then continue
                    // sleep(Duration::from_millis(min(
                    //     10,
                    //     (between_requests * 10.0).floor() as u64,
                    // )))
                    sleep(Duration::from_millis(10)).await; //retry every 10 ms
                                                            //todo reset request interval due to time wasted in waiting for connection
                    continue;
                }
                tokio::spawn(send_requests(
                    req.clone(),
                    pool.clone(),
                    request_sent as i32,
                    job_id.clone(),
                    metrics.clone(),
                    return_pool.clone(),
                    connections,
                ));
                // trace!(
                //     "sleeping for {} ms, remaining requests {}, between requests: {}",
                //     sleep_between_iteration,
                //     total_remaining_qps,
                //     between_requests
                // );
                // sleep(Duration::from_millis(sleep_for)).await;
                // sleep(Duration::from_millis(sleep_between_iteration)).await;
                // total_remaining_qps -= request_sent;
                to_be_sent -= request_sent;
                if to_be_sent < 1 {
                    break;
                }
            }
        }
    }
    */
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

async fn get_queue_pool(host_port: String, _job_id: &str) -> QueuePool {
    QueuePool::new(host_port)
}

async fn delete_pool(job_id: &str) {
    let mut write_guard = CONNECTION_POOLS.write().await;
    write_guard.remove(job_id);
}
