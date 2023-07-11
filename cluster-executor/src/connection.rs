use futures_util::{stream::FuturesUnordered, StreamExt};
use hyper::client::conn;
use hyper::client::conn::{Connection, SendRequest};
use hyper::{Body, Error};
use log::{debug, trace, warn};
use overload_metrics::Metrics;
use std::collections::VecDeque;
use std::env;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tower::ServiceExt;

pub use overload_http::ConnectionKeepAlive;

type SharedMutableVec = Arc<RwLock<Vec<HttpConnection>>>;
type InteriorMutable = SharedMutableVec;

const ENV_NAME_POOL_CONNECTION_TIMEOUT: &str = "POOL_CONNECTION_TIMEOUT";
const DEFAULT_POOL_CONNECTION_TIMEOUT: u64 = 1000;

pub fn pool_connection_timeout() -> u64 {
    env::var(ENV_NAME_POOL_CONNECTION_TIMEOUT)
        .map_err(|_| ())
        .and_then(|val| u64::from_str(&val).map_err(|_| ()))
        .unwrap_or(DEFAULT_POOL_CONNECTION_TIMEOUT)
}

pub(crate) struct QueuePool {
    pub(crate) max_connection: i64,
    pub(crate) connections: VecDeque<HttpConnection>,
    pub(crate) recyclable_connections: SharedMutableVec,
    pub(crate) new_connections: SharedMutableVec,
    busy_connections: usize,
    _total_connection: AtomicU32,
    _new_connection: AtomicU32,
    _recyclable_connection: AtomicU32,
    host_port: String,
    // An elastic pools adds connections when required.
    // the pool starts with max_connection=0 and never changes.
    // once it changes max_connection, it'll become non-elastic
    elastic: bool,
    keep_alive: ConnectionKeepAlive,
    pub(crate) last_use: Instant,
    failed_connection: Arc<AtomicUsize>,
}

impl QueuePool {
    pub fn new(host_port: String, keep_alive: ConnectionKeepAlive) -> Self {
        QueuePool {
            max_connection: 0,
            connections: VecDeque::new(),
            recyclable_connections: Arc::new(RwLock::new(Vec::new())),
            new_connections: Arc::new(RwLock::new(Vec::new())),
            busy_connections: 0,
            _total_connection: AtomicU32::new(0),
            _new_connection: AtomicU32::new(0),
            _recyclable_connection: AtomicU32::new(0),
            host_port,
            elastic: true,
            keep_alive,
            last_use: Instant::now(),
            failed_connection: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub async fn set_connection_count(&mut self, connection_count: usize, metrics: &Arc<Metrics>) {
        if self.elastic && connection_count != self.max_connection as usize {
            self.elastic = false; // make it non-elastic as max_connection has changed
        }
        let avail_con_len = self.connections.len();
        if avail_con_len > connection_count {
            self.connections
                .drain(0..(avail_con_len - connection_count));
        }
        let avail_con_len = self.connections.len();
        let connection_count = connection_count as i64;
        let mut diff = connection_count - self.max_connection;
        if diff > 0 {
            let nc = self.new_connections.read().await.len();
            // no need to take recyclable_connections into consideration as they're already getting
            // counted in busy_connections.
            // let rc = self.recyclable_connections.read().await.len();
            let active_con = nc + self.busy_connections + avail_con_len;
            diff = std::cmp::max(0, connection_count - active_con as i64);
        };
        debug!(
            "setting pool size to {}, previous: {}, diff: {}",
            &connection_count, self.max_connection, diff
        );
        self.max_connection = connection_count;
        let new_connections = self.new_connections.clone();
        let recycled = self.recyclable_connections.clone();
        let host_port = self.host_port.clone();
        let resizer = QueuePool::resize_connection_pool(
            new_connections,
            recycled,
            host_port,
            diff,
            self.keep_alive,
            metrics.clone(),
            self.failed_connection.clone(),
        );
        tokio::spawn(resizer);
    }

    pub async fn get_connections(
        &mut self,
        connection_request: u32,
        metrics: &Arc<Metrics>,
    ) -> Vec<HttpConnection> {
        debug!(
            "request received for {} connections, max_connection: {}, busy_connection: {}",
            connection_request, self.max_connection, self.busy_connections
        );
        let returned_to_pool = self
            .claim_connections(self.recyclable_connections.clone(), metrics)
            .await;
        self.claim_connections(self.new_connections.clone(), metrics)
            .await;
        self.busy_connections -= returned_to_pool;
        if self.elastic {
            let diff = self.connections.len() as i64 - connection_request as i64;
            if diff < 0 {
                let diff = diff.abs();
                debug!("elastic pool, requesting {} more connections", diff);
                // need more connections
                self.add_new_connection_mut(diff, metrics, self.failed_connection.clone());
            }
        } else {
            //re-enforce the pool size
            let available = self.connections.len();
            let current_active = available + self.busy_connections;
            let diff = current_active as i64 - self.max_connection;
            if diff > 0 {
                debug!(
                    "re-enforce the pool size - busy: {}, available: {}, pool size: {}",
                    self.busy_connections, available, self.max_connection
                );
                //has extra connections, drop them
                self.connections
                    .truncate((std::cmp::max(0, available as i64 - diff)) as usize);
                let change = available - self.connections.len();
                if change > 0 {
                    metrics.pool_connection_dropped(change as u64);
                    metrics.pool_connection_idle(-(change as f64));
                }
            }
        }

        let available = self.connections.len();
        let conn_to_get = std::cmp::min(connection_request as usize, available);
        let mut vec = Vec::with_capacity(conn_to_get);
        let mut futures = FuturesUnordered::new();

        let mut dropped: u64 = 0;
        for _ in 0..connection_request {
            if let Some(con) = self.connections.pop_front() {
                if con.should_drop {
                    dropped += 1;
                    drop(con); //for readability
                } else {
                    futures.push(QueuePool::check_ready(con))
                }
            } else {
                break;
            }
        }
        let mut broken = 0;
        while let Some((con, result)) = futures.next().await {
            if result.is_ok() {
                vec.push(con);
            } else {
                broken += 1;
            }
        }
        // refill broken & dropped connections
        self.add_new_connection_mut(
            (broken + dropped + self.failed_connection.swap(0, Ordering::Relaxed) as u64) as i64,
            metrics,
            self.failed_connection.clone(),
        );
        metrics.pool_connection_broken(broken);
        metrics.pool_connection_dropped(broken + dropped);
        let len = vec.len();
        self.busy_connections += len;
        let len = len as f64;
        metrics.pool_connection_busy(len);
        metrics.pool_connection_idle(-(len));
        vec
    }

    fn add_new_connection_mut(
        &mut self,
        count: i64,
        metrics: &Arc<Metrics>,
        failed_connection: Arc<AtomicUsize>,
    ) {
        debug!("[add_new_connection_mut] - adding {count} new connections");
        if count == 0 {
            return;
        }
        let new_connections = self.new_connections.clone();
        let host_port = self.host_port.clone();
        let conn_gen = Self::add_new_connection(
            new_connections,
            host_port,
            count,
            self.keep_alive,
            metrics.clone(),
            failed_connection,
        );
        tokio::spawn(conn_gen);
    }

    pub fn get_return_pool(&self) -> InteriorMutable {
        self.recyclable_connections.clone()
    }

    /// Add or remove connections
    #[allow(clippy::comparison_chain)]
    async fn resize_connection_pool(
        new_connections: SharedMutableVec,
        recycle_connections: SharedMutableVec,
        host_port: String,
        diff: i64,
        keep_alive: ConnectionKeepAlive,
        metrics: Arc<Metrics>,
        failed_connection: Arc<AtomicUsize>,
    ) {
        debug!("resizing pool by: {}", &diff);
        if diff < 0 {
            // remove connections from the pool
            let diff = diff.unsigned_abs() as usize;
            // try removing recycled connection first
            let removed = QueuePool::resize_conn_container(recycle_connections, diff).await;
            debug!("removed {} connections from recycled pool", &removed);
            // didn't have enough connections in recycle_connections, try removing from new connection
            if removed < diff {
                let removed =
                    QueuePool::resize_conn_container(new_connections, diff - removed).await;
                debug!("removed {} connections from new connection pool", &removed);
            }
        } else if diff > 0 {
            // need to add new connections
            debug!("resizing, requesting {} more connections", diff);
            Self::add_new_connection(
                new_connections,
                host_port,
                diff,
                keep_alive,
                metrics,
                failed_connection,
            )
            .await;
        } else {
        }
    }

    async fn add_new_connection(
        new_connections: SharedMutableVec,
        host_port: String,
        count: i64,
        keep_alive: ConnectionKeepAlive,
        metrics: Arc<Metrics>,
        failed_connection: Arc<AtomicUsize>,
    ) {
        debug!("Adding {} new connections", count);
        metrics.pool_connection_attempt(count as u64);
        let mut futures = FuturesUnordered::new();
        for _ in 0..count {
            let connection_future = open_connection(&host_port, keep_alive);
            futures.push(connection_future);
        }
        let mut conn = Vec::with_capacity(count as usize);
        while let Ok(Some(connection)) = tokio::time::timeout(
            Duration::from_millis(pool_connection_timeout()),
            futures.next(),
        )
        .await
        {
            if let Some(connection) = connection {
                conn.push(connection)
            }
        }
        drop(futures);
        let len = conn.len();
        let failed = count as usize - len;
        failed_connection.fetch_add(failed, Ordering::Relaxed);
        debug!("Added {} new connections", len);
        debug!("failed to add {} connections", failed);
        metrics.pool_connection_success(len as u64);
        metrics.pool_connection_idle(len as f64);
        new_connections.write().await.append(&mut conn);
    }

    async fn resize_conn_container(container: SharedMutableVec, to_be_removed: usize) -> usize {
        let mut write_guard = container.write().await;
        let len = write_guard.len();
        if len < to_be_removed {
            write_guard.clear();
            len
        } else {
            write_guard.truncate(len - to_be_removed);
            to_be_removed
        }
    }
    async fn claim_connections(
        &mut self,
        con_container: SharedMutableVec,
        _metrics: &Arc<Metrics>,
    ) -> usize {
        let mut write_guard = con_container.write().await;
        let len = write_guard.len();
        if len == 0 {
            return 0;
        }
        let drain = write_guard.drain(..);
        let _ = self.connections.try_reserve(len);
        for conn in drain {
            self.connections.push_back(conn);
        }
        len
    }

    #[inline]
    pub(crate) async fn return_connection(
        return_pool: &SharedMutableVec,
        mut connection: HttpConnection,
        metrics: &Arc<Metrics>,
    ) {
        trace!("[return_connection] - returning connection");
        //no need to check connection ready status here, checking when giving out connections
        if connection.expire_at < since_epoch() || connection.remaining_reuse <= 1 {
            debug!(
                "[return_connection] - connection should drop - expire_at:{}, remaining_reuse: {}",
                connection.expire_at, connection.remaining_reuse
            );
            connection.should_drop = true;
        }
        connection.remaining_reuse -= 1;
        metrics.pool_connection_busy(-1_f64);
        metrics.pool_connection_idle(1_f64);
        return_pool.write().await.push(connection);
    }
    /*
    pub(crate) async fn return_connection_vec(
        return_pool: &SharedMutableVec,
        mut connections: Vec<HttpConnection>,
        metrics: &Arc<Metrics>,
    ) {
        metrics.pool_connection_busy(-(connections.len() as f64));
        let mut ready_futures: FuturesUnordered<_>=connections.drain(..)
            .map(|connection| {
                Self::check_ready(connection)
            })
            .collect();
        // let handle = connection.request_handle.ready().await;
        let mut return_pool_write_guard = return_pool.write().await;
        while let Some((mut connection, result)) = ready_futures.next().await {
            if let Err(e) = result {
                debug!("error - connection not ready, error: {:?}", e);
                connection.broken = true;
                metrics.pool_connection_broken(1);
            } else {
                metrics.pool_connection_idle(1_f64);
            }
            return_pool_write_guard.push(connection)
        }

    }
    */

    #[inline]
    async fn check_ready<'a>(
        mut connection: HttpConnection,
    ) -> (HttpConnection, Result<(), Error>) {
        let result = connection.request_handle.ready().await.map(|_| {});
        (connection, result)
    }
}

#[allow(dead_code)]
pub(crate) struct HttpConnection {
    pub(crate) connection: Option<Connection<TcpStream, Body>>,
    pub(crate) request_handle: SendRequest<Body>,
    should_drop: bool,
    expire_at: u64,
    remaining_reuse: i32,
    join_handle: JoinHandle<()>,
}

async fn open_connection(
    host_port: &str,
    keep_alive: ConnectionKeepAlive,
) -> Option<HttpConnection> {
    debug!("Opening connection to: {}", host_port);
    let tcp_stream = TcpStream::connect(host_port).await;
    match tcp_stream {
        Ok(stream) => {
            trace!("handshaking with: {}", host_port);
            match conn::handshake(stream).await {
                Ok(result) => {
                    let con = result.1;
                    let join_handle = tokio::spawn(async move {
                        if let Err(e) = con.await {
                            eprintln!("Error in connection: {}", e);
                        }
                    });
                    let connection = HttpConnection {
                        request_handle: result.0,
                        connection: None,
                        should_drop: false,
                        expire_at: get_expiry(keep_alive.ttl),
                        remaining_reuse: keep_alive
                            .max_request_per_connection
                            .get()
                            .try_into()
                            .unwrap_or(i32::MAX),
                        join_handle,
                    };
                    // no need to check connection ready status here
                    // checking when giving out connections
                    // match ready_connection(&mut connection).await {
                    //     Ok(_) => Some(connection),
                    //     Err(_) => None,
                    // }
                    Some(connection)
                }
                Err(err) => {
                    warn!("failed handshaking with: {}, error: {}", host_port, err);
                    None
                }
            }
        }
        Err(err) => {
            warn!(
                "failed to open connection to: {}, error:{:?}",
                host_port,
                err.kind()
            );
            None
        }
    }
}

fn since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn get_expiry(ttl: u32) -> u64 {
    since_epoch().checked_add(ttl as u64).unwrap_or(u64::MAX)
}

/*
async fn ready_connection(conn: &mut HttpConnection) -> Result<(), ()> {
    trace!("Checking if connection is not ready");
    match timeout(Duration::from_millis(10), conn.request_handle.ready()).await {
        Ok(_) => Ok(()),
        Err(err) => {
            warn!("Connection is not ready - {:?}", err);
            Err(())
        }
    }
}
*/
