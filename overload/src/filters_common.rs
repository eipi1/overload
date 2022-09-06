use http::header::CONTENT_TYPE;
use http::{Response, StatusCode};
use hyper::Body;
use lazy_static::lazy_static;
use overload::http_util::{GenericError, GenericResponse};
use overload::metrics::MetricsFactory;
use prometheus::{Encoder, TextEncoder};
use serde::Serialize;
use warp::{reply, Filter};

lazy_static! {
    pub static ref METRICS_FACTORY: MetricsFactory = MetricsFactory::default();
}

pub fn prometheus_metric(
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::get().and(warp::path("metrics")).map(|| {
        let encoder = TextEncoder::new();
        let metrics = METRICS_FACTORY.registry().gather();
        let mut resp_buffer = vec![];
        let result = encoder.encode(&metrics, &mut resp_buffer);
        if result.is_ok() {
            Response::builder()
                .status(200)
                .header(CONTENT_TYPE, encoder.format_type())
                .body(Body::from(resp_buffer))
                .unwrap()
        } else {
            Response::builder()
                .status(500)
                .body(Body::from("Error exporting metrics"))
                .unwrap()
        }
    })
}

pub fn generic_result_to_reply_with_status<T: Serialize>(
    status: Result<GenericResponse<T>, GenericError>,
) -> reply::WithStatus<reply::Json> {
    match status {
        Ok(resp) => reply::with_status(reply::json(&resp), StatusCode::OK),
        Err(err) => generic_error_to_reply_with_status(err),
    }
}

pub fn generic_error_to_reply_with_status(err: GenericError) -> reply::WithStatus<reply::Json> {
    let status_code = err.error_code;
    reply::with_status(
        reply::json(&err),
        StatusCode::from_u16(status_code).unwrap(),
    )
}

#[cfg(test)]
pub(crate) mod test_common {
    use crate::filters_common;
    use hyper::{Body, Request};
    use prometheus::Encoder;
    use std::sync::Once;
    use tokio::sync::OnceCell;

    pub async fn init_http_mock() -> (httpmock::MockServer, url::Url) {
        let mock_server = httpmock::MockServer::start_async().await;
        let url = url::Url::parse(&mock_server.base_url()).unwrap();
        (mock_server, url)
    }

    pub static ASYNC_ONCE_HTTP_MOCK: OnceCell<(httpmock::MockServer, url::Url)> =
        OnceCell::const_new();

    static ONCE: Once = Once::new();

    pub fn setup() {
        ONCE.call_once(|| {
            // tracing_subscriber::fmt()
            //     .with_env_filter("trace")
            //     .try_init()
            //     .unwrap();

            let _ = tracing_subscriber::fmt()
                .with_env_filter(format!(
                    "overload={},rust_cloud_discovery={},cloud_discovery_kubernetes={},cluster_mode={},\
                    almost_raft={}, hyper={}",
                    "trace", "info", "info", "info", "info", "info"
                ))
                .try_init();
        });
    }

    pub fn send_request(body: String) -> hyper::client::ResponseFuture {
        let client = hyper::Client::new();
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1:3030/test")
            .body(Body::from(body))
            .expect("request builder");
        client.request(req)
    }

    pub fn json_request_random_constant(host: String, port: u16) -> String {
        let req = serde_json::json!(
        {
            "duration": 5,
            "req": {
              "RandomDataRequest": {
                "url": "/anything/{param1}/{param2}",
                "method": "GET",
                "uriParamSchema": {
                  "type": "object",
                  "properties": {
                    "param1": {
                      "type": "string",
                      "description": "The person's first name.",
                      "minLength": 6,
                      "maxLength": 15
                    },
                    "param2": {
                      "description": "Age in years which must be equal to or greater than zero.",
                      "type": "integer",
                      "minimum": 1000000,
                      "maximum": 1100000
                    }
                  }
                },
                "headers": {
                    "Connection":"keep-alive"
                }
              }
            },
            "target": {
                "host":host,
                "port": port,
                "protocol": "HTTP"
            },
            "qps": {
              "ConstantRate": {
                "countPerSec": 3
              }
            },
            "concurrentConnection": {
                "ConstantRate": {
                    "countPerSec": 3
                  }
            }
          }
        );
        req.to_string()
    }

    #[allow(dead_code)]
    fn get_metrics() -> String {
        let encoder = prometheus::TextEncoder::new();
        let metrics = filters_common::METRICS_FACTORY.registry().gather();
        let mut resp_buffer = vec![];
        let _result = encoder.encode(&metrics, &mut resp_buffer);
        String::from_utf8(resp_buffer).unwrap()
    }

    #[allow(dead_code)]
    fn get_value_for_metrics(metrics_name: &str, metrics: &str) -> i32 {
        // let lines = metrics.as_str().lines();
        for metric in metrics.lines() {
            if metric.starts_with(metrics_name) {
                return metric
                    .rsplit_once(' ')
                    .map_or(-1, |(_, count)| count.parse::<i32>().unwrap());
            }
        }
        -1
    }
}
