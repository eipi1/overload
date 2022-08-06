#![allow(clippy::upper_case_acronyms)]

use crate::generator::Target;
use crate::generator::{
    ArraySpec, Bounded, ConstantRate, Linear, RandomDataRequest, RateScheme, RequestFile,
    RequestGenerator, RequestList, RequestProvider,
};
use crate::http_util::GenericError;
use crate::{data_dir, fmt, HttpReq};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fmt::{Debug, Display, Formatter};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum RateSpec {
    ConstantRate(ConstantRate),
    Linear(Linear),
    ArraySpec(ArraySpec),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ConcurrentConnectionRateSpec {
    ConstantRate(ConstantRate),
    Linear(Linear),
    ArraySpec(ArraySpec),
    Bounded(Bounded),
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum RequestSpecEnum {
    RequestList(RequestList),
    RequestFile(RequestFile),
    RandomDataRequest(RandomDataRequest),
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
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub name: Option<String>,
    pub(crate) duration: u32,
    pub(crate) target: Target,
    pub(crate) req: RequestSpecEnum,
    pub(crate) qps: RateSpec,
    pub(crate) concurrent_connection: Option<ConcurrentConnectionRateSpec>,
    #[serde(default = "crate::metrics::default_histogram_bucket")]
    pub(crate) histogram_buckets: SmallVec<[f64; 6]>,
}

#[allow(clippy::from_over_into)]
impl Into<RequestGenerator> for Request {
    fn into(self) -> RequestGenerator {
        let qps: Box<dyn RateScheme + Send> = match self.qps {
            RateSpec::ConstantRate(qps) => Box::new(qps),
            RateSpec::Linear(qps) => Box::new(qps),
            RateSpec::ArraySpec(qps) => Box::new(qps),
        };
        let req: Box<dyn RequestProvider + Send> = match self.req {
            RequestSpecEnum::RequestList(req) => Box::new(req),
            RequestSpecEnum::RequestFile(mut req) => {
                //todo why rename here?
                req.file_name = format!("{}/{}.sqlite", data_dir(), &req.file_name);
                Box::new(req)
            }
            RequestSpecEnum::RandomDataRequest(req) => Box::new(req),
        };

        let connection_rate = if let Some(connection_rate_spec) = self.concurrent_connection {
            let rate_spec: Box<dyn RateScheme + Send> = match connection_rate_spec {
                ConcurrentConnectionRateSpec::ArraySpec(spec) => Box::new(spec),
                ConcurrentConnectionRateSpec::ConstantRate(spec) => Box::new(spec),
                ConcurrentConnectionRateSpec::Linear(spec) => Box::new(spec),
                ConcurrentConnectionRateSpec::Bounded(spec) => Box::new(spec),
            };
            Some(rate_spec)
        } else {
            None
        };

        RequestGenerator::new(self.duration, req, qps, self.target, connection_rate)
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
                GenericError::new(&*format!("Invalid offset {}, {}", &offset, e), 400)
            })?;
            let limit = value.remove("limit");
            let limit = limit.as_ref().map_or("20", |l| &*l);
            let limit = limit.parse::<usize>().map_err(|e| {
                GenericError::new(&*format!("Invalid limit {}, {}", &offset, e), 400)
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

#[cfg(test)]
mod test {
    use crate::generator::test::req_list_with_n_req;
    use crate::generator::Scheme;
    use crate::generator::Target;
    use crate::generator::{request_generator_stream, ConstantRate, RequestGenerator, RequestList};
    use crate::http_util::request::{JobStatusQueryParams, Request, RequestSpecEnum};
    use crate::http_util::GenericError;
    use crate::HttpReq;
    use http::Method;
    use std::collections::HashMap;
    use std::convert::TryInto;

    #[test]
    fn deserialize_str() {
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
              "histogramBuckets": [35,40,45,48,50, 52]
            }
        "#;
        let result = serde_json::from_str::<Request>(req);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(
            result.histogram_buckets,
            smallvec::SmallVec::from([35f64, 40f64, 45f64, 48f64, 50f64, 52f64])
        );
        let req = HttpReq {
            id: {
                if let RequestSpecEnum::RequestList(r) = &result.req {
                    r.data.get(0).unwrap().id.clone()
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
            serde_json::to_value(result.req).unwrap(),
            serde_json::to_value(RequestSpecEnum::RequestList(RequestList::from(vec![req])))
                .unwrap()
        );
    }

    #[tokio::test]
    #[allow(unused_must_use)]
    async fn test_req_param() {
        let generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(1)),
            Box::new(ConstantRate { count_per_sec: 1 }),
            Target {
                host: "example.com".into(),
                port: 8080,
                protocol: Scheme::HTTP,
            },
            None,
        );
        request_generator_stream(generator);
    }

    #[tokio::test]
    async fn test_request_1() {
        let req = r#"
            {
              "duration": 5,
              "name": "demo-test",
              "qps": {
                "ConstantRate": {
                  "countPerSec": 2
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
              "histogramBuckets": [35,40,45,48,50, 52]
            }
        "#;
        let result = serde_json::from_str::<Request>(req);
        match result {
            Err(err) => {
                panic!("Error: {}", err);
            }
            Ok(request) => {
                let _ = request_generator_stream(request.into());
            }
        }
    }

    #[tokio::test]
    async fn test_request_2() {
        let req = r#"
            {
              "duration": 5,
              "name": "demo-test",
              "qps": {
                "ConstantRate": {
                  "countPerSec": 2
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
              "concurrentConnections": {
                "max":100
              },
              "histogramBuckets": [35,40,45,48,50, 52]
            }
        "#;
        let result = serde_json::from_str::<Request>(req);
        match result {
            Err(err) => {
                panic!("Error: {}", err);
            }
            Ok(request) => {
                let _ = request_generator_stream(request.into());
            }
        }
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
}
