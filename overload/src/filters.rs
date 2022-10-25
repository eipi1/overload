use crate::filters_common;
use bytes::Buf;
use futures_util::Stream;
use http::StatusCode;
use log::{debug, trace};
use overload::http_util::handle_history_all;
use overload::http_util::request::{JobStatusQueryParams, MultiRequest, Request};
use overload::METRICS_FACTORY;
use overload::{data_dir, http_util};
use std::collections::HashMap;
use std::convert::{Infallible, TryFrom};
use warp::{reply, Filter, Reply};

pub fn get_routes() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let prometheus_metric = filters_common::prometheus_metric();
    let upload_binary_file = upload_binary_file();
    let stop_req = stop_req();
    let history = history();
    let overload_req = overload_req();
    let overload_multi_req = overload_multi_req();
    prometheus_metric
        .or(overload_req)
        .or(overload_multi_req)
        .or(stop_req)
        .or(history)
        .or(upload_binary_file)
}

pub fn overload_req() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(warp::path("test").and(warp::path::end()))
        .and(warp::body::content_length_limit(1024 * 1024))
        .and(warp::body::json())
        .and_then(|request: Request| async move { execute(request).await })
}

pub fn overload_multi_req(
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(warp::path("tests").and(warp::path::end()))
        .and(warp::body::content_length_limit(1024 * 1024))
        .and(warp::body::json())
        .and_then(|request: MultiRequest| async move { execute_multiple(request).await })
}

pub fn upload_binary_file(
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(
            warp::path("test")
                .and(warp::path("requests-bin"))
                .and(warp::path::end()),
        )
        .and(warp::body::content_length_limit(1024 * 1024 * 32))
        .and(warp::body::stream())
        .and_then(upload_binary_file_handler)
}

pub fn stop_req() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("test" / "stop" / String)
        .and_then(|job_id: String| async move { stop(job_id).await })
}

pub fn history() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("test" / "status")
        // should replace with untagged enum JobStatusQueryParams, but doesn't work due to
        // https://github.com/nox/serde_urlencoded/issues/66
        .and(warp::query::<HashMap<String, String>>())
        .and_then(|pager_option: HashMap<String, String>| async move {
            let option = JobStatusQueryParams::try_from(pager_option);
            let result: Result<reply::WithStatus<reply::Json>, Infallible> = match option {
                Ok(option) => {
                    trace!("req: all_job: {:?}", &option);
                    let status = handle_history_all(option).await;
                    trace!("resp: all_job: {:?}", &status);
                    Ok(filters_common::generic_result_to_reply_with_status(status))
                }
                Err(e) => Ok(filters_common::generic_error_to_reply_with_status(e)),
            };
            result
        })
}

/// should use shared storage, no need to forward upload request
// #[allow(clippy::clippy::explicit_write)]
async fn upload_binary_file_handler<S, B>(data: S) -> Result<impl Reply, Infallible>
where
    S: Stream<Item = Result<B, warp::Error>> + Unpin + Send + Sync,
    B: Buf + Send + Sync,
{
    let data_dir = data_dir();
    let result = http_util::csv_stream_to_sqlite(data, &*data_dir).await;
    match result {
        Ok(r) => Ok(reply::with_status(reply::json(&r), StatusCode::OK)),
        Err(e) => Ok(reply::with_status(
            reply::json(&e),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

async fn stop(job_id: String) -> Result<impl Reply, Infallible> {
    let resp = overload::http_util::stop(job_id).await;
    trace!("resp: stop: {:?}", &resp);
    Ok(filters_common::generic_result_to_reply_with_status(resp))
}

pub async fn execute(request: Request) -> Result<impl Reply, Infallible> {
    trace!("req: execute: {:?}", &request);
    let response = overload::http_util::handle_request(request, &METRICS_FACTORY).await;
    let json = reply::json(&response);
    trace!("resp: execute: {:?}", &response);
    Ok(json)
}

pub async fn execute_multiple(requests: MultiRequest) -> Result<impl Reply, Infallible> {
    debug!("req: execute_multiple: {:?}", &requests);
    let response = overload::http_util::handle_multi_request(requests, &METRICS_FACTORY).await;
    let json = reply::json(&response);
    debug!("resp: execute_multiple: {:?}", &response);
    Ok(json)
}

#[cfg(all(test, not(feature = "cluster")))]
mod standalone_mode_tests {
    use crate::filters_common::test_common::*;
    use httpmock::prelude::*;
    use log::{info, trace};
    use more_asserts::{assert_ge, assert_gt};
    use regex::Regex;
    use serde_json::json;
    use wiremock::MockServer;

    pub async fn init_env() -> (MockServer, url::Url, tokio::sync::oneshot::Sender<()>, u16) {
        let wire_mock = wiremock::MockServer::start().await;
        let wire_mock_uri = wire_mock.uri();
        let url = url::Url::parse(&wire_mock_uri).unwrap();

        let route = super::get_routes();

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let port = portpicker::pick_unused_port().unwrap();
        let (_addr, server) =
            warp::serve(route).bind_with_graceful_shutdown(([127, 0, 0, 1], port), async {
                rx.await.ok();
            });
        // Spawn the server into a runtime
        tokio::task::spawn(server);

        (wire_mock, url, tx, port)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_request_random_constant() {
        setup();
        let (_, _, tx, port) = init_env().await;

        let (mock_server, url) = ASYNC_ONCE_HTTP_MOCK.get_or_init(init_http_mock).await;

        let mock = mock_server.mock(|when, then| {
            when.method(GET)
                .path_matches(Regex::new(r"^/anything/[a-zA-Z]{6,15}/1(1|0)\d{5}$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .header("Connection", "keep-alive")
                .body(r#"{"hello": "world"}"#);
        });

        let response = send_request(
            json_request_random_constant(url.host().unwrap().to_string(), url.port().unwrap()),
            port,
        )
        .await
        .unwrap();
        println!("{:?}", response);
        let status = response.status();
        println!("body: {:?}", hyper::body::to_bytes(response).await.unwrap());
        assert_eq!(status, 200);

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        for _i in 1..6 {
            //each seconds we expect mock to receive 3 request
            // assert_eq!(i * 3, mock.hits_async().await); // Test with github action is failing, but pass on ide
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
        assert_eq!(15, mock.hits_async().await);
        mock.delete_async().await;
        //Inconsistent result - it fails sometimes, hence commenting out
        //let metrics = get_metrics();
        // assert_eq!(get_value_for_metrics("connection_pool_idle_connections", &metrics), 3);
        let _ = tx.send(());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_request_list_constant() {
        setup();
        let (_, _, tx, port) = init_env().await;

        let (mock_server, url) = ASYNC_ONCE_HTTP_MOCK.get_or_init(init_http_mock).await;

        let mock = mock_server.mock(|when, then| {
            when.method(GET)
                .path_matches(Regex::new(r"^/list/data$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"hello": "list"}"#);
        });

        let response = send_request(
            json_request_list_constant(
                url.host().unwrap().to_string(),
                url.port().unwrap(),
                3,
                "/list/data",
            ),
            port,
        )
        .await
        .unwrap();
        println!("{:?}", response);
        let status = response.status();
        println!("body: {:?}", hyper::body::to_bytes(response).await.unwrap());
        assert_eq!(status, 200);
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        for i in 1..6 {
            //each seconds we expect mock to receive 3 request
            assert_eq!(i * 3, mock.hits_async().await);
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
        mock.delete_async().await;
        let _ = tx.send(());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_request_list_constant_with_connection_lt_qps() {
        setup();
        let (_, _, tx, port) = init_env().await;

        let (mock_server, url) = ASYNC_ONCE_HTTP_MOCK.get_or_init(init_http_mock).await;

        let mock = mock_server.mock(|when, then| {
            when.method(GET)
                .path_matches(Regex::new(r"^/list/data2$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"hello": "list"}"#);
        });

        let response = send_request(
            json_request_list_constant(
                url.host().unwrap().to_string(),
                url.port().unwrap(),
                1,
                "/list/data2",
            ),
            port,
        )
        .await
        .unwrap();
        println!("{:?}", response);
        let status = response.status();
        println!("body: {:?}", hyper::body::to_bytes(response).await.unwrap());
        assert_eq!(status, 200);
        tokio::time::sleep(tokio::time::Duration::from_millis(990)).await;
        for i in 1..=5 {
            // each seconds we expect mock to receive 3 request
            assert_ge!(i * 3, mock.hits_async().await);
            trace!("mock hits: {}", mock.hits_async().await);
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
        }
        mock.delete_async().await;
        let _ = tx.send(());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_request_with_assertion_failure() {
        setup();
        let (_, _, tx, port) = init_env().await;

        let (mock_server, url) = ASYNC_ONCE_HTTP_MOCK.get_or_init(init_http_mock).await;

        let mock = mock_server.mock(|when, then| {
            when.method(GET)
                .path_matches(Regex::new(r"^/list/data2$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"hello": "list"}"#);
        });

        let response = send_request(
            json_request_list_constant_with_assertion(
                url.host().unwrap().to_string(),
                url.port().unwrap(),
                1,
                "/list/data2",
            ),
            port,
        )
        .await
        .unwrap();
        println!("{:?}", response);
        let status = response.status();
        println!("body: {:?}", hyper::body::to_bytes(response).await.unwrap());
        assert_eq!(status, 200);
        tokio::time::sleep(tokio::time::Duration::from_millis(2100)).await;
        let metrics = crate::filters_common::test_common::get_metrics();
        info!("{}", &metrics);
        assert_gt!(get_value_for_metrics("assertion_failure", &metrics), 1);
        tokio::time::sleep(tokio::time::Duration::from_millis(3100)).await;
        mock.delete_async().await;
        let _ = tx.send(());
    }

    fn json_request_list_constant_with_assertion(
        host: String,
        port: u16,
        connection_count: u16,
        url: &str,
    ) -> String {
        let req = json!(
        {
            "duration": 5,
            "req": {
                "RequestList": {
                    "data": [
                      {
                        "method": "GET",
                        "url": url
                      }
                    ]
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
                    "countPerSec": connection_count
                  }
            },
            "responseAssertion": {
              "assertions": [
                {
                  "id": 1,
                  "expectation": {
                    "Constant": "world"
                  },
                  "actual": {
                    "FromJsonResponse": {
                      "path": "$.hello"
                    }
                  }
                },
                {
                    "id": 2,
                    "expectation": {
                      "Constant": {
                        "hello": "list"
                      }
                    },
                    "actual": {
                      "FromJsonResponse": {
                        "path": "$"
                      }
                    }
                  }
              ]
            }
          }
        );
        req.to_string()
    }

    fn json_request_list_constant(
        host: String,
        port: u16,
        connection_count: u16,
        url: &str,
    ) -> String {
        let req = json!(
        {
            "duration": 5,
            "req": {
                "RequestList": {
                    "data": [
                      {
                        "method": "GET",
                        "url": url
                      }
                    ]
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
                    "countPerSec": connection_count
                  }
            }
          }
        );
        req.to_string()
    }
}
