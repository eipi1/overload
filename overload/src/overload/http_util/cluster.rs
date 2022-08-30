use crate::executor::cluster::cluster_execute_request_generator;
use crate::executor::execute_request_generator;
use crate::http_util::request::{JobStatusQueryParams, Request, RequestSpecEnum};
use crate::http_util::{job_id, GenericError, GenericResponse, PATH_JOB_STATUS, PATH_STOP_JOB};
use crate::metrics::MetricsFactory;
use crate::{data_dir, ErrorCode, JobStatus, Response, PATH_REQUEST_DATA_FILE_DOWNLOAD};
use bytes::Buf;
use cluster_mode::{Cluster, RestClusterNode};
use futures_core::future::BoxFuture;
use futures_util::{FutureExt, TryStreamExt};
use hyper::client::HttpConnector;
use hyper::{Body, Client};
use log::{error, trace};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::Arc;
use tokio::fs::File;

type GenericResult<T> = Result<GenericResponse<T>, GenericError>;

pub async fn handle_request_cluster(request: Request, cluster: Arc<Cluster>) -> Response {
    let job_id = job_id(&request);
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
        match forward_test_request(request, cluster, client).await {
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
    let job_id = job_id(&request);
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
        trace!(
            "forwarding request to primary: {}, {}",
            &uri,
            serde_json::to_string(&request).unwrap()
        );
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

async fn forward_other_requests_to_primary<'d, T: Serialize + DeserializeOwned>(
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
    let file_path = format!("{}/{}", data_dir(), &filename);
    if let Ok(true) = Path::new(&file_path).try_exists() {
        trace!("file {} already exists", &file_path);
        return Ok(());
    }
    if let Some(primaries) = cluster.primaries().await {
        if let Some(node) = primaries.iter().next() {
            let mut data_file_destination = File::create(&filename).await.map_err(|e| {
                error!("{:?}", e);
                anyhow::anyhow!("filed to create file {}", filename)
            })?;
            let primary_uri = node
                .service_instance()
                .uri()
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Invalid ServiceInstance URI"))?;
            trace!(
                "downloading file {} from primary: {}",
                &filename,
                &primary_uri
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
    Ok(())
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

        let _ = mock_server.mock(|when, then| {
            when.method(httpmock::Method::GET).path(mock_uri);
            then.status(200)
                .header("content-type", "application/octet-stream")
                .header("Connection", "keep-alive")
                .body_from_file("download_file_from_url_requests.sqlite");
        });

        let _ = tokio::fs::remove_file("requests.downloaded.sqlite").await;

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

        let mut file = std::fs::File::open("download_file_from_url_requests.sqlite").unwrap();
        let mut sha256 = sha2::Sha256::new();
        std::io::copy(&mut file, &mut sha256).unwrap();
        let hash_1 = sha256.finalize();

        let mut file =
            std::fs::File::open("download_file_from_url_requests.downloaded.sqlite").unwrap();
        let mut sha256 = sha2::Sha256::new();
        std::io::copy(&mut file, &mut sha256).unwrap();
        let hash_2 = sha256.finalize();

        assert_eq!(hash_1, hash_2);
    }
}
