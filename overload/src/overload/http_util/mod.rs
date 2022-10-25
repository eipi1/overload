#[cfg(feature = "cluster")]
pub mod cluster;
#[cfg(feature = "cluster")]
pub use self::cluster::handle_multi_request_cluster;
#[cfg(feature = "cluster")]
pub use self::cluster::handle_request_cluster;
#[cfg(feature = "cluster")]
pub use self::cluster::handle_request_from_primary;

pub mod request;
mod standalone;

#[cfg(feature = "cluster")]
pub use cluster::handle_history_all;
#[cfg(not(feature = "cluster"))]
pub use standalone::handle_history_all;

#[cfg(feature = "cluster")]
pub use cluster::stop;
#[cfg(not(feature = "cluster"))]
pub use standalone::stop;

use crate::executor::execute_request_generator;
use crate::http_util::request::{MultiRequest, Request};
use crate::metrics::MetricsFactory;
use crate::{HttpReq, JobStatus, Response};
use anyhow::Error as AnyError;
use bytes::Buf;
use csv_async::{AsyncDeserializer, AsyncReaderBuilder};
use futures_core::ready;
use futures_util::{FutureExt, Stream};
use http::{Method, Uri};
use log::{error, trace};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};
use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::error::Error as StdError;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::io::Error as StdIoError;
use std::io::ErrorKind;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};
use tokio::io::AsyncRead;
use tokio_stream::StreamExt;
use tokio_util::io::StreamReader;
use uuid::Uuid;

pub async fn handle_request(request: Request, metrics: &'static MetricsFactory) -> Response {
    let job_id = job_id(&request.name);
    let buckets = request.histogram_buckets.clone();
    let generator = request.into();

    tokio::spawn(execute_request_generator(
        generator,
        job_id.clone(),
        metrics
            .metrics_with_buckets(buckets.to_vec(), &*job_id)
            .await,
        noop().boxed(),
    ));
    Response::new(job_id, JobStatus::Starting)
}

pub async fn handle_multi_request(
    requests: MultiRequest,
    metrics: &'static MetricsFactory,
) -> Response {
    let job_id = job_id(&requests.name);

    for (idx, request) in requests.requests.into_iter().enumerate() {
        let job_id_for_req = format!("{}_{}", &job_id, idx);
        let buckets = request.histogram_buckets.clone();
        let generator = request.into();
        let metrics_instance = metrics
            .metrics_with_buckets(buckets.to_vec(), &*job_id_for_req)
            .await;

        tokio::spawn(execute_request_generator(
            generator,
            job_id_for_req,
            metrics_instance,
            noop().boxed(),
        ));
    }
    Response::new(job_id, JobStatus::Starting)
}

pub async fn noop() -> Result<(), anyhow::Error> {
    Ok(())
}

//todo verify for cluster mode. using job id as name for secondary request
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

pub async fn csv_stream_to_sqlite<S, B>(
    http_stream: S,
    data_dir: &str,
) -> Result<GenericResponse<String>, GenericError>
where
    S: Stream<Item = Result<B, warp::Error>> + Unpin + Send + Sync,
    B: Buf + Send + Sync,
{
    let stream = WarpStream { inner: http_stream };
    let reader = StreamReader::new(stream);
    let reader = AsyncReaderBuilder::new()
        .escape(Some(b'\\'))
        .create_deserializer(reader);
    let file_name = Uuid::new_v4().to_string();
    let result = csv_reader_to_sqlite(reader, format!("{}/{}.sqlite", data_dir, file_name)).await;
    match result {
        Ok(mut resp) => {
            resp.data.insert("file".to_string(), file_name);
            Ok(resp)
        }
        Err(_) => Err(GenericError {
            message: "Unknown error.".to_string(),
            ..Default::default()
        }),
    }
}

pub async fn csv_reader_to_sqlite<R>(
    mut reader: AsyncDeserializer<R>,
    dest_file: String,
) -> anyhow::Result<GenericResponse<String>>
where
    R: AsyncRead + Unpin + Send + Sync,
{
    let mut reader = reader.deserialize::<HttpReqCsvHelper>();
    let mut connection =
        SqliteConnectOptions::from_str(format!("sqlite://{}", dest_file).as_str())?
            .create_if_missing(true)
            .connect()
            .await?;
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS http_req (
            url TEXT NOT NULL,
            method TEXT NOT NULL,
            body BLOB,
            headers TEXT
           );"#,
    )
    .execute(&mut connection)
    .await?;
    let mut success: usize = 0;
    while let Some(req) = reader.next().await {
        match req {
            Ok(csv_req) => {
                trace!("csv row: {:?}", &csv_req);
                //todo use TryInto
                let req: Result<HttpReq, AnyError> = csv_req.try_into();
                match req {
                    Ok(req) => {
                        success += 1;
                        sqlx::query("insert into http_req values(?,?,?,?)")
                            .bind(&req.url)
                            .bind(&req.method.to_string())
                            .bind(&req.body)
                            .bind(serde_json::to_string(&req.headers)?)
                            .execute(&mut connection)
                            .await?;
                    }
                    Err(e) => {
                        error!("failed to parse data: {}", e);
                    }
                }
            }
            Err(e) => {
                error!("error while reading csv: {:?}", &e)
            }
        }
    }
    let mut response = GenericResponse::default();
    response
        .data
        .insert("valid_count".into(), success.to_string());
    connection.close();
    Ok(response)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestJobResponse {
    job_id: String,
    message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenericResponse<T: Serialize> {
    #[serde(flatten)]
    pub data: HashMap<String, T>,
}

impl<T: Serialize> Default for GenericResponse<T> {
    fn default() -> Self {
        GenericResponse {
            data: HashMap::with_capacity(2),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenericError {
    pub error_code: u16,
    pub message: String,
    #[serde(flatten)]
    pub data: HashMap<String, String>,
}

impl GenericError {
    pub fn internal_500(msg: &str) -> GenericError {
        GenericError {
            error_code: 500,
            message: msg.to_string(),
            ..Default::default()
        }
    }

    pub fn error_unknown() -> GenericError {
        Self::internal_500("Unknown error")
    }

    pub fn new(msg: &str, code: u16) -> Self {
        Self {
            error_code: code,
            message: msg.to_string(),
            ..Default::default()
        }
    }

    pub fn from_error<E: StdError>(code: u16, err: E) -> GenericError {
        Self::new(&*err.to_string(), code)
    }
}

impl From<http::Error> for GenericError {
    fn from(e: http::Error) -> Self {
        GenericError {
            error_code: 400,
            message: e.to_string(),
            ..Default::default()
        }
    }
}

macro_rules! from_error {
    ($t:ty) => {
        impl From<$t> for GenericError {
            fn from(e: $t) -> Self {
                GenericError {
                    error_code: 500,
                    message: e.to_string(),
                    ..Default::default()
                }
            }
        }
    };
}

from_error!(hyper::Error);
from_error!(AnyError);
from_error!(serde_json::Error);
from_error!(StdIoError);

impl Default for GenericError {
    fn default() -> Self {
        GenericError {
            error_code: u16::MAX,
            message: String::new(),
            data: HashMap::new(),
        }
    }
}

impl Display for GenericError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({}, {}, {})",
            self.error_code,
            self.message,
            self.data.len()
        )
    }
}

impl StdError for GenericError {}

#[doc(hidden)]
/// CSV reader doesn't support map, so read as string and convert to map.
/// This struct acts as intermediate state
#[derive(Debug, Serialize, Deserialize)]
struct HttpReqCsvHelper {
    pub method: String,
    pub url: String,
    pub body: Option<String>,
    pub headers: String,
}

#[allow(clippy::from_over_into)]
impl TryInto<HttpReq> for HttpReqCsvHelper {
    type Error = AnyError;

    fn try_into(self) -> Result<HttpReq, Self::Error> {
        Uri::try_from(&self.url)?;
        let method = Method::try_from(self.method.as_str())?;
        let headers = serde_json::from_str::<HashMap<String, String>>(self.headers.as_str())?;
        Ok(HttpReq {
            id: "".to_string(),
            method,
            url: self.url,
            body: self.body.map(|s| s.into_bytes()),
            headers,
        })
    }
}

#[must_use = "streams do nothing unless polled"]
struct WarpStream<S> {
    inner: S,
}

impl<S, B> Stream for WarpStream<S>
where
    S: Stream<Item = Result<B, warp::Error>> + Unpin + Send + Sync,
    B: Buf + Send + Sync,
{
    type Item = Result<B, WarpStdIoError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match ready!(Pin::new(&mut self.get_mut().inner).poll_next(cx)) {
            None => Poll::Ready(None),
            Some(data) => {
                let data = data.map_err(|inner| WarpStdIoError { inner });
                Poll::Ready(Some(data))
            }
        }
    }
}

#[derive(Debug)]
struct WarpStdIoError {
    inner: warp::Error,
}

impl fmt::Display for WarpStdIoError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.inner, f)
    }
}

impl StdError for WarpStdIoError {}

#[allow(clippy::from_over_into)]
impl Into<StdIoError> for WarpStdIoError {
    fn into(self) -> StdIoError {
        std::io::Error::new(ErrorKind::Other, self.inner.to_string())
    }
}

pub const PATH_JOB_STATUS: &str = "/test/status";
pub const PATH_STOP_JOB: &str = "/test/stop";
pub const PATH_FILE_UPLOAD: &str = "/test/requests-bin";

#[cfg(test)]
#[allow(dead_code)]
mod test {
    use crate::http_util::{csv_reader_to_sqlite, job_id, HttpReqCsvHelper};
    use crate::log_error;
    use csv_async::AsyncReaderBuilder;
    use log::error;
    use std::collections::HashMap;
    use std::sync::Once;
    use test_case::test_case;
    use tokio::fs::File;

    static ONCE: Once = Once::new();

    fn setup() {
        ONCE.call_once(|| {
            tracing_subscriber::fmt()
                .with_env_filter("trace")
                .try_init()
                .unwrap();
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_csv_to_sqlite() {
        // setup();
        let csv_data = r#""url","method","body","headers"
"http://httpbin.org/anything/11","GET","","{}"
"http://httpbin.org/anything/13","GET","","{}"
"http://httpbin.org/anything","POST","{\"some\":\"random data\",\"second-key\":\"more data\"}","{\"Authorization\":\"Bearer 123\"}"
"http://httpbin.org/bearer","GET","","{\"Authorization\":\"Bearer 123\"}"
"#;
        let reader = AsyncReaderBuilder::new()
            .escape(Some(b'\\'))
            .create_deserializer(csv_data.as_bytes());
        let to_sqlite = csv_reader_to_sqlite(reader, "requests.sqlite".to_string()).await;
        log_error!(to_sqlite);
        let _ = tokio::fs::remove_file("requests.sqlite").await;
    }

    async fn create_csv() {
        setup();
        let mut wri =
            csv_async::AsyncSerializer::from_writer(File::create("test-data.csv").await.unwrap());
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer 123".to_string());
        let req = HttpReqCsvHelper {
            // id: "".to_string(),
            method: "GET".to_string(),
            url: "http://httpbin.org/bearer".to_string(),
            body: Some("\"hello\",\"world\"".to_string()),
            headers: serde_json::to_string(&headers).unwrap(),
        };
        let result = wri.serialize(req).await;
        log_error!(result);
    }

    #[test_case("multi-req-7e19ae6b-59af-4916-8cc0-a04cc48a739b", "multi-req-7e19ae6b-59af-4916-8cc0-a04cc48a739b" ; "job id without sub test id")]
    #[test_case("multi-req-7e19ae6b-59af-4916-8cc0-a04cc48a739b_0", "multi-req-7e19ae6b-59af-4916-8cc0-a04cc48a739b_0" ; "job id with sub test id")]
    fn test_job_id(name: &str, expect: &str) {
        let option = Some(name.to_string());
        assert_eq!(job_id(&option), expect.to_string());
    }
}
