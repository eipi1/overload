use crate::cluster::secondary::{
    forward_get_status_request, forward_stop_request_to_primary, forward_test_request,
};
use crate::{job_id, pre_check, Response, PATH_FILE_UPLOAD};
use bytes::Buf;
use cluster_executor::{get_status_all, get_status_by_job_id, ErrorCode, JobStatus};
use cluster_mode::{Cluster, RestClusterNode};
use futures_util::Stream;
use http::header::CONTENT_LENGTH;
use hyper::{Body, Client};
use log::debug;
use log::error;
use overload_http::{GenericError, GenericResponse, JobStatusQueryParams, MultiRequest, Request};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio_stream::StreamExt;
use url::Url;

mod primary;
pub(crate) mod secondary;

type GenericResult<T> = Result<GenericResponse<T>, GenericError>;

pub async fn handle_request_cluster(mut request: Request, cluster: Arc<Cluster>) -> Response {
    let job_id = job_id(&request.name);
    request.name = Some(job_id.clone());
    if !cluster.is_active().await {
        Response::new(job_id, JobStatus::Error(ErrorCode::InactiveCluster))
    } else if cluster.is_primary().await {
        let secondaries = cluster.secondaries().await;
        if secondaries.is_none() || cluster.secondaries().await.unwrap().is_empty() {
            return Response::new(job_id, JobStatus::Error(ErrorCode::NoSecondary));
        }
        if let Err(e) = pre_check(&request) {
            return Response::new(job_id, JobStatus::Error(e));
        }
        tokio::spawn(cluster_executor::primary::handle_request(
            request,
            cluster.clone(),
        ));
        Response::new(job_id, JobStatus::Starting)
    } else {
        //forward request to primary
        let client = Client::new();
        match forward_test_request("test", request, cluster.clone(), client).await {
            Ok(resp) => resp,
            Err(err) => {
                error!("{}", err);
                unknown_error_resp(job_id)
            }
        }
    }
}

pub async fn handle_multi_request_cluster(
    mut requests: MultiRequest,
    cluster: Arc<Cluster>,
) -> Response {
    let job_id = job_id(&requests.name);
    if !cluster.is_active().await {
        Response::new(job_id, JobStatus::Error(ErrorCode::InactiveCluster))
    } else if cluster.is_primary().await {
        for (idx, mut request) in requests.requests.into_iter().enumerate() {
            let job_id_for_req = format!("{}_{}", &job_id, idx);
            request.name = Some(job_id_for_req);
            tokio::spawn(cluster_executor::primary::handle_request(
                request,
                cluster.clone(),
            ));
        }
        Response::new(job_id, JobStatus::Starting)
    } else {
        //forward request to primary
        let client = Client::new();
        requests.name = Some(job_id.clone());
        match forward_test_request("tests", requests, cluster, client).await {
            Ok(resp) => resp,
            Err(err) => {
                error!("{}", err);
                unknown_error_resp(job_id)
            }
        }
    }
}

pub async fn handle_history_all(
    params: JobStatusQueryParams,
    cluster: Arc<Cluster>,
) -> GenericResult<JobStatus> {
    if !cluster.is_active().await {
        Err(inactive_cluster_error())
    } else if cluster.is_primary().await {
        handle_get_status(params).await
    } else {
        // forward request to primary
        forward_get_status_request(params, &cluster).await
    }
}

pub async fn handle_get_status(
    params: JobStatusQueryParams,
) -> Result<GenericResponse<cluster_executor::JobStatus>, GenericError> {
    let status = match params {
        JobStatusQueryParams::JobId { job_id } => get_job_status(Some(job_id), 0, 0),
        JobStatusQueryParams::PagerOptions { offset, limit } => get_job_status(None, offset, limit),
    }
    .await;
    Ok(GenericResponse { data: status })
}

/// return status of test jobs.
/// job_id has higher priority and will return status of the the job. Otherwise will
/// return status of `limit` jobs, starting at `offset`
pub(crate) async fn get_job_status(
    job_id: Option<String>,
    offset: usize,
    limit: usize,
) -> HashMap<String, cluster_executor::JobStatus> {
    if let Some(job_id) = job_id {
        return get_status_by_job_id(&job_id).await;
    }

    let mut job_status = HashMap::new();
    let mut count = 0;
    let status_all = get_status_all().await;
    let len = status_all.len();
    for (id, status) in status_all.into_iter() {
        if count > len {
            break;
        }
        count += 1;
        if count > offset && count - offset > limit {
            break;
        }
        if count > offset {
            job_status.insert(id, status);
        }
    }
    job_status
}

pub async fn stop(job_id: String, cluster: Arc<Cluster>) -> GenericResult<String> {
    if !cluster.is_active().await {
        Err(inactive_cluster_error())
    } else if cluster.is_primary().await {
        stop_job(job_id).await
    } else {
        // forward request to primary
        forward_stop_request_to_primary(&job_id, &cluster).await
    }
}

pub async fn stop_job(job_id: String) -> Result<GenericResponse<String>, GenericError> {
    let mut result = cluster_executor::stop_request_by_job_id(&job_id).await;
    let response = result
        .drain(..)
        .map(|job| {
            let msg = if let Some(status) = job.1 {
                match status {
                    JobStatus::Starting | JobStatus::InProgress => "Job stopped",
                    JobStatus::Stopped => "Job already stopped",
                    JobStatus::Completed => "Job already completed",
                    JobStatus::Failed => "Job failed",
                    _ => {
                        error!("Invalid job status for job: {}", job_id);
                        "Invalid job status"
                    }
                }
                .to_string()
            } else {
                "Job Not Found".to_string()
            };
            (job.0, msg)
        })
        .collect::<HashMap<_, _>>();
    Ok(GenericResponse { data: response })
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
        crate::file_uploader::csv_stream_to_sqlite(data, data_dir).await
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

pub(crate) fn inactive_cluster_error() -> GenericError {
    GenericError {
        error_code: 404,
        message: {
            let status = cluster_executor::JobStatus::Error(ErrorCode::InactiveCluster);
            serde_json::to_string(&status).unwrap_or_default()
        },
        data: Default::default(),
    }
}

pub(crate) async fn primary_uri(
    primaries: &Option<HashSet<RestClusterNode>>,
) -> Result<&String, GenericError> {
    primaries
        .as_ref()
        .and_then(|s| s.iter().next())
        .map(|p| p.service_instance())
        .and_then(|si| si.uri().as_ref())
        .ok_or_else(|| GenericError::internal_500("No primary or invalid ServiceInstance URI"))
}

pub(crate) fn convert_stream<S, B>(
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

pub(crate) fn unknown_error_resp(job_id: String) -> Response {
    Response::new(job_id, JobStatus::Error(ErrorCode::Others))
}
