use crate::cluster::primary_uri;
use crate::cluster::{unknown_error_resp, GenericResult};
use crate::{Response, PATH_JOB_STATUS, PATH_STOP_JOB};
use bytes::Buf;
use cluster_executor::JobStatus;
use cluster_mode::Cluster;
use hyper::client::HttpConnector;
use hyper::{Body, Client};
use log::trace;
use overload_http::{GenericError, GenericResponse, JobStatusQueryParams};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::Arc;

pub(crate) async fn forward_other_requests_to_primary<T: Serialize + DeserializeOwned>(
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

pub(crate) async fn forward_stop_request_to_primary(
    job_id: &String,
    cluster: &Arc<Cluster>,
) -> GenericResult<String> {
    let client = Client::new();
    if let Ok(uri) = primary_uri(&cluster.primaries().await).await {
        let request = hyper::Request::builder()
            .uri(format!("{}{}/{}", &uri, PATH_STOP_JOB, &job_id))
            .method("GET")
            .body(Body::empty())?;
        forward_other_requests_to_primary::<String>(request, cluster.clone(), client).await
    } else {
        Err(GenericError::internal_500("Unknown error"))
    }
}

pub(crate) async fn forward_get_status_request(
    params: JobStatusQueryParams,
    cluster: &Arc<Cluster>,
) -> GenericResult<JobStatus> {
    let client = Client::new();
    if let Ok(uri) = primary_uri(&cluster.primaries().await).await {
        let request = hyper::Request::builder()
            .uri(format!("{}{}?{}", &uri, PATH_JOB_STATUS, params))
            .method("GET")
            .body(Body::empty())?;
        forward_other_requests_to_primary::<JobStatus>(request, cluster.clone(), client).await
    } else {
        Err(GenericError::internal_500("Unknown error"))
    }
}

pub(crate) async fn forward_test_request<T: Serialize>(
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
