use crate::executor::cluster::cluster_execute_request_generator;
use crate::executor::execute_request_generator;
use crate::http_util::request::{JobStatusQueryParams, MultiRequest, Request, RequestSpecEnum};
use crate::http_util::{
    csv_stream_to_sqlite, job_id, GenericError, GenericResponse, PATH_FILE_UPLOAD, PATH_JOB_STATUS,
    PATH_STOP_JOB,
};
use crate::metrics::MetricsFactory;
use crate::{data_dir_path, ErrorCode, JobStatus, Response, PATH_REQUEST_DATA_FILE_DOWNLOAD};
use bytes::Buf;
use cluster_mode::{Cluster, RestClusterNode};
use futures_core::future::BoxFuture;
use futures_core::Stream;
use futures_util::{FutureExt, StreamExt, TryStreamExt};
use http::header::CONTENT_LENGTH;
use hyper::client::HttpConnector;
use hyper::{Body, Client};
use log::{error, trace};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashSet;
use std::io;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tracing::debug;
use url::Url;

type GenericResult<T> = Result<GenericResponse<T>, GenericError>;

pub async fn handle_request_cluster(request: Request, cluster: Arc<Cluster>) -> Response {
    let job_id = job_id(&request.name);
    let buckets = request.histogram_buckets.clone();
    if !cluster.is_active().await {
        Response::new(job_id, JobStatus::Error(ErrorCode::InactiveCluster))
    } else if cluster.is_primary().await {
        let generator = request.into();
        tokio::spawn(cluster_execute_request_generator(
            generator,
            job_id.clone(),
            cluster,
            buckets,
        ));
        Response::new(job_id, JobStatus::Starting)
    } else {
        //forward request to primary
        let client = Client::new();
        let job_id = request
            .name
            .clone()
            .unwrap_or_else(|| "unknown_job".to_string());
        match forward_test_request("test", request, cluster, client).await {
            Ok(resp) => resp,
            Err(err) => {
                error!("{}", err);
                unknown_error_resp(job_id)
            }
        }
    }
}

pub async fn handle_multi_request_cluster(
    request: MultiRequest,
    cluster: Arc<Cluster>,
) -> Response {
    let job_id = job_id(&request.name);
    if !cluster.is_active().await {
        Response::new(job_id, JobStatus::Error(ErrorCode::InactiveCluster))
    } else if cluster.is_primary().await {
        for (idx, request) in request.requests.into_iter().enumerate() {
            let job_id_for_req = format!("{}_{}", &job_id, idx);
            let buckets = request.histogram_buckets.clone();
            let generator = request.into();
            tokio::spawn(cluster_execute_request_generator(
                generator,
                job_id_for_req,
                cluster.clone(),
                buckets,
            ));
        }
        Response::new(job_id, JobStatus::Starting)
    } else {
        //forward request to primary
        let client = Client::new();
        let job_id = request
            .name
            .clone()
            .unwrap_or_else(|| "unknown_job".to_string());
        match forward_test_request("tests", request, cluster, client).await {
            Ok(resp) => resp,
            Err(err) => {
                error!("{}", err);
                unknown_error_resp(job_id)
            }
        }
    }
}

pub async fn handle_request_from_primary(
    request: Request,
    metrics: &'static MetricsFactory,
    cluster: Arc<Cluster>,
) -> Response {
    let job_id = job_id(&request.name);
    let buckets = request.histogram_buckets.clone();
    let init = prepare(&request, cluster.clone());
    let generator = request.into();

    tokio::spawn(execute_request_generator(
        generator,
        job_id.clone(),
        metrics
            .metrics_with_buckets(buckets.to_vec(), &*job_id)
            .await,
        init,
    ));
    Response::new(job_id, JobStatus::Starting)
}

fn prepare(
    request: &Request,
    cluster: Arc<Cluster>,
) -> BoxFuture<'static, Result<(), anyhow::Error>> {
    initiator_for_request_from_secondary(request, cluster)
}

async fn forward_test_request<T: Serialize>(
    path: &str,
    request: T,
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
        trace!(
            "forwarding request to primary: {}, {}",
            &uri,
            serde_json::to_string(&request).unwrap()
        );
        let req = hyper::Request::builder()
            .uri(format!("{}/{}", &uri, path))
            .method("POST")
            .body(Body::from(serde_json::to_string(&request)?))?;
        let resp = client.request(req).await?;
        let bytes = hyper::body::to_bytes(resp.into_body()).await?;
        let resp = serde_json::from_slice::<Response>(bytes.as_ref())?;
        return Ok(resp);
    };
    Ok(unknown_error_resp("error".to_string()))
}

pub async fn handle_history_all(
    params: JobStatusQueryParams,
    cluster: Arc<Cluster>,
) -> GenericResult<JobStatus> {
    if !cluster.is_active().await {
        Err(inactive_cluster_error())
    } else if cluster.is_primary().await {
        super::standalone::handle_history_all(params).await
    } else {
        // forward request to primary
        let client = Client::new();
        if let Ok(uri) = primary_uri(&cluster.primaries().await).await {
            let request = hyper::Request::builder()
                .uri(format!("{}{}?{}", &uri, PATH_JOB_STATUS, params))
                .method("GET")
                .body(Body::empty())?;
            forward_other_requests_to_primary::<JobStatus>(request, cluster, client).await
        } else {
            Err(GenericError::internal_500("Unknown error"))
        }
    }
}

pub async fn stop(job_id: String, cluster: Arc<Cluster>) -> GenericResult<String> {
    if !cluster.is_active().await {
        Err(inactive_cluster_error())
    } else if cluster.is_primary().await {
        super::standalone::stop(job_id).await
    } else {
        // forward request to primary
        let client = Client::new();
        if let Ok(uri) = primary_uri(&cluster.primaries().await).await {
            let request = hyper::Request::builder()
                .uri(format!("{}{}/{}", &uri, PATH_STOP_JOB, &job_id))
                .method("GET")
                .body(Body::empty())?;
            forward_other_requests_to_primary::<String>(request, cluster, client).await
        } else {
            Err(GenericError::internal_500("Unknown error"))
        }
    }
}

async fn primary_uri(
    primaries: &Option<HashSet<RestClusterNode>>,
) -> Result<&String, GenericError> {
    primaries
        .as_ref()
        .and_then(|s| s.iter().next())
        .map(|p| p.service_instance())
        .and_then(|si| si.uri().as_ref())
        .ok_or_else(|| GenericError::internal_500("No primary or invalid ServiceInstance URI"))
}

async fn forward_other_requests_to_primary<T: Serialize + DeserializeOwned>(
    req: http::Request<Body>,
    _cluster: Arc<Cluster>,
    client: Client<HttpConnector>,
) -> GenericResult<T> {
    trace!("forwarding request to primary: {}", &req.uri());
    let resp = client.request(req).await?;
    let bytes = hyper::body::to_bytes(resp.into_body()).await?.reader();
    let resp: GenericResponse<T> = serde_json::from_reader(bytes)?;
    Ok(resp)
}

pub(crate) fn initiator_for_request_from_secondary(
    request: &Request,
    cluster: Arc<Cluster>,
) -> BoxFuture<'static, Result<(), anyhow::Error>> {
    return match &request.req {
        RequestSpecEnum::RequestFile(req) => {
            download_request_file_from_primary(format!("{}.sqlite", &req.file_name), cluster)
                .boxed()
        }
        _ => crate::http_util::noop().boxed(),
    };
}

async fn download_request_file_from_primary(
    filename: String,
    cluster: Arc<Cluster>,
) -> Result<(), anyhow::Error> {
    let file_path = data_dir_path().join(&filename);
    debug!(
        "[download_request_file_from_primary] - downloading file {:?} from primary",
        &file_path
    );
    if let Ok(true) = file_path.try_exists() {
        debug!("file {:?} already exists", &file_path);
        return Ok(());
    }
    if let Some(primaries) = cluster.primaries().await {
        if let Some(node) = primaries.iter().next() {
            let mut data_file_destination = File::create(&file_path).await.map_err(|e| {
                error!("[download_request_file_from_primary] - error: {:?}", e);
                anyhow::anyhow!("filed to create file {:?}", &file_path)
            })?;
            let primary_uri = node
                .service_instance()
                .uri()
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Invalid ServiceInstance URI"))?;
            debug!(
                "downloading file {:?} from primary: {}",
                &filename, &primary_uri
            );
            download_file_from_url(primary_uri, &filename, &mut data_file_destination).await?
        }
    }
    Ok(())
}

pub(crate) async fn download_file_from_url(
    uri: &str,
    filename: &str,
    data_file_destination: &mut File,
) -> Result<(), anyhow::Error> {
    let url = format!("{}{}/{}", &uri, PATH_REQUEST_DATA_FILE_DOWNLOAD, filename);
    debug!("[download_file_from_url] - downloading file from {}", &url);
    let req = hyper::Request::builder()
        .uri(&url)
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

pub async fn handle_file_upload<S, B>(
    data: S,
    data_dir: &str,
    cluster: Arc<Cluster>,
    content_len: u64,
) -> GenericResult<String>
where
    S: Stream<Item = Result<B, warp::Error>> + Unpin + Send + Sync + 'static,
    B: Buf + Send + Sync,
{
    if !cluster.is_active().await {
        Err(inactive_cluster_error())
    } else if cluster.is_primary().await {
        debug!("[handle_file_upload] primary node, saving the file");
        csv_stream_to_sqlite(data, data_dir).await
    } else {
        // forward request to primary
        let client = Client::new();
        if let Ok(uri) = primary_uri(&cluster.primaries().await).await {
            let url = Url::parse(uri)
                .and_then(|url| url.join(PATH_FILE_UPLOAD))
                .unwrap();
            debug!(
                "[handle_file_upload] secondary node, uploading file to {}",
                &url
            );
            let stream = convert_stream(data);
            let request = hyper::Request::builder()
                .uri(url.to_string())
                .method("POST")
                .header(CONTENT_LENGTH, content_len)
                .body(Body::from(stream))?;
            let resp = client.request(request).await?;
            let bytes = hyper::body::to_bytes(resp.into_body()).await?.reader();
            let resp: GenericResponse<String> = serde_json::from_reader(bytes)?;
            Ok(resp)
        } else {
            Err(GenericError::internal_500("Unknown error"))
        }
    }
}

fn convert_stream<S, B>(
    data: S,
) -> Box<dyn Stream<Item = Result<bytes::Bytes, Box<(dyn std::error::Error + Send + Sync)>>> + Send>
where
    S: Stream<Item = Result<B, warp::Error>> + Unpin + Send + Sync + 'static,
    B: Buf + Send + Sync,
{
    let stream = data.map(move |x| {
        x.map(|mut y| y.copy_to_bytes(y.remaining()))
            .map_err(|e| Box::new(e) as _)
    });
    Box::new(stream)
}

fn inactive_cluster_error() -> GenericError {
    GenericError {
        error_code: 404,
        message: {
            let status = JobStatus::Error(ErrorCode::InactiveCluster);
            serde_json::to_string(&status).unwrap_or_default()
        },
        data: Default::default(),
    }
}

fn unknown_error_resp(job_id: String) -> Response {
    Response::new(job_id, JobStatus::Error(ErrorCode::Others))
}

#[cfg(test)]
mod test {
    use crate::PATH_REQUEST_DATA_FILE_DOWNLOAD;
    use sha2::Digest;
    use std::sync::Once;
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;
    use tracing::info;

    static ONCE: Once = Once::new();

    pub fn setup() {
        ONCE.call_once(|| {
            let _ = tracing_subscriber::fmt()
                .with_env_filter("trace")
                .try_init();
        });
    }

    #[tokio::test]
    async fn download_file_from_url() {
        setup();
        let mock_server = httpmock::MockServer::start_async().await;
        let url = url::Url::parse(&mock_server.base_url()).unwrap();

        crate::test_utils::generate_sqlite_file("download_file_from_url_requests.sqlite").await;
        let mock_uri = format!(
            "{}/download_file_from_url_requests.sqlite",
            PATH_REQUEST_DATA_FILE_DOWNLOAD
        );
        info!("mock uri: {}", &mock_uri);

        let mut file = File::open("download_file_from_url_requests.sqlite")
            .await
            .unwrap();
        let mut contents = vec![];
        use tokio::io::AsyncReadExt;
        file.read_to_end(&mut contents).await.unwrap();
        println!("len = {}", contents.len());

        let _ = mock_server.mock(|when, then| {
            when.method(httpmock::Method::GET).path(mock_uri);
            then.status(200)
                .header("content-type", "application/octet-stream")
                .header("Connection", "keep-alive")
                .body_from_file("download_file_from_url_requests.sqlite");
        });

        let mut data_file_destination =
            File::create("download_file_from_url_requests.downloaded.sqlite")
                .await
                .unwrap();
        super::download_file_from_url(
            &format!("http://{}:{}", url.host().unwrap(), url.port().unwrap()),
            "download_file_from_url_requests.sqlite",
            &mut data_file_destination,
        )
        .await
        .unwrap();
        data_file_destination.flush().await.unwrap();

        let mut file = std::fs::File::open("download_file_from_url_requests.sqlite").unwrap();
        let mut sha256 = sha2::Sha256::new();
        std::io::copy(&mut file, &mut sha256).unwrap();
        let hash_1 = sha256.finalize();

        let mut file =
            std::fs::File::open("download_file_from_url_requests.downloaded.sqlite").unwrap();
        let mut sha256 = sha2::Sha256::new();
        std::io::copy(&mut file, &mut sha256).unwrap();
        let hash_2 = sha256.finalize();

        let _ = tokio::fs::remove_file("download_file_from_url_requests.sqlite").await;
        let _ = tokio::fs::remove_file("download_file_from_url_requests.downloaded.sqlite").await;
        println!("{:?}", &hash_1);
        println!("{:?}", &hash_2);

        assert_eq!(hash_1, hash_2);
    }
}
