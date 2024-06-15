mod rate_spec;
mod request_specs;

pub use rate_spec::{ArraySpec, Bounded, ConstantRate, Elastic, Linear, Steps};
pub use request_specs::{
    JsonTemplate, RandomDataRequest, RequestFile, RequestList, SplitRequestFile,
};

use anyhow::Error as AnyError;
use common_types::LoadGenerationMode;
use once_cell::sync::OnceCell;
use response_assert::ResponseAssertion;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use sqlx::sqlite::SqliteRow;
use sqlx::Row;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt::{Display, Formatter};
use std::io::Error as StdIoError;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::str::FromStr;
use std::{env, fmt};

pub const DEFAULT_HTTP_PORT: u16 = 3030;
pub const ENV_NAME_HTTP_PORT: &str = "HTTP_ENDPOINT_PORT";
pub static HTTP_PORT: OnceCell<u16> = OnceCell::new();
pub const DEFAULT_DATA_DIR: &str = "/tmp";

pub fn data_dir_path() -> PathBuf {
    env::var("DATA_DIR")
        .map(|env| PathBuf::from("/").join(env))
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_DATA_DIR))
}

pub fn http_port() -> u16 {
    *HTTP_PORT.get_or_init(|| {
        env::var(ENV_NAME_HTTP_PORT)
            .map_err(|_| ())
            .and_then(|port| u16::from_str(&port).map_err(|_| ()))
            .unwrap_or(DEFAULT_HTTP_PORT)
    })
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Scheme {
    HTTP,
    //Unsupported
    // HTTPS
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub protocol: Scheme,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum RequestSpecEnum {
    RequestList(RequestList),
    RequestFile(RequestFile),
    SplitRequestFile(SplitRequestFile),
    RandomDataRequest(RandomDataRequest),
    JsonTemplateRequest(JsonTemplate),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum RateSpecEnum {
    ConstantRate(ConstantRate),
    Linear(Linear),
    ArraySpec(ArraySpec),
    Steps(Steps),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum ConcurrentConnectionRateSpec {
    ConstantRate(ConstantRate),
    Linear(Linear),
    ArraySpec(ArraySpec),
    Steps(Steps),
    Elastic(Elastic),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpReq {
    #[serde(default = "uuid")]
    pub id: String,
    #[serde(with = "http_serde::method")]
    pub method: http::Method,
    //todo as a http::Uri
    //panic in send_request if doesn't start with /
    pub url: String,
    pub body: Option<String>,
    #[serde(default = "HashMap::new")]
    pub headers: HashMap<String, String>,
}

impl<'a> sqlx::FromRow<'a, SqliteRow> for HttpReq {
    fn from_row(row: &'a SqliteRow) -> Result<Self, sqlx::Error> {
        let id: i64 = row.get("rowid");
        let method: String = row.get("method");
        let url: String = row.get("url");
        let body: Option<String> = row.get("body");
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

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionKeepAlive {
    pub ttl: u32,
    pub max_request_per_connection: NonZeroU32,
}

impl Default for ConnectionKeepAlive {
    fn default() -> Self {
        Self {
            ttl: u32::MAX,
            max_request_per_connection: NonZeroU32::new(u32::MAX).unwrap(),
        }
    }
}

/// Describe the request
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(from = "RequestShadow", rename_all = "camelCase")]
pub struct Request {
    pub name: Option<String>,
    pub duration: u32,
    pub target: Target,
    pub req: RequestSpecEnum,
    pub qps: RateSpecEnum,
    pub concurrent_connection: ConcurrentConnectionRateSpec,
    pub connection_keep_alive: ConnectionKeepAlive,
    pub histogram_buckets: SmallVec<[f64; 6]>,
    pub response_assertion: Option<ResponseAssertion>,
    pub generation_mode: LoadGenerationMode,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RequestShadow {
    pub name: Option<String>,
    pub duration: u32,
    pub target: Target,
    pub req: RequestSpecEnum,
    pub qps: RateSpecEnum,
    #[serde(default = "default_concurrent_connection")]
    pub concurrent_connection: ConcurrentConnectionRateSpec,
    #[serde(default)]
    pub connection_keep_alive: ConnectionKeepAlive,
    #[serde(default = "default_histogram_bucket")]
    pub histogram_buckets: SmallVec<[f64; 6]>,
    pub response_assertion: Option<ResponseAssertion>,
    #[serde(default = "default_generation_mode")]
    pub generation_mode: LoadGenerationMode,
}

impl From<RequestShadow> for Request {
    fn from(mut value: RequestShadow) -> Self {
        let target = &value.target;
        let req_spec = &mut value.req;
        match req_spec {
            RequestSpecEnum::JsonTemplateRequest(req) => {
                validate_and_update_host_header(&mut req.headers, target)
            }
            RequestSpecEnum::RandomDataRequest(req) => {
                validate_and_update_host_header(&mut req.headers, target)
            }
            _ => {}
        }
        Self {
            name: value.name,
            duration: value.duration,
            target: value.target,
            req: value.req,
            qps: value.qps,
            concurrent_connection: value.concurrent_connection,
            connection_keep_alive: value.connection_keep_alive,
            histogram_buckets: value.histogram_buckets,
            response_assertion: value.response_assertion,
            generation_mode: value.generation_mode,
        }
    }
}

/// check if host header exists, put the target host if it doesn't.
fn validate_and_update_host_header(headers: &mut HashMap<String, String>, target: &Target) {
    let host_exists = headers.keys().any(|x| http::header::HOST.eq(x.as_str()));
    if !host_exists {
        headers.insert(http::header::HOST.to_string(), target.host.clone());
    }
}

/// Describe multiple tests in single request
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultiRequest {
    pub name: Option<String>,
    pub requests: Vec<Request>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum JobStatusQueryParams {
    JobId { job_id: String },
    PagerOptions { offset: usize, limit: usize },
}

impl Display for JobStatusQueryParams {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            JobStatusQueryParams::JobId { job_id } => {
                write!(f, "job_id={}", job_id)
            }
            JobStatusQueryParams::PagerOptions { offset, limit } => {
                write!(f, "offset={}&limit={}", offset, limit)
            }
        }
    }
}

impl TryFrom<HashMap<String, String>> for JobStatusQueryParams {
    type Error = GenericError;

    fn try_from(mut value: HashMap<String, String>) -> Result<Self, Self::Error> {
        let job_id = value.remove("job_id");
        //try for job_id first
        let params = if let Some(job_id) = job_id {
            JobStatusQueryParams::JobId { job_id }
        } else {
            //try for offset & limit
            let offset = value.remove("offset");
            let offset = offset.as_ref().map_or("0", |o| o);
            let offset = offset.parse::<usize>().map_err(|e| {
                GenericError::new(&format!("Invalid offset {}, {}", &offset, e), 400)
            })?;
            let limit = value.remove("limit");
            let limit = limit.as_ref().map_or("20", |l| l);
            let limit = limit.parse::<usize>().map_err(|e| {
                GenericError::new(&format!("Invalid limit {}, {}", &offset, e), 400)
            })?;
            if limit < 1 {
                return Err(GenericError::new("limit can't be less than 1", 400));
            }
            JobStatusQueryParams::PagerOptions { offset, limit }
        };
        //after removing params, map should be empty
        if value.is_empty() {
            Ok(params)
        } else {
            Err(GenericError::new("Invalid or too many query params", 400))
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestJobResponse {
    pub job_id: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenericResponse<T: Serialize> {
    #[serde(flatten)]
    pub data: HashMap<String, T>,
}

impl<T: Serialize> Default for GenericResponse<T> {
    fn default() -> Self {
        GenericResponse {
            data: HashMap::with_capacity(2),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenericError {
    pub error_code: u16,
    pub message: String,
    #[serde(flatten)]
    pub data: HashMap<String, String>,
}

impl GenericError {
    pub fn internal_500(msg: &str) -> GenericError {
        GenericError {
            error_code: 500,
            message: msg.to_string(),
            ..Default::default()
        }
    }

    pub fn error_unknown() -> GenericError {
        Self::internal_500("Unknown error")
    }

    pub fn new(msg: &str, code: u16) -> Self {
        Self {
            error_code: code,
            message: msg.to_string(),
            ..Default::default()
        }
    }

    pub fn from_error<E: StdError>(code: u16, err: E) -> GenericError {
        Self::new(&err.to_string(), code)
    }
}

impl From<http::Error> for GenericError {
    fn from(e: http::Error) -> Self {
        GenericError {
            error_code: 400,
            message: e.to_string(),
            ..Default::default()
        }
    }
}

macro_rules! from_error {
    ($t:ty) => {
        impl From<$t> for GenericError {
            fn from(e: $t) -> Self {
                use log::error;
                use std::backtrace::Backtrace;
                error!("error: {}, {}", e.to_string(), Backtrace::force_capture());
                GenericError {
                    error_code: 500,
                    message: e.to_string(),
                    ..Default::default()
                }
            }
        }
    };
}

from_error!(hyper::Error);
from_error!(AnyError);
from_error!(serde_json::Error);
from_error!(StdIoError);

impl Default for GenericError {
    fn default() -> Self {
        GenericError {
            error_code: u16::MAX,
            message: String::new(),
            data: HashMap::new(),
        }
    }
}

impl Display for GenericError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({}, {}, {})",
            self.error_code,
            self.message,
            self.data.len()
        )
    }
}

impl StdError for GenericError {}

pub fn default_histogram_bucket() -> SmallVec<[f64; 6]> {
    smallvec::SmallVec::from(DEFAULT_HISTOGRAM_BUCKET)
}

pub fn default_generation_mode() -> LoadGenerationMode {
    LoadGenerationMode::Immediate
}

pub fn default_concurrent_connection() -> ConcurrentConnectionRateSpec {
    ConcurrentConnectionRateSpec::Elastic(Elastic::default())
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub const DEFAULT_HISTOGRAM_BUCKET: [f64; 6] = [20f64, 50f64, 100f64, 300f64, 700f64, 1100f64];
pub const PATH_REQUEST_DATA_FILE_DOWNLOAD: &str = "/cluster/data-file/";

#[cfg(test)]
mod test {
    use crate::*;
    use http::header::HOST;
    use http::Method;
    use std::collections::HashMap;
    use std::convert::TryInto;

    #[test]
    fn request_de_serialize_test_1() {
        let req = r#"
            {
              "duration": 1,
              "name": "demo-test",
              "qps": {
                "ConstantRate": {
                  "countPerSec": 1
                }
              },
              "req": {
                "RequestList": {
                  "data": [
                    {
                      "method": "GET",
                      "url": "example.com"
                    }
                  ]
                }
              },
              "target": {
                "host": "example.com",
                "port": 8080,
                "protocol": "HTTP"
              },
              "histogramBuckets": [35,40,45,48,50, 52],
              "generationMode": "immediate"
            }
        "#;
        let result = serde_json::from_str::<Request>(req).unwrap();
        let serialized = serde_json::to_string(&result).unwrap();
        println!("serialized: {}", serialized);
        //deserialize after serialized and then verify to ensure both de/serialization is okay
        let request = serde_json::from_str::<Request>(req).unwrap();

        assert_eq!(
            request.histogram_buckets,
            smallvec::SmallVec::from([35f64, 40f64, 45f64, 48f64, 50f64, 52f64])
        );
        let req = HttpReq {
            id: {
                if let RequestSpecEnum::RequestList(r) = &request.req {
                    r.data.first().unwrap().id.clone()
                } else {
                    "".to_string()
                }
            },
            method: Method::GET,
            url: "example.com".to_string(),
            body: None,
            headers: Default::default(),
        };
        assert_eq!(
            serde_json::to_value(request.req).unwrap(),
            serde_json::to_value(RequestSpecEnum::RequestList(RequestList::from(vec![req])))
                .unwrap()
        );
        assert!(matches!(
            request.generation_mode,
            LoadGenerationMode::Immediate
        ));
    }

    #[test]
    fn request_de_serialize_test_2() {
        let req = r#"
            {
              "duration": 1,
              "name": "demo-test",
              "qps": {
                "ConstantRate": {
                  "countPerSec": 1
                }
              },
              "req": {
                "RequestList": {
                  "data": [
                    {
                      "method": "GET",
                      "url": "example.com"
                    }
                  ]
                }
              },
              "target": {
                "host": "example.com",
                "port": 8080,
                "protocol": "HTTP"
              },
              "histogramBuckets": [
                35,
                40,
                45,
                48,
                50,
                52
              ],
              "generationMode": {
                "batch": {
                  "batchSize": 10
                }
              }
            }
        "#;
        let result = serde_json::from_str::<Request>(req).unwrap();
        let serialized = serde_json::to_string(&result).unwrap();
        println!("serialized: {}", serialized);
        //deserialize after serialized and then verify to ensure both de/serialization is okay
        let request = serde_json::from_str::<Request>(req).unwrap();

        assert_eq!(
            request.histogram_buckets,
            smallvec::SmallVec::from([35f64, 40f64, 45f64, 48f64, 50f64, 52f64])
        );
        let req = HttpReq {
            id: {
                if let RequestSpecEnum::RequestList(r) = &request.req {
                    r.data.first().unwrap().id.clone()
                } else {
                    "".to_string()
                }
            },
            method: Method::GET,
            url: "example.com".to_string(),
            body: None,
            headers: Default::default(),
        };
        assert_eq!(
            serde_json::to_value(request.req).unwrap(),
            serde_json::to_value(RequestSpecEnum::RequestList(RequestList::from(vec![req])))
                .unwrap()
        );
        assert!(matches!(
            request.generation_mode,
            LoadGenerationMode::Batch { .. }
        ));
    }

    #[test]
    fn job_status_query_param() {
        let mut query = HashMap::new();
        query.insert("offset".to_string(), "0".to_string());
        query.insert("limit".to_string(), "20".to_string());
        let result: JobStatusQueryParams = query.try_into().unwrap();
        assert!(matches!(
            &result,
            JobStatusQueryParams::PagerOptions {
                offset: 0,
                limit: 20
            }
        ));
        assert_eq!("offset=0&limit=20", format!("{}", result));

        query = HashMap::new();
        query.insert("job_id".to_string(), "some-job-id".to_string());
        let result: JobStatusQueryParams = query.try_into().unwrap();
        println!("{:?}", &result);
        assert!(matches!(result, JobStatusQueryParams::JobId { job_id: _ }));
        assert_eq!("job_id=some-job-id", format!("{}", result));

        query = HashMap::new();
        query.insert("offset".to_string(), "0".to_string());
        query.insert("limit".to_string(), "20".to_string());
        query.insert("job_id".to_string(), "some-uuid".to_string());
        let result: Result<JobStatusQueryParams, GenericError> = query.try_into();
        println!("{:?}", &result);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_post_http_req() {
        let req_str = r#"
          {
            "method": "POST",
            "url": "/anything",
            "headers": {
              "Host": "127.0.0.1:2080",
              "Connection":"keep-alive"
            },
            "body": "{\"data\":[{\"shopId\":12345,\"itemId\":54321},{\"shopId\":12345,\"itemId\":54321}]}"
          }"#;
        let _req: HttpReq = serde_json::from_str(req_str).unwrap();
    }

    #[test]
    fn test_host_header_update() {
        let req_str = r#"
        {
          "duration": 10,
          "name": "json-template-get.json",
          "qps": {
            "ConstantRate": {
              "countPerSec": 3
            }
          },
          "req": {
            "JsonTemplateRequest": {
              "method": "GET",
              "url": "{{patternStr(\"/anything/[a-z]{4}\")}}",
              "headers": {
                "Connection":"keep-alive"
              }
            }
          },
          "target": {
            "host": "127.0.0.1",
            "port": 2080,
            "protocol": "HTTP"
          },
          "generationMode": {
            "batch": {
              "batchSize": 3
            }
          },
          "concurrentConnection": {
            "ConstantRate": {
              "countPerSec": 3
            }
          }
        }
        "#;
        let result = serde_json::from_str(req_str);
        assert!(result.is_ok());
        let req: Request = result.unwrap();
        let target_host = &req.target.host;
        let req = req.req;
        assert!(matches!(req, RequestSpecEnum::JsonTemplateRequest(_)));
        if let RequestSpecEnum::JsonTemplateRequest(req) = req {
            assert_eq!(req.headers.len(), 2);
            assert!(req
                .headers
                .iter()
                .any(|(k, v)| HOST.eq(k.as_str()) && v.eq(target_host)));

            assert!(req.headers.keys().any(|x| { HOST.eq(x.as_str()) }));
        }
    }

    #[test]
    fn test_host_header_no_overwrite() {
        let req_str = r#"
        {
          "duration": 10,
          "name": "json-template-get.json",
          "qps": {
            "ConstantRate": {
              "countPerSec": 3
            }
          },
          "req": {
            "JsonTemplateRequest": {
              "method": "GET",
              "url": "{{patternStr(\"/anything/[a-z]{4}\")}}",
              "headers": {
                "Connection":"keep-alive",
                "host": "localhost"
              }
            }
          },
          "target": {
            "host": "127.0.0.1",
            "port": 2080,
            "protocol": "HTTP"
          },
          "generationMode": {
            "batch": {
              "batchSize": 3
            }
          },
          "concurrentConnection": {
            "ConstantRate": {
              "countPerSec": 3
            }
          }
        }
        "#;
        let result = serde_json::from_str(req_str);
        assert!(result.is_ok());
        let req: Request = result.unwrap();
        let req = req.req;
        assert!(matches!(req, RequestSpecEnum::JsonTemplateRequest(_)));
        if let RequestSpecEnum::JsonTemplateRequest(req) = req {
            assert_eq!(req.headers.len(), 2);
            assert!(req
                .headers
                .iter()
                .any(|(k, v)| HOST.eq(k.as_str()) && v.eq("localhost")));

            assert!(req.headers.keys().any(|x| { HOST.eq(x.as_str()) }));
        }
    }
}
