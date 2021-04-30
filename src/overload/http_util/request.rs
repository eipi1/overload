#![allow(clippy::upper_case_acronyms)]

use crate::generator::{ArrayQPS, ConstantQPS, Linear, RequestGenerator, RequestSpec, QPSScheme, RequestList, RequestFile};
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

/// Describe the request, for example
/// ```JSON
/// {
///     "name": null,
///     "duration": 2,
///     "req": [{
///           "method": "GET",
///           "url": "http://localhost:8080/random",
///           "body": null
///         },
///         {
///           "method": "GET",
///           "url": "http://example.com",
///           "body": null
///         }
///
///     ],
///     "qps": {
///       "ConstantQPS": {
///         "qps": 3
///       }
///     }
///   }
/// ```
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub name: Option<String>,
    pub(crate) duration: u32,
    pub(crate) req: Vec<HttpReq>,
    pub(crate) qps: QPSSpec,
}

#[derive(Deserialize)]
pub struct RequestGeneric {
    pub name: Option<String>,
    pub(crate) duration: u32,
    // #[serde(bound = "T: crate::generator::RequestSpec+ serde::Deserialize<'de>")]
    // pub(crate) req: Box<dyn RequestSpec>,
    pub(crate) req: RequestSpecEnum,
    pub(crate) qps: QPSSpec,
}

impl Into<RequestGenerator> for RequestGeneric {
    fn into(self) -> RequestGenerator {
        let x:Box<dyn QPSScheme+Send> = match self.qps {
            QPSSpec::ConstantQPS(qps) => { Box::new(qps) }
            QPSSpec::Linear(qps) => { Box::new(qps) }
            QPSSpec::ArrayQPS(qps) => { Box::new(qps) }
        };
        let req:Box<dyn RequestSpec+Send> = match self.req {
            RequestSpecEnum::RequestList(req) => { Box::new(req) }
            RequestSpecEnum::RequestFile(req) => { Box::new(req) }
        };
        RequestGenerator::new(self.duration, req , x)
    }
}

#[derive(Debug, Deserialize)]
pub struct PagerOptions {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[cfg(test)]
mod test {
    use crate::generator::{request_generator_stream, ConstantQPS, RequestGenerator, RequestSpec};
    use crate::http_util::request::{QPSSpec, Request, RequestGeneric};
    use crate::HttpReq;
    use std::collections::HashMap;
    use uuid::Uuid;

    #[test]
    fn test_enum_serde() {
        let req = sample_request();
        let req = serde_json::to_string(&req);
        assert!(req.is_ok());

        let req = r#"
  {
    "name": null,
    "duration": 1,
    "req": [{
          "method": "GET",
          "url": "example.com",
          "body": null
        }
    ],
    "qps": {
      "ConstantQPS": {
        "qps": 1
      }
    }
  }
        "#;
        let result = serde_json::from_str::<Request>(req);
        // let result = serde_json::from_str::<RequestGeneric>(req);
        assert!(result.is_ok())
    }

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
        let result:RequestGeneric = serde_json::from_str(req).unwrap();
    }

    #[tokio::test]
    #[allow(unused_must_use)]
    async fn test_req_param() {
        let req = sample_request();
        let mut qps = 0;
        if let QPSSpec::ConstantQPS(_qps) = req.qps {
            qps = _qps.qps;
        }

        let generator =
            RequestGenerator::new(3, req.req, Box::new(ConstantQPS { qps: qps as u32 }));
        request_generator_stream(generator);
    }

    fn sample_request() -> Request {
        Request {
            req: vec![HttpReq {
                id: Uuid::new_v4().to_string(),
                body: None,
                url: "example.com".to_string(),
                method: http::Method::GET,
                headers: HashMap::new(),
            }],
            qps: QPSSpec::ConstantQPS(ConstantQPS { qps: 1 }),
            duration: 1,
            name: None,
        }
    }
}
