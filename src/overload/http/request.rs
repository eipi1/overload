use crate::generator::{ArrayQPS, ConstantQPS, Linear, RequestGenerator};
use crate::HttpReq;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
// #[serde(tag = "type", content = "spec", rename_all = "camelCase")]
pub(crate) enum QPSSpec {
    ConstantQPS(ConstantQPS),
    Linear(Linear),
    ArrayQPS(ArrayQPS),
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

impl Into<RequestGenerator> for Request {
    fn into(self) -> RequestGenerator {
        match self.qps {
            QPSSpec::ConstantQPS(_qps) => {
                RequestGenerator::new(self.duration, self.req, Box::new(_qps))
            }
            QPSSpec::Linear(_qps) => RequestGenerator::new(self.duration, self.req, Box::new(_qps)),
            QPSSpec::ArrayQPS(_qps) => {
                RequestGenerator::new(self.duration, self.req, Box::new(_qps))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PagerOptions {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[cfg(test)]
mod test {
    use crate::generator::{request_generator_stream, ConstantQPS, RequestGenerator};
    use crate::http::request::{QPSSpec, Request};
    use crate::{HttpReq, ReqMethod};
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
        assert!(result.is_ok())
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
            // req: ReqSpecType::SingleReqSpec { req: HttpReq { body: None, url: "example.com".to_string(), method: ReqMethod::GET } },
            req: vec![HttpReq {
                id: Uuid::new_v4().to_string(),
                body: None,
                url: "example.com".to_string(),
                method: ReqMethod::GET,
            }],
            qps: QPSSpec::ConstantQPS(ConstantQPS { qps: 1 }),
            duration: 1,
            name: None,
        }
    }
}
