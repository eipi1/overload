#![allow(clippy::upper_case_acronyms)]

use crate::generator::{
    ArrayQPS, ConstantQPS, Linear, QPSScheme, RequestFile, RequestGenerator, RequestList,
    RequestProvider,
};
use crate::HttpReq;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum QPSSpec {
    ConstantQPS(ConstantQPS),
    Linear(Linear),
    ArrayQPS(ArrayQPS),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum RequestSpecEnum {
    RequestList(RequestList),
    RequestFile(RequestFile),
}

impl From<Vec<HttpReq>> for RequestSpecEnum {
    fn from(req: Vec<HttpReq>) -> Self {
        RequestSpecEnum::RequestList(RequestList { data: req })
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
            RequestSpecEnum::RequestFile(req) => Box::new(req),
        };
        RequestGenerator::new(self.duration, req, qps)
    }
}

#[derive(Debug, Deserialize)]
pub struct PagerOptions {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[cfg(test)]
mod test {
    use crate::generator::test::req_list_with_n_req;
    use crate::generator::{request_generator_stream, ConstantQPS, RequestGenerator};
    use crate::http_util::request::Request;

    #[test]
    fn deserialize_str() {
        let req = r#"
          {
            "name": null,
            "duration": 1,
            "req":
            {"RequestList":{
            "data": [{
                  "method": "GET",
                  "url": "example.com",
                  "body": null
                }
            ]
            }}
            ,
            "qps": {
              "ConstantQPS": {
                "qps": 1
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
}
