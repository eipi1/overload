use crate::HttpReq;
use async_trait::async_trait;
use bb8::ManageConnection;
use hyper::client::conn;
use hyper::client::conn::{Connection, SendRequest};
use hyper::Body;
use log::{trace, warn};
use tokio::net::TcpStream;
use url::quirks::host;

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
    use std::sync::Once;
    use bb8::ManageConnection;
    use lazy_static::lazy_static;
    use wiremock::MockServer;
    use crate::executor::connection::HttpConnectionPool;

    lazy_static! {
        static ref WIRE_MOCK: MockServer = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { wiremock::MockServer::start().await });
    }

    static ONCE: Once = Once::new();

    fn setup() {
        ONCE.call_once(|| {
            tracing_subscriber::fmt()
                .with_env_filter("trace")
                .try_init()
                .unwrap();
        });
    }


    #[test]
    fn test_pool() {
        setup();
        let wire_mock_uri = WIRE_MOCK.uri();
        let url = url::Url::parse(&wire_mock_uri).unwrap();
        let host_port = format!("{}:{}", url.host_str().unwrap(), url.port().unwrap());
        let pool = HttpConnectionPool::new(&host_port);
        let pool = bb8::Pool::builder().build(pool).await.unwrap();



        println!("{}", string);
    }
}
