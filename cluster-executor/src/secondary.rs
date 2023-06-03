#![allow(unused_imports)]

use crate::connection::{ConnectionKeepAlive, HttpConnection, QueuePool};
use crate::request_providers::RequestProvider;
use crate::{
    data_dir_path, log_error, HttpRequestFuture, HttpRequestState, MessageFromPrimary, Metadata,
    OriginalRequest, RateMessage, ReturnableConnection, CYCLE_LENGTH_IN_MILLIS,
};
use anyhow::{anyhow, Error as AnyError, Result as AnyResult};
use cluster_mode::Cluster;
use futures_core::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::{FutureExt, TryStreamExt};
use http::header::HeaderName;
use http::{HeaderMap, HeaderValue, Uri};
use hyper::body::{Bytes, HttpBody};
use hyper::{Body, Client};
use lazy_static::lazy_static;
use log::{debug, error, info, trace, warn};
use lua_helper::{init_lua, load_lua_func, load_lua_func_with_registry, LuaAssertionResult};
use mlua::Value::Function;
use once_cell::sync::OnceCell;
use overload_http::{HttpReq, Request, RequestSpecEnum, Target, PATH_REQUEST_DATA_FILE_DOWNLOAD};
use overload_metrics::{Metrics, MetricsFactory, METRICS_FACTORY};
use regex::Regex;
use remoc::rch;
use remoc::rch::base::{Receiver, RecvError};
use response_assert::{LuaExecSender, ResponseAssertion};
use std::cmp::{max, min};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, io};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::mpsc::{UnboundedReceiver as TkMpscReceiver, UnboundedSender as TkMpscSender};
use tokio::sync::oneshot::Sender;
use tokio::sync::oneshot::{Receiver as TkOneShotReceiver, Sender as TkOneShotSender};
use tokio::sync::RwLock;
use tokio::time::{sleep, timeout};
use tokio_stream::StreamExt;
use url::Url;
use uuid::Uuid;

pub const ENV_NAME_BUNDLE_SIZE: &str = "REQUEST_BUNDLE_SIZE";
pub static REQUEST_BUNDLE_SIZE: OnceCell<u16> = OnceCell::new();
pub static DEFAULT_REQUEST_BUNDLE_SIZE: u16 = 50;

pub fn request_bundle_size() -> u16 {
    *REQUEST_BUNDLE_SIZE.get_or_init(|| {
        env::var(ENV_NAME_BUNDLE_SIZE)
            .map_err(|_| ())
            .and_then(|port| u16::from_str(&port).map_err(|_| ()))
            .unwrap_or(DEFAULT_REQUEST_BUNDLE_SIZE)
    })
}

/// Listen to request from primary
pub async fn primary_listener(port: u16, metrics_factory: &'static MetricsFactory) {
    info!("Starting remoc server at: {}", &port);
    // Listen for incoming TCP connection.
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
        .await
        .unwrap_or_else(|_| panic!("Failed to bind port: {}", &port));
    info!("Started remoc server at: {}", &port);
    loop {
        let (socket, address) = listener.accept().await.unwrap();
        info!(
            "[primary_listener] - accepted connection from: {:?}",
            &address
        );
        let (socket_rx, socket_tx) = socket.into_split();

        let result: Result<(_, rch::base::Sender<()>, _), _> =
            remoc::Connect::io(remoc::Cfg::default(), socket_rx, socket_tx).await;
        match result {
            Ok((conn, _tx, rx)) => {
                tokio::spawn(conn);
                tokio::spawn(async {
                    let result = handle_connection_from_primary(rx, metrics_factory).await;
                    log_error!(result);
                });
            }
            Err(e) => {
                error!(
                    "[primary_listener] - remoc connection error: {}, address: {:?},",
                    e, address
                );
            }
        };
    }
}

async fn handle_connection_from_primary(
    rx: Receiver<MessageFromPrimary>,
    metrics_factory: &MetricsFactory,
) -> AnyResult<()> {
    let mut rx = rx;

    //the first message secondary receive from primary should be metadata message
    let metadata = check_for_metadata_msg(&mut rx).await?;
    info!("[handle_connection_from_primary] - {:?}", &metadata);

    //the second message secondary receive from primary should be request message
    let mut request = check_for_request_msg(&mut rx).await?;
    info!("[handle_connection_from_primary] - {:?}", &request);

    let job_id = job_id(&request.name);
    let buckets = request.histogram_buckets.clone();
    let metrics = metrics_factory
        .metrics_with_buckets(buckets.to_vec(), &job_id)
        .await;
    let init = prepare(&mut request, metadata.primary_host.clone());
    init.await?;

    let (mut queue_pool, tx) = init_connection_pool(
        &request.target,
        job_id.clone(),
        request.connection_keep_alive,
    )
    .await
    .ok_or_else(|| anyhow!("Unable to create connection pool"))?;

    let mut request_spec = request.req;

    let mut stop = false;
    let mut finish = false;
    let mut error_exit = false;
    let mut reset = false;

    let mut prev_connection_count = 0;
    let mut time_offset: i128 = 0;
    let response_assertion = Arc::new(request.response_assertion.unwrap_or_default());
    let lua_executor_sender = response_assert::init_lua_executor(&response_assertion).await;
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        if msg.is_err() {
            error!(
                "[handle_connection_from_primary] - [{}] - there should a message every second, \
                but nothing received for two seconds",
                &job_id
            );
            continue;
        }
        let msg = msg.unwrap();
        match msg {
            Err(err) => {
                error!(
                    "[handle_connection_from_primary] - [{}] - receiver error: {:?}",
                    &job_id, err
                );
                error_exit = true;
                break;
            }
            Ok(msg) => {
                match msg {
                    None => {
                        debug!("[handle_connection_from_primary] - None received, exiting loop");
                        break;
                    }
                    Some(msg) => match msg {
                        MessageFromPrimary::Metadata(_) => {
                            error!("[handle_connection_from_primary] - [{}] - unexpected MessageFromPrimary::Request", &job_id);
                        }
                        MessageFromPrimary::Rates(rate) => {
                            trace!("[handle_connection_from_primary] - {:?}", &rate);
                            (prev_connection_count, time_offset) = handle_rate_msg(
                                rate,
                                prev_connection_count,
                                &metrics,
                                time_offset,
                                job_id.clone(),
                                &mut queue_pool,
                                &mut request_spec,
                                response_assertion.clone(),
                                lua_executor_sender.clone(),
                            )
                            .await;
                        }
                        MessageFromPrimary::Stop => {
                            stop = true;
                            break;
                        }
                        MessageFromPrimary::Finished => {
                            finish = true;
                            stop = true;
                            break;
                        }
                        MessageFromPrimary::Reset => {
                            info!(
                                "[handle_connection_from_primary] - [{}] - reset received",
                                &job_id
                            );
                            reset = true;
                        }
                        MessageFromPrimary::Request(mut req) => {
                            if !reset {
                                error!("[handle_connection_from_primary] - [{}] - unexpected message - {:?}", &job_id, req);
                            }
                            info!("[handle_connection_from_primary] - [{}] - reset - new request - {:?}", &job_id, &req);
                            prepare(&mut req, metadata.primary_host.clone()).await?;
                            request_spec = req.req;
                            reset = false;
                        }
                    },
                }
            }
        }
    }
    // #[cfg(feature = "cluster")]
    {
        if error_exit {
            CONNECTION_POOLS
                .write()
                .await
                .insert(job_id.clone(), queue_pool);
            let _ = tx.send(());
        } else if stop || finish {
            CONNECTION_POOLS.write().await.remove(&job_id);
            CONNECTION_POOLS_USAGE_LISTENER
                .write()
                .await
                .remove(&job_id);
            METRICS_FACTORY.remove_metrics(&job_id).await;
        }
    }
    // #[cfg(not(feature = "cluster"))]
    // {
    //     METRICS_FACTORY.remove_metrics(&job_id).await;
    // }
    debug!("[handle_connection_from_primary] - [{}] - exiting with status stop:{}, finish:{}, error_exit:{}", &job_id,
    stop, finish, error_exit);
    Ok(())
}

// #[cfg(feature = "cluster")]
lazy_static! {
    //in cluster mode, connection between primary & secondary may break and should avoid creating
    // new pool when reconnect. Otherwise it'll lead to inconsistent number of connections.
    // To avoid that, use global pool collection.
    pub(crate) static ref CONNECTION_POOLS: RwLock<HashMap<String, QueuePool>> = RwLock::new(HashMap::new());
    pub(crate) static ref CONNECTION_POOLS_USAGE_LISTENER: RwLock<HashMap<String, tokio::sync::oneshot::Receiver<()>>> = RwLock::new(HashMap::new());
}

async fn init_connection_pool(
    target: &Target,
    job_id: String,
    keep_alive: ConnectionKeepAlive,
) -> Option<(QueuePool, Sender<()>)> {
    let host_port = format!("{}:{}", &target.host, &target.port);

    // #[cfg(not(feature = "cluster"))]
    // let mut queue_pool = get_new_queue_pool(host_port.clone()).await;
    // #[cfg(feature = "cluster")]
    let queue_pool = {
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
            // 9_700 => almost 10 seconds. Why chose 10? - because primary retries after 10 secs
            let result = timeout(Duration::from_millis(9_700), async {
                loop {
                    // this loop isn't necessary. added to breakdown time only for debugging purpose
                    if let Ok(notification) = timeout(Duration::from_millis(100), &mut rx).await {
                        match notification {
                            Ok(_) => break Ok(()),
                            Err(e) => {
                                error!("Error from pool notification receiver: {}", e);
                                break Err(());
                            }
                        }
                    } else {
                        debug!("pool not found. trying again");
                    }
                }
            })
            .await;
            if result.is_err() || result.unwrap().is_err() {
                error!("pool not found within limit, no request will be sent, returning");
                //return tx back to container
                CONNECTION_POOLS_USAGE_LISTENER
                    .write()
                    .await
                    .insert(job_id, rx);
                return None;
            }
            // if received notification
            get_existing_queue_pool(&job_id).await.unwrap()
        } else {
            get_new_queue_pool(host_port.clone(), keep_alive).await
        }
    };

    // {
    //     let mut write_guard = JOB_STATUS.write().await;
    //     write_guard.insert(job_id.clone(), JobStatus::InProgress);
    // }
    // #[cfg(feature = "cluster")]
    let tx = {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        CONNECTION_POOLS_USAGE_LISTENER
            .write()
            .await
            .insert(job_id.clone(), rx);
        tx
    };
    Some((queue_pool, tx))
}

async fn get_new_queue_pool(host_port: String, keep_alive: ConnectionKeepAlive) -> QueuePool {
    debug!("Creating new pool for: {}", &host_port);
    QueuePool::new(host_port, keep_alive)
}

// #[cfg(feature = "cluster")]
async fn get_existing_queue_pool(job_id: &str) -> Option<QueuePool> {
    CONNECTION_POOLS.write().await.remove(job_id)
}

async fn check_for_request_msg(rx: &mut Receiver<MessageFromPrimary>) -> Result<Request, AnyError> {
    //Request message can come after a Reset message, so try twice
    for _ in 0..=1 {
        //timeout of 30 sec
        trace!("[check_for_request_msg] ...");
        let msg = tokio::time::timeout(Duration::from_secs(30), rx.recv()).await;
        info!("[check_for_request_msg] - received {:?}", msg);
        let msg = msg??.ok_or_else(|| anyhow!("No message received"))?;

        match msg {
            MessageFromPrimary::Request(request) => {
                return Ok(request);
            }
            MessageFromPrimary::Reset => {}
            _ => {
                return Err(anyhow!(
                    "Expected MessageFromPrimary::Request|Reset, but didn't receive"
                ))
            }
        }
    }
    Err(anyhow!("No message received"))
}

async fn check_for_metadata_msg(
    rx: &mut Receiver<MessageFromPrimary>,
) -> Result<Metadata, AnyError> {
    //close the connection if nothing received for 30 sec
    let msg = tokio::time::timeout(Duration::from_secs(30), rx.recv()).await;
    let msg = msg??.ok_or_else(|| anyhow!("No message received"))?;

    match msg {
        MessageFromPrimary::Metadata(metadata) => Ok(metadata),
        _ => Err(anyhow!(
            "Expected MessageFromPrimary::Metadata, but didn't receive"
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_rate_msg(
    rate: RateMessage,
    mut prev_connection_count: u32,
    metrics: &Arc<Metrics>,
    mut time_offset: i128,
    job_id: String,
    queue_pool: &mut QueuePool,
    req_spec: &mut RequestSpecEnum,
    response_assertion: Arc<ResponseAssertion>,
    lua_executor_sender: Option<LuaExecSender>,
) -> (u32, i128) {
    let start_of_cycle = Instant::now();
    let connection_count = rate.connections;
    let qps = rate.qps;

    if prev_connection_count != connection_count {
        queue_pool
            .set_connection_count(connection_count as usize, metrics)
            .await;
        metrics.pool_size(connection_count as f64);
        prev_connection_count = connection_count;
    }
    queue_pool.last_use = Instant::now();

    let mut corrected_with_offset = start_of_cycle;
    if time_offset > 0 {
        //the previous cycle took longer than cycle duration
        // use less time for next one
        corrected_with_offset = start_of_cycle
            .checked_sub(Duration::from_millis(time_offset as u64))
            .unwrap();
    }

    send_multiple_requests(
        qps,
        job_id.clone(),
        metrics.clone(),
        queue_pool,
        connection_count,
        corrected_with_offset,
        &response_assertion,
        req_spec,
        lua_executor_sender,
    )
    .await;
    time_offset = start_of_cycle.elapsed().as_millis() as i128 - CYCLE_LENGTH_IN_MILLIS;
    (prev_connection_count, time_offset)
}

#[allow(clippy::too_many_arguments)]
async fn send_multiple_requests(
    number_of_req: u32,
    job_id: String,
    metrics: Arc<Metrics>,
    queue_pool: &mut QueuePool,
    connection_count: u32,
    start_of_cycle: Instant,
    response_assertion: &Arc<ResponseAssertion>,
    req_spec: &mut RequestSpecEnum,
    lua_executor_sender: Option<LuaExecSender>,
) {
    debug!(
        "[{}] [send_multiple_requests] - count: {}, connection count: {}",
        &job_id, &number_of_req, &connection_count
    );

    if number_of_req == 0 {
        trace!("no requests are sent, qps is 0",);
        return;
    }
    let return_pool = queue_pool.get_return_pool();

    let time_remaining =
        move || CYCLE_LENGTH_IN_MILLIS - start_of_cycle.elapsed().as_millis() as i128;
    let interval_between_requests = |remaining_requests: u32| {
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
    let mut total_remaining_qps = number_of_req;
    let request_bundle_size: u32 = max((number_of_req / 100) + 1, request_bundle_size() as u32);
    let bundle_size = if connection_count == 0 {
        //elastic pool
        min(number_of_req, request_bundle_size)
    } else {
        min(number_of_req, min(connection_count, request_bundle_size))
    };
    let mut sleep_time = interval_between_requests(number_of_req) * bundle_size as f32;
    //reserve 10% of the time for the internal logic
    sleep_time = (sleep_time - (sleep_time * 0.1)).floor();

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
        debug!("[{}] - connection requested:{}, available connection: {}, request remaining: {}, remaining duration: {}, sleep duration: {}",
            &job_id, requested_connection, available_connection, total_remaining_qps, remaining_t, sleep_time);
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
            let requests_to_send = get_requests(req_spec, available_connection).await;
            if requests_to_send.is_err() {
                error!("Error while generating request - {:?}", requests_to_send);
                return;
            }
            let mut requests_to_send = requests_to_send.unwrap();
            if requests_to_send.is_empty() {
                error!("Error while generating request - no request generated");
            }
            // if number of request < available_connection, clone the connections and fill up
            let available_req = requests_to_send.len();
            if available_req < available_connection {
                //todo this logic is for file provider. Should handle in there
                fill_req_if_less_than_connections(
                    available_connection,
                    available_req,
                    &mut requests_to_send,
                );
            }
            let id = job_id.clone();
            let m = metrics.clone();
            let rp = return_pool.clone();
            let assertion = response_assertion.clone();
            let lua_sender = lua_executor_sender.clone();
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
                            lua_sender.clone(),
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

fn fill_req_if_less_than_connections(
    available_connection: usize,
    available_req: usize,
    requests_to_send: &mut Vec<HttpReq>,
) {
    let mut i = 0;
    let mut need_to_insert = available_connection - available_req;
    loop {
        if need_to_insert == 0 {
            break;
        }
        if i < available_req {
            requests_to_send.push(requests_to_send[i].clone());
            i += 1;
            need_to_insert -= 1;
        } else {
            //reached end, start from beginning again
            i = 0;
        }
    }
}

async fn send_single_requests(
    req: HttpReq,
    job_id: String,
    metrics: &Arc<Metrics>,
    mut connection: HttpConnection,
    assertion: &ResponseAssertion,
    lua_executor_sender: Option<LuaExecSender>,
) -> HttpConnection {
    let body = if let Some(body) = req.body.clone() {
        trace!("[send_single_requests] - has body: {}", &body);
        Bytes::from(body).into()
    } else {
        Body::empty()
    };
    let request_uri = Uri::try_from(req.url.clone()).unwrap();
    //todo remove unwrap
    let mut request = http::Request::builder()
        .uri(request_uri.clone())
        .method(req.method.clone());
    let headers = request.headers_mut().unwrap();
    for (k, v) in req.headers.iter() {
        try_add_header(headers, k, v);
    }
    let request = request.body(body).unwrap();
    trace!("sending request: {:?}", &request.uri());

    let request = connection.request_handle.send_request(request);
    let request_future = HttpRequestFuture {
        state: HttpRequestState::Init,
        timer: None,
        job_id,
        request: Pin::new(Box::new(request)),
        body: None,
        status: None,
        metrics,
        assertion,
        request_uri,
        original_request: OriginalRequest::Request(req),
        lua_executor_sender,
        lua_assert_future: None,
    };
    request_future.await;
    connection
}

fn try_add_header(headers: &mut HeaderMap, k: &str, v: &str) -> Option<HeaderName> {
    let header_name = HeaderName::from_str(k).ok()?;
    let header_value = HeaderValue::from_str(v).ok()?;
    headers.insert(header_name.clone(), header_value);
    Some(header_name)
}

async fn get_requests(
    req_spec: &mut RequestSpecEnum,
    count: usize,
) -> anyhow::Result<Vec<HttpReq>> {
    match req_spec {
        RequestSpecEnum::RequestList(req) => req.get_n(count),
        RequestSpecEnum::RequestFile(req) => req.get_n(count),
        RequestSpecEnum::RandomDataRequest(req) => req.get_n(count),
        RequestSpecEnum::SplitRequestFile(req) => req.get_n(count),
    }
    .await
}

fn job_id(request_name: &Option<String>) -> String {
    request_name
        .clone()
        .map_or(Uuid::new_v4().to_string(), |n| {
            let uuid = Regex::new(
                r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}(_[0-9]+)?$",
            )
            .unwrap();
            if uuid.is_match(&n) {
                n
            } else {
                let mut name = n.trim().to_string();
                name.push('-');
                name.push_str(Uuid::new_v4().to_string().as_str());
                name
            }
        })
}

fn prepare(
    request: &mut Request,
    primary_uri: String,
) -> BoxFuture<'static, Result<(), anyhow::Error>> {
    initiator_for_request_from_primary(request, primary_uri)
}

/// For RequestFile -
/// * Update file name with full path
/// * Download file from primary
pub(crate) fn initiator_for_request_from_primary(
    request: &mut Request,
    primary_uri: String,
) -> BoxFuture<'static, Result<(), anyhow::Error>> {
    return match &mut request.req {
        RequestSpecEnum::RequestFile(req) => {
            download_request_file_from_primary(req.file_name.to_string(), primary_uri).boxed()
        }
        RequestSpecEnum::SplitRequestFile(req) => {
            download_request_file_from_primary(req.file_name.to_string(), primary_uri).boxed()
        }
        RequestSpecEnum::RandomDataRequest(_) | RequestSpecEnum::RequestList(_) => noop().boxed(),
    };
}

pub async fn noop() -> Result<(), anyhow::Error> {
    Ok(())
}

async fn download_request_file_from_primary(
    file_path: String,
    primary_uri: String,
) -> Result<(), anyhow::Error> {
    let file_path = PathBuf::from(&file_path);
    debug!(
        "[download_request_file_from_primary] - downloading file {:?} from primary",
        &file_path
    );
    if let Ok(true) = file_path.try_exists() {
        debug!("file {:?} already exists", &file_path);
        return Ok(());
    }

    // next steps will never be executed in standalone mode
    let mut data_file_destination = File::create(&file_path).await.map_err(|e| {
        error!("[download_request_file_from_primary] - error: {:?}", e);
        anyhow::anyhow!("filed to create file {:?}", &file_path)
    })?;
    let filename = &file_path.file_name().and_then(|f| f.to_str()).unwrap();
    download_file_from_url(&primary_uri, filename, &mut data_file_destination).await?;
    Ok(())
}

pub async fn download_file_from_url(
    host: &str,
    filename: &str,
    data_file_destination: &mut File,
) -> Result<(), anyhow::Error> {
    let url =
        Url::parse(format!("http://{}:{}", &host, overload_http::http_port()).as_str()).unwrap();
    let url = url
        .join(PATH_REQUEST_DATA_FILE_DOWNLOAD)
        .and_then(|url| url.join(filename))
        .unwrap();
    debug!("[download_file_from_url] - downloading file from {}", &url);
    let req = hyper::Request::builder()
        .uri(url.as_str())
        .method("GET")
        .body(Body::empty())
        .map_err(|e| {
            error!("{:?}", &e);
            anyhow::anyhow!("building request failed for {}, error: {:?}", &url, &e)
        })?;
    let client = Client::new();
    let mut resp = client.request(req).await?;

    fn to_tokio_async_read(r: impl futures::io::AsyncRead) -> impl tokio::io::AsyncRead {
        tokio_util::compat::FuturesAsyncReadCompatExt::compat(r)
    }
    let futures_io_async_read =
        TryStreamExt::map_err(resp.body_mut(), |e| io::Error::new(io::ErrorKind::Other, e))
            .into_async_read();
    let mut tokio_async_read = to_tokio_async_read(futures_io_async_read);
    tokio::io::copy(&mut tokio_async_read, data_file_destination).await?;
    let _ = data_file_destination.flush().await;
    Ok(())
}

#[cfg(test)]
mod test {
    use crate::connection::QueuePool;
    #[cfg(feature = "cluster")]
    use crate::primary::get_sender_for_secondary;
    use crate::secondary::{
        fill_req_if_less_than_connections, handle_rate_msg, primary_listener,
        send_multiple_requests,
    };
    use crate::test_common::init;
    #[cfg(feature = "cluster")]
    use crate::test_common::{cluster_node, get_request};
    use crate::{
        send_metadata_with_primary, send_request_to_secondary, RateMessage, DEFAULT_REMOC_PORT,
    };
    use log::info;
    use overload_http::{ConnectionKeepAlive, HttpReq, RequestList, RequestSpecEnum};
    use overload_metrics::MetricsFactory;
    use regex::Regex;
    use response_assert::ResponseAssertion;
    use std::cmp::max;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use url::Url;

    #[test]
    fn test_fill_req_if_less_than_connections() {
        let req = HttpReq {
            id: "".to_string(),
            method: http::Method::GET,
            url: "http://example.com/anything/abcdef/1000001".to_string(),
            body: None,
            headers: Default::default(),
        };
        let mut req = vec![req];
        fill_req_if_less_than_connections(7, 1, &mut req);
        assert_eq!(req.len(), 7);
    }

    #[cfg(feature = "cluster")]
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn test_primary_listener() {
        // init();
        tokio::spawn(primary_listener(
            DEFAULT_REMOC_PORT,
            &overload_metrics::METRICS_FACTORY,
        ));

        let mut instances = vec![];
        let node = cluster_node(3030, 3031);
        instances.push(
            node.service_instance()
                .instance_id()
                .as_ref()
                .unwrap()
                .clone(),
        );
        let handle1 = tokio::spawn(async move {
            let sender = get_sender_for_secondary(&node).await;
            assert!(sender.is_some());
            let sender = sender.unwrap();
            let request = get_request("localhost".to_string(), 8082);
            let mut senders = HashMap::new();
            senders.insert("localhost".to_string(), sender);
            let instances = ["localhost".to_string()];
            send_metadata_with_primary("127.0.0.1", &mut senders, &instances).await;
            let result = send_request_to_secondary(request, &mut senders, &instances).await;
            // let result = init_sender(request, "127.0.0.1".to_string(), &mut sender).await;
            info!("init result: {:?}", &result);
            assert!(result.is_ok());
            tokio::time::sleep(Duration::from_millis(10)).await;
        });

        let node = cluster_node(3030, 3031);
        instances.push(
            node.service_instance()
                .instance_id()
                .as_ref()
                .unwrap()
                .clone(),
        );
        let handle2 = tokio::spawn(async move {
            let sender = get_sender_for_secondary(&node).await;
            assert!(sender.is_some());
            let sender = sender.unwrap();
            let request = get_request("localhost".to_string(), 8082);
            let mut senders = HashMap::new();
            senders.insert("localhost".to_string(), sender);
            send_metadata_with_primary("localhost", &mut senders, &instances).await;
            // send_metadata("localhost", &mut senders).await;
            let result = send_request_to_secondary(request, &mut senders, &instances).await;
            // let result = init_sender(request, "127.0.0.1".to_string(), &mut sender).await;
            info!("init result: {:?}", &result);
            assert!(result.is_ok());
            tokio::time::sleep(Duration::from_millis(10)).await;
        });
        assert!(handle1.await.is_ok());
        assert!(handle2.await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn test_send_multiple_requests() {
        // init();
        let mock_server = httpmock::MockServer::start_async().await;
        let url = Url::parse(&mock_server.base_url()).unwrap();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path_matches(Regex::new(r"^/anything/[a-zA-Z]{6,15}/1([10])\d{5}$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .header("Connection", "keep-alive")
                .body(r#"{"hello": "world"}"#);
        });

        let mut req_spec = req_spec_enum(&url);
        let job_id = uuid::Uuid::new_v4().to_string();
        let metrics = MetricsFactory::default().metrics(&job_id).await;
        let mut queue_pool = pool(&url);
        let assertion = Arc::new(ResponseAssertion::default());
        let start = Instant::now();
        send_multiple_requests(
            25,
            job_id,
            metrics,
            &mut queue_pool,
            5,
            start,
            &assertion,
            &mut req_spec,
            None,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(max(
            0,
            1000 - (Instant::now() - start).as_millis() as i32,
        ) as u64))
        .await;
        info!("mock hits: {}", mock.hits_async().await);
        mock.assert_hits_async(25).await;
    }

    fn req_spec_enum(url: &Url) -> RequestSpecEnum {
        let req = HttpReq {
            id: "".to_string(),
            method: http::Method::GET,
            url: url.join("anything/abcdef/1000001").unwrap().to_string(),
            body: None,
            headers: Default::default(),
        };
        let req = vec![req];
        RequestSpecEnum::RequestList(RequestList { data: req })
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn test_handle_rate_msg() {
        // init();
        let rate_message = RateMessage {
            qps: 25,
            connections: 5,
        };
        let job_id = uuid::Uuid::new_v4().to_string();
        let metrics = MetricsFactory::default().metrics(&job_id).await;

        let mock_server = httpmock::MockServer::start_async().await;
        let url = Url::parse(&mock_server.base_url()).unwrap();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path_matches(Regex::new(r"^/anything/[a-zA-Z]{6,15}/1([10])\d{5}$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .header("Connection", "keep-alive")
                .body(r#"{"hello": "world"}"#);
        });

        let mut queue_pool = pool(&url);
        let assertion = Arc::new(ResponseAssertion::default());

        let mut req_spec = req_spec_enum(&url);

        let start = Instant::now();
        handle_rate_msg(
            rate_message,
            0u32,
            &metrics,
            0i128,
            job_id,
            &mut queue_pool,
            &mut req_spec,
            assertion,
            None,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(max(
            0,
            1000 - (Instant::now() - start).as_millis() as i32,
        ) as u64))
        .await;
        info!("mock hits: {}", mock.hits_async().await);
        mock.assert_hits_async(25).await;
    }

    fn pool(url: &Url) -> QueuePool {
        QueuePool::new(
            format!("{}:{}", url.host_str().unwrap(), url.port().unwrap()),
            ConnectionKeepAlive::default(),
        )
    }
}
