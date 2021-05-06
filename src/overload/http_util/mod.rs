pub mod request;

#[cfg(feature = "cluster")]
use crate::executor::cluster;
use crate::executor::{execute_request_generator, get_job_status, send_stop_signal};
use crate::http_util::request::Request;
#[cfg(feature = "cluster")]
use crate::ErrorCode;
use crate::{HttpReq, JobStatus, Response};
use bytes::Buf;
#[cfg(feature = "cluster")]
use cluster_mode::Cluster;
use csv_async::{AsyncDeserializer, AsyncReaderBuilder};
use futures_core::ready;
use futures_util::Stream;
use http::{Method, Uri};
#[cfg(feature = "cluster")]
use hyper::client::HttpConnector;
#[cfg(feature = "cluster")]
use hyper::{Body, Client};
#[allow(unused_imports)]
use log::{error, trace};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::ConnectOptions;
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::io::Error as StdIoError;
use std::io::ErrorKind;
use std::pin::Pin;
use std::str::FromStr;
#[cfg(feature = "cluster")]
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::AsyncRead;
use tokio_stream::StreamExt;
use tokio_util::io::StreamReader;
use uuid::Uuid;
use std::convert::{TryInto, TryFrom};
use anyhow::Error as AnyError;

pub async fn handle_request(request: Request) -> Response {
    let job_id = job_id(&request);
    let generator = request.into();

    tokio::spawn(execute_request_generator(generator, job_id.clone()));
    Response::new(job_id, JobStatus::Starting)
}

#[cfg(feature = "cluster")]
pub async fn handle_request_cluster(request: Request, cluster: Arc<Cluster>) -> Response {
    let job_id = job_id(&request);
    if !cluster.is_active().await {
        Response::new(job_id, JobStatus::Error(ErrorCode::InactiveCluster))
    } else if cluster.is_primary().await {
        let generator = request.into();
        tokio::spawn(cluster::cluster_execute_request_generator(
            generator,
            job_id.clone(),
            cluster,
        ));
        Response::new(job_id, JobStatus::Starting)
    } else {
        //forward request to primary
        let client = Client::new();
        let job_id = request
            .name
            .clone()
            .unwrap_or_else(|| "unknown_job".to_string());
        match forward_test_request(request, cluster, client).await {
            Ok(resp) => resp,
            Err(err) => {
                error!("{}", err);
                unknown_error_resp(job_id)
            }
        }
    }
}

#[cfg(feature = "cluster")]
async fn forward_test_request(
    request: Request,
    cluster: Arc<Cluster>,
    client: Client<HttpConnector>,
) -> anyhow::Result<Response> {
    let primaries = cluster
        .primaries()
        .await
        .ok_or_else(|| anyhow::anyhow!("Primary returns None"))?;
    if let Some(primary) = primaries.iter().next() {
        let uri = primary
            .service_instance()
            .uri()
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Invalid ServiceInstance URI"))?;
        trace!("forwarding request to primary: {}", &uri);
        let req = hyper::Request::builder()
            .uri(format!("{}/test", &uri))
            .method("POST")
            .body(Body::from(serde_json::to_string(&request)?))?;
        let resp = client.request(req).await?;
        let bytes = hyper::body::to_bytes(resp.into_body()).await?;
        let resp = serde_json::from_slice::<Response>(bytes.as_ref())?;
        return Ok(resp);
    };
    Ok(unknown_error_resp(
        request.name.unwrap_or_else(|| "unknown_job".to_string()),
    ))
}

#[cfg(feature = "cluster")]
fn unknown_error_resp(job_id: String) -> Response {
    Response::new(job_id, JobStatus::Error(ErrorCode::Others))
}

//todo verify for cluster mode. using job id as name for secondary request
fn job_id(request: &Request) -> String {
    request
        .name
        .clone()
        .map_or(Uuid::new_v4().to_string(), |n| {
            let uuid = Regex::new(
                r"\b[0-9a-f]{8}\b-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-\b[0-9a-f]{12}\b$",
            )
            .unwrap();
            if uuid.is_match(&n) {
                n
            } else {
                let mut name = n;
                name.push('-');
                name.push_str(Uuid::new_v4().to_string().as_str());
                name
            }
        })
}

pub async fn stop(job_id: String) -> TestJobResponse {
    let result = send_stop_signal(job_id.clone()).await;
    TestJobResponse {
        job_id,
        message: result,
    }
}

pub async fn handle_history_all(
    offset: usize,
    limit: usize,
) -> HashMap<String, JobStatus, RandomState> {
    get_job_status(offset, limit).await
}

pub async fn csv_stream_to_sqlite<S, B>(
    http_stream: S,
    data_dir: &str,
) -> Result<GenericResponse, GenericError>
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

async fn csv_reader_to_sqlite<R>(
    mut reader: AsyncDeserializer<R>,
    dest_file: String,
) -> anyhow::Result<GenericResponse>
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
            body TEXT,
            headers TEXT
           );"#,
    )
    .execute(&mut connection)
    .await?;
    let mut success:usize = 0;
    while let Some(req) = reader.next().await {
        match req {
            Ok(csv_req) => {
                trace!("{:?}", &csv_req);
                //todo use TryInto
                let req: Result<HttpReq, AnyError> = csv_req.try_into();
                match req {
                    Ok(req) => {
                        success+=1;
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
    response.data.insert("valid_count".into(),success.to_string());
    Ok(response)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestJobResponse {
    job_id: String,
    message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenericResponse {
    #[serde(flatten)]
    pub data: HashMap<String, String>,
}

impl Default for GenericResponse {
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
        let headers = serde_json::from_str::<HashMap<String, String>>(&self.headers.as_str())?;
        Ok(HttpReq{
            id: "".to_string(),
            method,
            url: self.url,
            body: self.body.map(|s| s.into_bytes()),
            headers
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
        // let opt_item = ready!(Pin::new(&mut self.get_mut().body).poll_next(cx));

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

#[cfg(test)]
mod test {
    use crate::http_util::{csv_reader_to_sqlite, HttpReqCsvHelper};
    use crate::log_error;
    use csv_async::AsyncReaderBuilder;
    use log::error;
    use std::collections::HashMap;
    use std::sync::Once;
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
        setup();
        let reader = AsyncReaderBuilder::new()
            .escape(Some(b'\\'))
            .create_deserializer(File::open("requests.csv").await.unwrap());
        let to_sqlite = csv_reader_to_sqlite(reader, "requests.sqlite".to_string()).await;
        log_error!(to_sqlite);
    }

    #[tokio::test]
    #[ignore]
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
}
