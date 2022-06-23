use crate::http_util::request::JobStatusQueryParams;
use crate::http_util::{GenericError, GenericResponse, PATH_JOB_STATUS, PATH_STOP_JOB};
use crate::{ErrorCode, JobStatus};
use bytes::Buf;
use cluster_mode::{Cluster, RestClusterNode};
use http::Request;
use hyper::client::HttpConnector;
use hyper::{Body, Client};
use log::trace;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;

type GenericResult<T> = Result<GenericResponse<T>, GenericError>;

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
            forward_test_request::<JobStatus>(request, cluster, client).await
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
            forward_test_request::<String>(request, cluster, client).await
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

async fn forward_test_request<'d, T: Serialize + DeserializeOwned>(
    req: Request<Body>,
    _cluster: Arc<Cluster>,
    client: Client<HttpConnector>,
) -> GenericResult<T> {
    trace!("forwarding request to primary: {}", &req.uri());
    let resp = client.request(req).await?;
    let bytes = hyper::body::to_bytes(resp.into_body()).await?.reader();
    let resp: GenericResponse<T> = serde_json::from_reader(bytes)?;
    Ok(resp)
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
