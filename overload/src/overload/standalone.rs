use crate::{job_id, pre_check, Response};
use cluster_executor::{get_status_all, get_status_by_job_id, JobStatus};
use log::error;
use overload_http::{GenericError, GenericResponse, JobStatusQueryParams, MultiRequest, Request};
use std::collections::HashMap;

pub async fn handle_request(request: Request) -> Response {
    let mut request = request;
    let job_id = job_id(&request.name);
    request.name = Some(job_id.clone());
    if let Err(e) = pre_check(&request) {
        return Response::new(job_id, JobStatus::Error(e));
    }
    tokio::spawn(cluster_executor::standalone::handle_request(request));
    Response::new(job_id, JobStatus::Starting)
}

pub async fn handle_multi_request(requests: MultiRequest) -> Response {
    let job_id = job_id(&requests.name);

    for (idx, mut request) in requests.requests.into_iter().enumerate() {
        let job_id_for_req = format!("{}_{}", &job_id, idx);
        request.name = Some(job_id_for_req);
        if let Err(e) = pre_check(&request) {
            return Response::new(job_id, JobStatus::Error(e));
        }
        tokio::spawn(cluster_executor::standalone::handle_request(request));
    }
    Response::new(job_id, JobStatus::Starting)
}

pub async fn stop(job_id: String) -> Result<GenericResponse<String>, GenericError> {
    let mut result = cluster_executor::stop_request_by_job_id(&job_id).await;
    let response = result
        .drain(..)
        .map(|job| {
            let msg = if let Some(status) = job.1 {
                match status {
                    cluster_executor::JobStatus::Starting
                    | cluster_executor::JobStatus::InProgress => "Job stopped",
                    cluster_executor::JobStatus::Stopped => "Job already stopped",
                    cluster_executor::JobStatus::Completed => "Job already completed",
                    cluster_executor::JobStatus::Failed => "Job failed",
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
