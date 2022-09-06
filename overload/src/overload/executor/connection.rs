use async_trait::async_trait;
use bb8::ManageConnection;
use bb8::PoolCustomizer;
use futures_util::{stream::FuturesUnordered, StreamExt};
use hyper::client::conn;
use hyper::client::conn::{Connection, SendRequest};
use hyper::Body;
use log::{debug, trace, warn};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};
use tower::ServiceExt;

type SharedMutableVec = Arc<RwLock<Vec<HttpConnection>>>;
type InteriorMutable = SharedMutableVec;

pub(crate) struct QueuePool {
    pub(crate) max_connection: i64,
    pub(crate) connections: VecDeque<HttpConnection>,
    pub(crate) recyclable_connections: SharedMutableVec,
    pub(crate) new_connections: SharedMutableVec,
    _total_connection: AtomicU32,
    _new_connection: AtomicU32,
    _recyclable_connection: AtomicU32,
    host_port: String,
    // An elastic pools adds connections when required.
    // the pool starts with max_connection =0 and never changes.
    // once it changes max_connection, it'll become non-elastic
    elastic: bool,
}

impl QueuePool {
    pub fn new(host_port: String) -> Self {
        QueuePool {
            max_connection: 0,
            connections: VecDeque::new(),
            recyclable_connections: Arc::new(RwLock::new(Vec::new())),
            new_connections: Arc::new(RwLock::new(Vec::new())),
            _total_connection: AtomicU32::new(0),
            _new_connection: AtomicU32::new(0),
            _recyclable_connection: AtomicU32::new(0),
            host_port,
            elastic: true,
        }
    }

    pub fn set_connection_count(&mut self, connection_count: usize) {
        trace!("setting pool size to {}", &connection_count);
        if self.elastic && connection_count != self.max_connection as usize {
            self.elastic = false; // make it non-elastic as max_connection has changed
        }
        if self.connections.len() > connection_count {
            self.connections
                .drain(0..(self.connections.len() - connection_count));
        }
        let connection_count = connection_count as i64;
        let diff = connection_count - self.max_connection;
        self.max_connection = connection_count;
        let new_connections = self.new_connections.clone();
        let recycled = self.recyclable_connections.clone();
        let host_port = self.host_port.clone();
        let resizer = QueuePool::resize_connection_pool(new_connections, recycled, host_port, diff);
        tokio::spawn(resizer);
    }

    pub async fn get_connections(&mut self, count: u32) -> Vec<HttpConnection> {
        trace!("request received for {} connections", count);
        let broken = self
            .claim_connections(self.recyclable_connections.clone())
            .await;
        self.claim_connections(self.new_connections.clone()).await;
        if self.elastic {
            let diff = self.connections.len() as i64 - count as i64;
            if diff < 0 {
                // need more connections
                self.add_new_connection_mut(diff.abs());
            }
        }
        if broken != 0 {
            //refill broken connections
            self.add_new_connection_mut(broken);
        }
        let mut vec = Vec::with_capacity(std::cmp::min(count as usize, self.connections.len()));
        for _ in 0..count {
            if let Some(con) = self.connections.pop_front() {
                vec.push(con);
            } else {
                break;
            }
        }
        vec
    }

    fn add_new_connection_mut(&mut self, count: i64) {
        let new_connections = self.new_connections.clone();
        let host_port = self.host_port.clone();
        let conn_gen = Self::add_new_connection(new_connections, host_port, count);
        tokio::spawn(conn_gen);
    }

    pub fn get_return_pool(&self) -> InteriorMutable {
        self.recyclable_connections.clone()
    }

    /// Add or remove connections
    async fn resize_connection_pool(
        new_connections: SharedMutableVec,
        recycle_connections: SharedMutableVec,
        host_port: String,
        diff: i64,
    ) {
        trace!("resizing pool to: {}", &diff);
        if diff < 0 {
            // remove connections from the pool
            let diff = diff.unsigned_abs() as usize;
            // try removing recycled connection first
            let removed = QueuePool::resize_conn_container(recycle_connections, diff).await;
            trace!("removed {} connections from recycled pool", &removed);
            // didn't have enough connections in recycle_connections, try removing from new connection
            if removed < diff {
                let removed =
                    QueuePool::resize_conn_container(new_connections, diff - removed).await;
                trace!("removed {} connections from new connection pool", &removed);
            }
        } else {
            // need to add new connections
            Self::add_new_connection(new_connections, host_port, diff).await;
        }
    }

    async fn add_new_connection(new_connections: SharedMutableVec, host_port: String, count: i64) {
        let mut futures = FuturesUnordered::new();
        for _ in 0..count {
            let connection_future = open_connection(&host_port);
            futures.push(connection_future);
        }
        let mut conn = vec![];
        while let Some(connection) = futures.next().await {
            match connection {
                Some(connection) => conn.push(connection),
                None => {}
            }
        }
        debug!("Adding {} new connections", conn.len());
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
    async fn claim_connections(&mut self, con_container: SharedMutableVec) -> i64 {
        let mut write_guard = con_container.write().await;
        let len = write_guard.len();
        let drain = write_guard.drain(..);
        self.connections.try_reserve(len).unwrap();
        let mut broken = 0;
        for conn in drain {
            if conn.broken {
                broken += 1;
                continue;
            }
            self.connections.push_back(conn);
        }
        broken
    }

    #[inline]
    pub(crate) async fn return_connection(
        return_pool: &SharedMutableVec,
        mut connection: HttpConnection,
    ) {
        let handle = connection.request_handle.ready().await;
        if let Err(e) = handle {
            warn!("error - connection not ready, error: {:?}", e);
            connection.broken = true;
        }
        return_pool.write().await.push(connection)
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct ConcurrentConnectionCountManager {
    min_number_of_connection: u32,
    number_of_connection: AtomicU32,
}

#[allow(dead_code)]
impl ConcurrentConnectionCountManager {
    pub fn new(conn_count: u32) -> ConcurrentConnectionCountManager {
        ConcurrentConnectionCountManager {
            min_number_of_connection: conn_count,
            number_of_connection: AtomicU32::new(conn_count),
        }
    }
}

impl Default for ConcurrentConnectionCountManager {
    /// at least one initial min size is required.
    /// If initial size is set to 0, updating the value later is not updating
    /// min pool size, for example following test case fails -
    /// ```compile_fail
    /// let customizer = ConcurrentConnectionCountManager::new(0);
    /// let customizer = Arc::new(customizer);
    /// let customizer_dyn: Arc<dyn PoolCustomizer> = customizer.clone();
    ///
    /// let pool = bb8::Pool::builder()
    ///     .pool_customizer(customizer_dyn)
    ///     .build(pool)
    ///     .await
    ///     .unwrap();
    /// assert_eq!(pool.state().connections,0);
    /// customizer.update(10);
    /// let _ = pool.get().await;
    /// tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    /// assert_eq!(pool.state().connections, 10);
    /// ```
    fn default() -> ConcurrentConnectionCountManager {
        ConcurrentConnectionCountManager {
            min_number_of_connection: 1,
            number_of_connection: AtomicU32::new(1),
        }
    }
}

impl PoolCustomizer for ConcurrentConnectionCountManager {
    fn min_idle(&self) -> u32 {
        trace!("Checking min pool size: {}", self.min_number_of_connection);
        self.min_number_of_connection
    }
    fn max_size(&self) -> u32 {
        let max = self.number_of_connection.load(Ordering::Relaxed);
        trace!("Checking max pool size: {}", max);
        max
    }

    fn update(&self, conn_count: u32) {
        trace!("updating pool connection count to: {}", &conn_count);
        let _ = &self
            .number_of_connection
            .store(conn_count, Ordering::Relaxed);
    }
}

#[allow(dead_code)]
pub(crate) struct HttpConnectionPool {
    host: String,
    // total_conn: i16,
    // borrowed_conn: i16,
}

#[allow(dead_code)]
impl HttpConnectionPool {
    /// host and port in format: `host-name:port`
    pub(crate) fn new(host_port: &str) -> HttpConnectionPool {
        HttpConnectionPool {
            host: host_port.to_string(),
            // total_conn: 0,
            // borrowed_conn: 0,
        }
    }
}

#[async_trait]
impl ManageConnection for HttpConnectionPool {
    type Connection = HttpConnection;
    type Error = ();

    async fn connect(&self) -> Result<Self::Connection, Self::Error> {
        let mut conn = open_connection(&self.host).await.ok_or(())?;
        ready_connection(&mut conn).await.map(|_| conn)
    }

    async fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Self::Error> {
        trace!("Checking if connection is ready");
        match timeout(Duration::from_millis(5), conn.request_handle.ready()).await {
            Ok(result) => match result {
                Ok(_) => Ok(()),
                Err(err) => {
                    warn!("Error while making connection ready: {}", err);
                    conn.broken = true;
                    Err(())
                }
            },
            Err(_) => {
                warn!("Couldn't make connection ready within the timeout");
                conn.broken = true;
                Err(())
            }
        }
    }

    fn has_broken(&self, conn: &mut Self::Connection) -> bool {
        conn.broken
    }
}

#[allow(dead_code)]
pub(crate) struct HttpConnection {
    pub(crate) connection: Option<Connection<TcpStream, Body>>,
    // pub(crate) request_handle: Mutex<SendRequest<Body>>,
    pub(crate) request_handle: SendRequest<Body>,
    broken: bool,
    join_handle: JoinHandle<()>,
}

async fn open_connection(host_port: &str) -> Option<HttpConnection> {
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
                    let mut connection = HttpConnection {
                        request_handle: result.0,
                        connection: None,
                        broken: false,
                        join_handle,
                    };
                    match ready_connection(&mut connection).await {
                        Ok(_) => Some(connection),
                        Err(_) => None,
                    }
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

#[cfg(test)]
mod test {
    use super::ConcurrentConnectionCountManager;
    use crate::executor::connection::HttpConnectionPool;
    use bb8::PoolCustomizer;
    use std::sync::Arc;
    use std::sync::Once;
    use std::time::Duration;

    static ONCE: Once = Once::new();

    fn setup() {
        ONCE.call_once(|| {
            let _ = tracing_subscriber::fmt()
                .with_env_filter("trace")
                .try_init();
        });
    }

    #[tokio::test]
    #[ignore]
    async fn test_pool() {
        setup();
        let wire_mock = wiremock::MockServer::start().await;
        let wire_mock_uri = wire_mock.uri();
        let url = url::Url::parse(&wire_mock_uri).unwrap();
        let host_port = format!("{}:{}", url.host_str().unwrap(), url.port().unwrap());
        let pool = HttpConnectionPool::new(&host_port);
        let customizer = ConcurrentConnectionCountManager::new(1);
        let customizer = Arc::new(customizer);
        let customizer_dyn: Arc<dyn PoolCustomizer> = customizer.clone();
        let reaper_rate = Duration::from_millis(200);

        let pool = bb8::Pool::builder()
            .connection_timeout(Duration::from_millis(100))
            .pool_customizer(customizer_dyn)
            .reaper_rate(reaper_rate)
            .idle_timeout(Some(Duration::from_millis(100)))
            .build(pool)
            .await
            .unwrap();

        assert_eq!(pool.state().connections, 1);
        assert_eq!(pool.state().idle_connections, 1);
        let x = pool.get().await.unwrap();

        //should timeout as pool size one but requested two
        let xx = pool.get().await;
        assert!(xx.is_err());

        assert_eq!(pool.state().connections, 1);
        assert_eq!(pool.state().idle_connections, 0);
        do_nothing(x); //use to avoid drop
        tokio::time::pause();
        tokio::time::advance(Duration::from_millis(200)).await;
        tokio::time::resume();
        let x = pool.get().await; //connection should return
        assert!(x.is_ok());
        drop(x);

        customizer.update(10);
        let con_1 = pool.get().await;
        let con_2 = pool.get().await;
        let con_3 = pool.get().await;
        let con_4 = pool.get().await;
        assert_eq!(pool.state().connections, 5);
        assert_eq!(pool.state().idle_connections, 1);
        do_nothing(con_1); //use to avoid drop
        do_nothing(con_2); //use to avoid drop
        do_nothing(con_3); //use to avoid drop
        do_nothing(con_4); //use to avoid drop
                           // tokio::time::pause();
                           // tokio::time::advance(reaper_rate).await;
        tokio::time::sleep(reaper_rate).await;
        assert_eq!(pool.state().idle_connections, 5);

        customizer.update(3);
        //wait for reaper to remove unused connection
        tokio::time::sleep(Duration::from_millis(350)).await; //time to reap
        let con_1 = pool.get().await;
        let con_2 = pool.get().await;
        let con_3 = pool.get().await;
        assert_eq!(pool.state().idle_connections, 0);
        assert_eq!(pool.state().connections, 3);

        let con_4 = pool.get().await;
        assert!(con_4.is_err());
        do_nothing(con_1); //use to avoid drop
        do_nothing(con_2); //use to avoid drop
        do_nothing(con_3); //use to avoid drop
    }
    fn do_nothing<T>(_a: T) {}
}
