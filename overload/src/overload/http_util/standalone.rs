use crate::executor::{get_job_status, send_stop_signal};
use crate::http_util::request::JobStatusQueryParams;
use crate::http_util::{GenericError, GenericResponse};
use crate::JobStatus;
use log::error;
use std::collections::HashMap;

pub async fn handle_history_all(
    params: JobStatusQueryParams,
) -> Result<GenericResponse<JobStatus>, GenericError> {
    let status = match params {
        JobStatusQueryParams::JobId { job_id } => get_job_status(Some(job_id), 0, 0),
        JobStatusQueryParams::PagerOptions { offset, limit } => get_job_status(None, offset, limit),
    }
    .await;
    Ok(GenericResponse { data: status })
}

pub async fn stop(job_id: String) -> Result<GenericResponse<String>, GenericError> {
    let mut result = send_stop_signal(&job_id).await;
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
