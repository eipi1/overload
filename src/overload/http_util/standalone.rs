use crate::executor::{get_job_status, send_stop_signal};
use crate::http_util::{GenericError, GenericResponse};
use crate::JobStatus;
use std::collections::HashMap;
use crate::http_util::request::JobStatusQueryParams;

pub async fn handle_history_all(params: JobStatusQueryParams) -> Result<GenericResponse<JobStatus>, GenericError> {
    let status = match params {
        JobStatusQueryParams::JobId { job_id } => {
            get_job_status(Some(job_id), 0, 0)
        }
        JobStatusQueryParams::PagerOptions { offset,limit } => {
            get_job_status(None, offset, limit)
        }
    }.await;
    Ok(GenericResponse { data: status })
}

pub async fn stop(job_id: String) -> Result<GenericResponse<String>, GenericError> {
    let result = send_stop_signal(&job_id).await;
    let mut response = HashMap::with_capacity(2);
    response.insert("job_id".to_string(), job_id);
    response.insert("message".to_string(), result);
    Ok(GenericResponse { data: response })
}
