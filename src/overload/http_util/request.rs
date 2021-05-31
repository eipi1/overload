#![allow(clippy::upper_case_acronyms)]

use crate::generator::{
    ArrayQPS, ConstantQPS, Linear, QPSScheme, RequestFile, RequestGenerator, RequestList,
    RequestProvider,
};
use crate::http_util::GenericError;
use crate::{data_dir, fmt, HttpReq};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fmt::{Debug, Display, Formatter};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum QPSSpec {
    ConstantQPS(ConstantQPS),
    Linear(Linear),
    ArrayQPS(ArrayQPS),
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum RequestSpecEnum {
    RequestList(RequestList),
    RequestFile(RequestFile),
}

impl From<Vec<HttpReq>> for RequestSpecEnum {
    fn from(req: Vec<HttpReq>) -> Self {
        RequestSpecEnum::RequestList(RequestList { data: req })
    }
}

impl TryFrom<&String> for RequestSpecEnum {
    type Error = serde_json::Error;

    fn try_from(value: &String) -> Result<Self, Self::Error> {
        serde_json::from_str(&*value)
    }
}

/// Describe the request, for example
/// ```
/// # use overload::http_util::request::Request;
/// # fn main() {
///     # let req = r#"
/// {
///   "name": null,
///   "duration": 1,
///   "req": {
///     "RequestList": {
///       "data": [
///         {
///           "method": "GET",
///           "url": "example.com",
///           "body": null
///         }
///       ]
///     }
///   },
///   "qps": {
///     "ConstantQPS": {
///       "qps": 1
///     }
///   }
/// # "#;
/// #     let result = serde_json::from_str::<Request>(req);
/// # }
/// ```
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub name: Option<String>,
    pub(crate) duration: u32,
    pub(crate) req: RequestSpecEnum,
    pub(crate) qps: QPSSpec,
}

#[allow(clippy::from_over_into)]
impl Into<RequestGenerator> for Request {
    fn into(self) -> RequestGenerator {
        let qps: Box<dyn QPSScheme + Send> = match self.qps {
            QPSSpec::ConstantQPS(qps) => Box::new(qps),
            QPSSpec::Linear(qps) => Box::new(qps),
            QPSSpec::ArrayQPS(qps) => Box::new(qps),
        };
        let req: Box<dyn RequestProvider + Send> = match self.req {
            RequestSpecEnum::RequestList(req) => Box::new(req),
            RequestSpecEnum::RequestFile(mut req) => {
                //todo why rename here?
                req.file_name = format!("{}/{}.sqlite", data_dir(), &req.file_name);
                Box::new(req)
            }
        };
        RequestGenerator::new(self.duration, req, qps)
    }
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
            let offset = offset.as_ref().map_or("0", |o| &*o);
            let offset = offset.parse::<usize>().map_err(|e| {
                GenericError::new(
                    &*format!("Invalid offset {}, {}", &offset, e.to_string()),
                    400,
                )
            })?;
            let limit = value.remove("limit");
            let limit = limit.as_ref().map_or("20", |l| &*l);
            let limit = limit.parse::<usize>().map_err(|e| {
                GenericError::new(
                    &*format!("Invalid limit {}, {}", &offset, e.to_string()),
                    400,
                )
            })?;
            if limit < 1 {
                return Err(GenericError::new("limit can't be less than 1", 400));
            }
            JobStatusQueryParams::PagerOptions {
                offset,
                limit,
            }
        };
        //after removing params, map should be empty
        if value.is_empty() {
            Ok(params)
        } else {
            Err(GenericError::new("Invalid or too many query params", 400))
        }
    }
}

#[cfg(test)]
mod test {
    use crate::generator::test::req_list_with_n_req;
    use crate::generator::{request_generator_stream, ConstantQPS, RequestGenerator};
    use crate::http_util::request::{JobStatusQueryParams, Request};
    use crate::http_util::GenericError;
    use std::collections::HashMap;
    use std::convert::TryInto;

    #[test]
    fn deserialize_str() {
        let req = r#"
            {
              "duration": 1,
              "name": null,
              "qps": {
                "ConstantQPS": {
                  "qps": 1
                }
              },
              "req": {
                "RequestList": {
                  "data": [
                    {
                      "body": null,
                      "method": "GET",
                      "url": "example.com"
                    }
                  ]
                }
              }
            }
        "#;
        let result = serde_json::from_str::<Request>(req);
        assert!(result.is_ok());
    }

    #[tokio::test]
    #[allow(unused_must_use)]
    async fn test_req_param() {
        let generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(1)),
            Box::new(ConstantQPS { qps: 1 }),
        );
        request_generator_stream(generator);
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
        assert_eq!("offset=0&limit=20",format!("{}",result));

        query = HashMap::new();
        query.insert("job_id".to_string(), "some-job-id".to_string());
        let result: JobStatusQueryParams = query.try_into().unwrap();
        println!("{:?}", &result);
        assert!(matches!(result, JobStatusQueryParams::JobId { job_id: _ }));
        assert_eq!("job_id=some-job-id",format!("{}",result));

        query = HashMap::new();
        query.insert("offset".to_string(), "0".to_string());
        query.insert("limit".to_string(), "20".to_string());
        query.insert("job_id".to_string(), "some-uuid".to_string());
        let result: Result<JobStatusQueryParams, GenericError> = query.try_into();
        println!("{:?}", &result);
        assert!(result.is_err());
    }
}
