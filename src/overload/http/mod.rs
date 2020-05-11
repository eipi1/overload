pub mod request;

use crate::executor::{execute_request_generator, send_stop_signal, get_job_status};
use crate::http::request::{Request};
use crate::{JobStatus, Response};
use uuid::Uuid;
use serde::{Serialize, Deserialize};
use std::collections::hash_map::RandomState;
use std::collections::HashMap;

pub async fn handle_request(request: Request) -> Response {
    let job_id = request.name.clone().map_or(Uuid::new_v4().to_string(), |n| {
        let mut name = n;
        name.push('-');
        name.push_str(Uuid::new_v4().to_string().as_str());
        name
    });
    let generator = request.into();

    tokio::spawn(execute_request_generator(generator, job_id.clone()));
    Response::new(job_id, JobStatus::Starting)
}

pub async fn stop(job_id: String) -> GenericResponse {
    let result = send_stop_signal(job_id.clone()).await;
    GenericResponse {
        job_id,
        message: result,
    }
}

pub async fn handle_history_all(offset:usize, limit:usize) -> HashMap<String, JobStatus, RandomState> {
    get_job_status(offset,limit).await
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenericResponse {
    job_id: String,
    message: String,
}