#![allow(clippy::upper_case_acronyms)]
#![warn(unused_lifetimes)]
#![forbid(unsafe_code)]
#![allow(clippy::single_match)]

use cluster_executor::cleanup_job;
use lazy_static::lazy_static;
use log::error;
use overload_http::{Request, RequestSpecEnum};
use overload_metrics::MetricsFactory;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

pub use cluster_executor::ErrorCode;
pub use cluster_executor::JobStatus;
use datagen::generate_data;

#[cfg(feature = "cluster")]
pub mod cluster;
pub mod file_uploader;
#[cfg(not(feature = "cluster"))]
pub mod standalone;

pub const TEST_REQ_TIMEOUT: u8 = 30; // 30 sec hard timeout for requests to test target
pub const DEFAULT_DATA_DIR: &str = "/tmp";
pub const PATH_REQUEST_DATA_FILE_DOWNLOAD: &str = "/cluster/data-file";

pub const PATH_JOB_STATUS: &str = "/test/status";
pub const PATH_STOP_JOB: &str = "/test/stop";
pub const PATH_FILE_UPLOAD: &str = "/test/requests-bin";

lazy_static! {
    pub static ref METRICS_FACTORY: MetricsFactory = MetricsFactory::default();
}

#[deprecated(note = "use data_dir_path")]
pub fn data_dir() -> String {
    env::var("DATA_DIR").unwrap_or_else(|_| DEFAULT_DATA_DIR.to_string())
}

#[deprecated(note = "use cluster_executor::data_dir_path")]
pub fn data_dir_path() -> PathBuf {
    env::var("DATA_DIR")
        .map(|env| PathBuf::from("/").join(env))
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_DATA_DIR))
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

pub(crate) fn pre_check(request: &Request) -> Result<(), ErrorCode> {
    match &request.req {
        RequestSpecEnum::RequestFile(file) => {
            if !request_file_exists(&file.file_name) {
                return Err(ErrorCode::RequestFileNotFound);
            }
        }
        RequestSpecEnum::RandomDataRequest(req) => {
            if let Some(schema) = &req.body_schema {
                if matches!(generate_data(schema), Value::Null) {
                    return Err(ErrorCode::BodyGenerationFailure);
                }
            }
            if let Some(schema) = &req.uri_param_schema {
                if matches!(generate_data(schema), Value::Null) {
                    return Err(ErrorCode::UriParamGenerationFailure);
                }
            }
        }
        _ => {}
    }
    request
        .response_assertion
        .as_ref()
        .and_then(|assertion| assertion.lua_assertion.as_ref())
        .map(|chunks| chunks.join("\n"))
        .map(|chunk| {
            lua_helper::verify_lua_chunk(&chunk).map_err(|e| {
                error!("lua verification failed - chunk: {}, error: {}", &chunk, e);
                ErrorCode::LuaParseFailure
            })
        })
        .unwrap_or(Ok(()))
}

#[inline(always)]
#[allow(dead_code)]
fn request_file_exists(file_name: &str) -> bool {
    let mut path = cluster_executor::data_dir_path().join(file_name);
    path.set_extension("sqlite");
    path.exists()
}

//todo verify for cluster mode. using job id as name for secondary request
fn job_id(request_name: &Option<String>) -> String {
    request_name
        .clone()
        .map_or(Uuid::new_v4().to_string(), |n| {
            let uuid = Regex::new(
                r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}(_[0-9]+)?$",
            )
            .unwrap();
            if uuid.is_match(&n) {
                n
            } else {
                let mut name = n.trim().to_string();
                name.push('-');
                name.push_str(Uuid::new_v4().to_string().as_str());
                name
            }
        })
}

pub async fn init() {
    //no need to remove pools with improved cluster communication
    //as secondaries receive stop/finish message they'll can be cleaned up when done.
    /*#[cfg(feature = "cluster")]
    tokio::spawn(async {
        //If the pool hasn't been used for 30 second, drop it
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            interval.tick().await;
            let removable = {
                CONNECTION_POOLS
                    .read()
                    .await
                    .iter()
                    .filter(|v| v.1.last_use.elapsed() > Duration::from_secs(30))
                    .map(|v| v.0.clone())
                    .collect::<Vec<String>>()
            };
            for job_id in removable {
                CONNECTION_POOLS.write().await.remove(&job_id);
                METRICS_FACTORY.remove_metrics(&job_id).await;
                CONNECTION_POOLS_USAGE_LISTENER
                    .write()
                    .await
                    .remove(&job_id);
            }
        }
    });
    */

    //start JOB_STATUS clean up task
    let mut interval = tokio::time::interval(Duration::from_secs(1800));
    loop {
        interval.tick().await;
        cleanup_job(|status| {
            matches!(
                status,
                JobStatus::Failed | JobStatus::Stopped | JobStatus::Completed
            )
        })
        .await;
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
    use crate::file_uploader::csv_reader_to_sqlite;
    use crate::job_id;
    use csv_async::AsyncReaderBuilder;
    use test_case::test_case;

    pub async fn generate_sqlite_file(file_path: &str) {
        let csv_data = r#"
"url","method","body","headers"
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

    #[test_case("multi-req-7e19ae6b-59af-4916-8cc0-a04cc48a739b", "multi-req-7e19ae6b-59af-4916-8cc0-a04cc48a739b" ; "job id without sub test id")]
    #[test_case("multi-req-7e19ae6b-59af-4916-8cc0-a04cc48a739b_0", "multi-req-7e19ae6b-59af-4916-8cc0-a04cc48a739b_0" ; "job id with sub test id")]
    fn test_job_id(name: &str, expect: &str) {
        let option = Some(name.to_string());
        assert_eq!(job_id(&option), expect.to_string());
    }
}
