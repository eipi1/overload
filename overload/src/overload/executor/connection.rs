use async_trait::async_trait;
use bb8::ManageConnection;
use bb8::PoolCustomizer;
use hyper::client::conn;
use hyper::client::conn::{Connection, SendRequest};
use hyper::Body;
use log::{debug, trace, warn};
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};
use tower::ServiceExt;

#[derive(Debug)]
pub(crate) struct ConcurrentConnectionCountManager {
    min_number_of_connection: u32,
    number_of_connection: AtomicU32,
}

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

pub(crate) struct HttpConnectionPool {
    host: String,
    // total_conn: i16,
    // borrowed_conn: i16,
}

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
                    Some(HttpConnection {
                        request_handle: result.0,
                        connection: None,
                        broken: false,
                        join_handle,
                    })
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
