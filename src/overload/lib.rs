pub mod executor;
pub mod http;
pub mod generator;

use serde::{Serialize, Deserialize};
use std::fmt::Display;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ReqMethod {
    GET,
    POST,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpReq {
    pub method: ReqMethod,
    pub url: String,
    pub body: Option<Vec<u8>>,
}

impl Display for HttpReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        //todo print body length
        write!(f, "{:?} {}", &self.method, &self.url)
    }
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone)]
pub enum JobStatus {
    Starting,
    InProgress,
    Stopped,
    Completed,
    Failed,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    job_id: String,
    status: JobStatus,
}

impl Response {
    pub fn new(job_id: String, status: JobStatus) -> Self {
        Response {
            job_id,
            status,
        }
    }
}

