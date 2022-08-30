#![allow(clippy::upper_case_acronyms)]

mod datagen;
pub mod executor;
pub mod generator;
pub mod http_util;
pub mod metrics;

use http::Method;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::Row;
use std::collections::HashMap;
use std::convert::TryInto;
use std::fmt::Display;
use std::hash::{Hash, Hasher};
use std::{env, fmt};

pub const DEFAULT_DATA_DIR: &str = "/tmp";
pub const PATH_REQUEST_DATA_FILE_DOWNLOAD: &str = "/cluster/data-file";

pub fn data_dir() -> String {
    env::var("DATA_DIR").unwrap_or_else(|_| DEFAULT_DATA_DIR.to_string())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpReq {
    #[serde(default = "uuid")]
    pub id: String,
    #[serde(with = "http_serde::method")]
    pub method: Method,
    //todo as a http::Uri
    //panic in send_request if doesn't start with /
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
    /// For a test, each request will be given a uuid. As long as uuid is equal
    /// the request will be treated as same request.
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

impl<'a> sqlx::FromRow<'a, SqliteRow> for HttpReq {
    fn from_row(row: &'a SqliteRow) -> Result<Self, sqlx::Error> {
        let id: i64 = row.get("rowid");
        let method: String = row.get("method");
        let url: String = row.get("url");
        let body: Option<Vec<u8>> = row.get("body");
        let headers: String = row.get("headers");

        let req = HttpReq {
            id: id.to_string(),
            method: method.as_str().try_into().unwrap(),
            url,
            body,
            headers: serde_json::from_str(headers.as_str()).unwrap_or_default(),
        };
        Ok(req)
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
    SqliteOpenFailed,
    PreparationFailed,
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

    pub fn get_status(&self) -> JobStatus {
        self.status
    }
}

#[macro_export]
macro_rules! log_error {
    ($result:expr) => {
        if let Err(e) = $result {
            use log::error;
            error!("{}", e.to_string());
        }
    };
}

#[cfg(test)]
pub mod test_utils {
    use crate::http_util::csv_reader_to_sqlite;
    use csv_async::AsyncReaderBuilder;

    pub async fn generate_sqlite_file(file_path: &str) {
        let csv_data = r#""url","method","body","headers"
            "http://httpbin.org/anything/11","GET","","{}"
            "http://httpbin.org/anything/13","GET","","{}"
            "http://httpbin.org/anything","POST","{\"some\":\"random data\",\"second-key\":\"more data\"}","{\"Authorization\":\"Bearer 123\"}"
            "http://httpbin.org/bearer","GET","","{\"Authorization\":\"Bearer 123\"}"
            "#;
        let reader = AsyncReaderBuilder::new()
            .escape(Some(b'\\'))
            .create_deserializer(csv_data.as_bytes());
        let to_sqlite = csv_reader_to_sqlite(reader, file_path.to_string()).await;
        log_error!(to_sqlite);
    }
}
