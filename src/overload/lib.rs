#![allow(clippy::upper_case_acronyms)]

pub mod executor;
pub mod generator;
pub mod http_util;

use http::Method;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::fmt::Display;
use std::fmt;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpReq {
    #[serde(default = "uuid")]
    pub id: String,
    #[serde(with = "http_serde::method")]
    pub method: Method,
    pub url: String,
    pub body: Option<Vec<u8>>,
    #[serde(default = "HashMap::new")]
    pub headers: HashMap<String, String>,
}

impl HttpReq {
    pub fn new(url: String) -> Self {
        HttpReq {
            id: uuid(),
            method: http::Method::GET,
            url,
            body: None,
            headers: HashMap::new(),
        }
    }
}

impl Display for HttpReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        //todo print body length
        write!(f, "{:?} {}", &self.method, &self.url)
    }
}

impl PartialEq for HttpReq {
    /// The purpose is not to test if two request is exactly equal, rather to check if two
    /// represent the same request.
    /// For a test, each request will be given a uuid. As long as uuid is same
    /// the request will be treated as equal request.
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for HttpReq {}

impl Hash for HttpReq {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone)]
pub enum JobStatus {
    Starting,
    InProgress,
    Stopped,
    Completed,
    Failed,
    Error(ErrorCode),
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone)]
pub enum ErrorCode {
    InactiveCluster,
    Others,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    job_id: String,
    status: JobStatus,
}

impl Response {
    pub fn new(job_id: String, status: JobStatus) -> Self {
        Response { job_id, status }
    }
}

#[macro_export]
macro_rules! cfg_cluster {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "cluster")]
            $item
        )*
    }
}

#[macro_export]
macro_rules! log_error {
    ($result:expr) => {
        if let Err(e) = $result {
            error!("{}", e.to_string());
        }
    };
}
