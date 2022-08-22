use crate::executor::cluster::cluster_execute_request_generator;
use crate::http_util::request::{JobStatusQueryParams, Request};
use crate::http_util::{
    job_id, unknown_error_resp, GenericError, GenericResponse, GenericResult, PATH_JOB_STATUS,
    PATH_STOP_JOB,
};
use crate::{ErrorCode, JobStatus, Response};
use bytes::Buf;
use cluster_mode::{Cluster, RestClusterNode};
use hyper::client::HttpConnector;
use hyper::{Body, Client};
use log::{error, trace};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;

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
