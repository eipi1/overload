use crate::HttpReq;
use async_trait::async_trait;
use bb8::ManageConnection;
use bb8::PoolCustomizer;
use hyper::client::conn;
use hyper::client::conn::{Connection, SendRequest};
use hyper::Body;
use log::{trace, warn};
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::net::TcpStream;

#[derive(Debug)]
struct ConcurrentConnectionCountManager {
    number_of_connection: AtomicU32,
}

impl ConcurrentConnectionCountManager {
    pub fn new(conn_count: u32) -> ConcurrentConnectionCountManager {
        ConcurrentConnectionCountManager{
            number_of_connection: AtomicU32::new(conn_count),
        }
    }

    pub fn update (&self, conn_count: u32) {
        trace!("updating pool connection count to: {}", &conn_count);
        let _ = &self.number_of_connection.store(conn_count, Ordering::Relaxed);
    }
}

impl Default for ConcurrentConnectionCountManager {
    /// at least one initial min size is required.
    /// If initial size is set to 0, updating the value later is not updating
    /// min pool size, for example following test case fails -
    /// ```rust
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
            number_of_connection: AtomicU32::new(1),
        }
    }
}

impl PoolCustomizer for ConcurrentConnectionCountManager {
    fn min_idle(&self) -> u32 {
        let min = self.number_of_connection.load(Ordering::Relaxed);
        trace!("Cheking min idle: {}", min);
        min
    }
    fn max_size(&self) -> u32 {
        let max= self.number_of_connection.load(Ordering::Relaxed);
        trace!("Cheking min idle: {}", max);
        max
    }
}

struct HttpConnectionPool {
    host: String,
    total_conn: i16,
    borrowed_conn: i16,
}

impl HttpConnectionPool {
    /// host and port in format: `host-name:port`
    fn new(host_port: &str) -> HttpConnectionPool {
        HttpConnectionPool {
            host: host_port.to_string(),
            total_conn: 0,
            borrowed_conn: 0,
        }
    }
}

#[async_trait]
impl ManageConnection for HttpConnectionPool {
    type Connection = HttpConnection;
    type Error = ();

    async fn connect(&self) -> Result<Self::Connection, Self::Error> {
        open_connection(&self.host).await.ok_or(())
    }

    async fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Self::Error> {
        Ok(())
    }

    fn has_broken(&self, conn: &mut Self::Connection) -> bool {
        conn.broken
    }
}

struct HttpConnection {
    connection: Connection<TcpStream, Body>,
    request_handle: SendRequest<Body>,
    broken: bool,
}

impl HttpConnection {}

async fn open_connection(host_port: &str) -> Option<HttpConnection> {
    // req.url
    //todo validate url on request
    // let url = url::Url::parse(&req.url).ok()?;
    // let host = url.host_str()?;
    // let port = url.port_or_known_default()?;
    // let host_port = format!("{}:{}", host, port);
    trace!("Opening connection to: {}", host_port);
    // let result = TcpStream::connect(host_port).await;
    let tcp_stream = TcpStream::connect(host_port).await;
    match tcp_stream {
        Ok(stream) => {
            trace!("handshaking with: {}", host_port);
            match conn::handshake(stream).await {
                Ok(result) => Some(HttpConnection {
                    request_handle: result.0,
                    connection: result.1,
                    broken: false,
                }),
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

#[cfg(test)]
mod test {
    use crate::executor::connection::HttpConnectionPool;
    use bb8::ManageConnection;
    use lazy_static::lazy_static;
    use std::sync::Once;
    use wiremock::MockServer;
    use super::ConcurrentConnectionCountManager;
    use std::sync::Arc;
    use bb8::PoolCustomizer;

    // lazy_static! {
    //     static ref WIRE_MOCK: MockServer = tokio::runtime::Runtime::new()
    //         .unwrap()
    //         .block_on(async { wiremock::MockServer::start().await });
    // }

    static ONCE: Once = Once::new();

    fn setup() {
        ONCE.call_once(|| {
            tracing_subscriber::fmt()
                .with_env_filter("trace")
                .try_init()
                .unwrap();
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

        let pool = bb8::Pool::builder()
            .pool_customizer(customizer_dyn)
            .build(pool)
            .await
            .unwrap();
        assert_eq!(pool.state().connections,1);
        customizer.update(10);
        let _ = pool.get().await;
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        assert_eq!(pool.state().connections, 10);

    }
}
