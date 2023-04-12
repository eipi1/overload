use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::error::Error as StdError;
use std::fmt;
use std::fmt::{Debug, Display};
use std::io::Error as StdIoError;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};

use crate::valid_sqlite;
use anyhow::Error as AnyError;
use bytes::{Buf, Bytes};
use csv_async::{AsyncDeserializer, AsyncReaderBuilder};
use futures_core::ready;
use futures_util::{Stream, TryStreamExt};
use http::{Method, Uri};
use log::{error, trace};
use overload_http::HttpReq;
use overload_http::{GenericError, GenericResponse};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio_stream::StreamExt;
use tokio_util::io::StreamReader;
use uuid::Uuid;

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
        Ok(resp) => Ok(file_upload_success_response(file_name, resp)),
        Err(_) => Err(GenericError {
            message: "Unknown error.".to_string(),
            ..Default::default()
        }),
    }
}

pub async fn csv_reader_to_sqlite<R>(
    mut reader: AsyncDeserializer<R>,
    dest_file: String,
) -> anyhow::Result<usize>
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
    // let mut response = GenericResponse::default();
    // response
    //     .data
    //     .insert("valid_count".into(), success.to_string());
    connection.close();
    Ok(success)
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

impl Display for WarpStdIoError {
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

#[doc(hidden)]
/// CSV reader doesn't support map, so read as string and convert to map.
/// This struct acts as intermediate state
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct HttpReqCsvHelper {
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
            body: self.body,
            headers,
        })
    }
}

#[must_use = "streams do nothing unless polled"]
struct WarpByteStream<S> {
    inner: S,
}

impl<S, B> Stream for WarpByteStream<S>
where
    S: Stream<Item = Result<B, warp::Error>> + Unpin + Send + Sync,
    B: Buf + Send + Sync,
{
    type Item = Result<Bytes, WarpStdIoError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match ready!(Pin::new(&mut self.get_mut().inner).poll_next(cx)) {
            None => Poll::Ready(None),
            Some(data) => {
                let data = data
                    .map_err(|inner| WarpStdIoError { inner })
                    .map(|mut b| b.copy_to_bytes(b.remaining()));
                Poll::Ready(Some(data))
            }
        }
    }
}

pub async fn save_sqlite<S, B>(
    http_stream: S,
    data_dir: &str,
) -> Result<GenericResponse<String>, GenericError>
where
    S: Stream<Item = Result<B, warp::Error>> + Unpin + Send + Sync,
    B: Buf + Send + Sync,
{
    fn to_tokio_async_read(r: impl futures::io::AsyncRead) -> impl tokio::io::AsyncRead {
        tokio_util::compat::FuturesAsyncReadCompatExt::compat(r)
    }
    let file_name = Uuid::new_v4().to_string();
    let mut data_file_destination = PathBuf::from(data_dir).join(&file_name);
    data_file_destination.set_extension("sqlite");
    let mut data_file = File::create(&data_file_destination).await.map_err(|e| {
        GenericError::internal_500(&format!(
            "failed to create file: {}, error: {:?}",
            &data_file_destination.to_str().unwrap(),
            e
        ))
    })?;
    let http_stream = WarpByteStream { inner: http_stream };
    let async_read = TryStreamExt::map_err(http_stream, |e| {
        std::io::Error::new(std::io::ErrorKind::Other, e)
    })
    .into_async_read();
    let mut tokio_async_read = to_tokio_async_read(async_read);
    tokio::io::copy(&mut tokio_async_read, &mut data_file).await?;
    let _ = data_file.flush().await;
    let valid_count = valid_sqlite(data_file_destination.to_str().unwrap()).await?;
    Ok(file_upload_success_response(file_name, valid_count))
}

fn file_upload_success_response(file_name: String, valid_count: usize) -> GenericResponse<String> {
    let mut response = GenericResponse::default();
    response.data.insert("file".to_string(), file_name);
    response
        .data
        .insert("valid_count".into(), valid_count.to_string());
    response
}

#[cfg(test)]
mod test {
    use crate::file_uploader::csv_reader_to_sqlite;
    use crate::log_error;
    use csv_async::AsyncReaderBuilder;
    use log::error;
    use std::sync::Once;

    static ONCE: Once = Once::new();

    pub fn setup() {
        ONCE.call_once(|| {
            let _ = tracing_subscriber::fmt()
                .with_env_filter("trace")
                .try_init();
        });
    }

    // #[tokio::test]
    // async fn download_file_from_url() {
    //     setup();
    //     let mock_server = httpmock::MockServer::start_async().await;
    //     let url = url::Url::parse(&mock_server.base_url()).unwrap();
    //
    //     crate::test_utils::generate_sqlite_file("download_file_from_url_requests.sqlite").await;
    //     let mock_uri = format!(
    //         "{}/download_file_from_url_requests.sqlite",
    //         PATH_REQUEST_DATA_FILE_DOWNLOAD
    //     );
    //     info!("mock uri: {}", &mock_uri);
    //
    //     let mut file = File::open("download_file_from_url_requests.sqlite")
    //         .await
    //         .unwrap();
    //     let mut contents = vec![];
    //     use tokio::io::AsyncReadExt;
    //     file.read_to_end(&mut contents).await.unwrap();
    //     println!("len = {}", contents.len());
    //
    //     let _ = mock_server.mock(|when, then| {
    //         when.method(httpmock::Method::GET).path(mock_uri);
    //         then.status(200)
    //             .header("content-type", "application/octet-stream")
    //             .header("Connection", "keep-alive")
    //             .body_from_file("download_file_from_url_requests.sqlite");
    //     });
    //
    //     let mut data_file_destination =
    //         File::create("download_file_from_url_requests.downloaded.sqlite")
    //             .await
    //             .unwrap();
    //     download_file_from_url(
    //         &format!("http://{}:{}", url.host().unwrap(), url.port().unwrap()),
    //         "download_file_from_url_requests.sqlite",
    //         &mut data_file_destination,
    //     )
    //         .await
    //         .unwrap();
    //     data_file_destination.flush().await.unwrap();
    //
    //     let mut file = std::fs::File::open("download_file_from_url_requests.sqlite").unwrap();
    //     let mut sha256 = sha2::Sha256::new();
    //     std::io::copy(&mut file, &mut sha256).unwrap();
    //     let hash_1 = sha256.finalize();
    //
    //     let mut file =
    //         std::fs::File::open("download_file_from_url_requests.downloaded.sqlite").unwrap();
    //     let mut sha256 = sha2::Sha256::new();
    //     std::io::copy(&mut file, &mut sha256).unwrap();
    //     let hash_2 = sha256.finalize();
    //
    //     let _ = tokio::fs::remove_file("download_file_from_url_requests.sqlite").await;
    //     let _ = tokio::fs::remove_file("download_file_from_url_requests.downloaded.sqlite").await;
    //     println!("{:?}", &hash_1);
    //     println!("{:?}", &hash_2);
    //
    //     assert_eq!(hash_1, hash_2);
    // }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_csv_to_sqlite() {
        setup();
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

    // async fn create_csv() {
    //     setup();
    //     let mut wri =
    //         csv_async::AsyncSerializer::from_writer(File::create("test-data.csv").await.unwrap());
    //     let mut headers = HashMap::new();
    //     headers.insert("Authorization".to_string(), "Bearer 123".to_string());
    //     let req = HttpReqCsvHelper {
    //         // id: "".to_string(),
    //         method: "GET".to_string(),
    //         url: "http://httpbin.org/bearer".to_string(),
    //         body: Some("\"hello\",\"world\"".to_string()),
    //         headers: serde_json::to_string(&headers).unwrap(),
    //     };
    //     let result = wri.serialize(req).await;
    //     log_error!(result);
    // }
}
