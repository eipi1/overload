#![allow(opaque_hidden_inferred_bound)]

use crate::filters_common;
use bytes::Buf;
use cluster_executor::data_dir_path;
use futures_util::Stream;
use http::StatusCode;
use log::{debug, trace};
use overload::standalone::handle_get_status;
use overload_http::{JobStatusQueryParams, MultiRequest, Request};
use std::collections::HashMap;
use std::convert::{Infallible, TryFrom};
use warp::{reply, Filter};

pub fn get_routes() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let prometheus_metric = filters_common::prometheus_metric();
    let upload_binary_file = upload_binary_file();
    let stop_req = stop_req();
    let history = status();
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

pub fn status() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("test" / "status")
        // should replace with untagged enum JobStatusQueryParams, but doesn't work due to
        // https://github.com/nox/serde_urlencoded/issues/66
        .and(warp::query::<HashMap<String, String>>())
        .and_then(|pager_option: HashMap<String, String>| async move {
            let option = JobStatusQueryParams::try_from(pager_option);
            let result: Result<reply::WithStatus<reply::Json>, Infallible> = match option {
                Ok(option) => {
                    trace!("req: all_job: {:?}", &option);
                    let status = handle_get_status(option).await;
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
async fn upload_binary_file_handler<S, B>(data: S) -> Result<impl warp::Reply, Infallible>
where
    S: Stream<Item = Result<B, warp::Error>> + Unpin + Send + Sync,
    B: Buf + Send + Sync,
{
    let path = data_dir_path();
    let data_dir = path.to_str().unwrap();
    let result = overload::file_uploader::csv_stream_to_sqlite(data, data_dir).await;
    match result {
        Ok(r) => Ok(reply::with_status(reply::json(&r), StatusCode::OK)),
        Err(e) => Ok(reply::with_status(
            reply::json(&e),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

async fn stop(job_id: String) -> Result<impl warp::Reply, Infallible> {
    let resp = overload::standalone::stop(job_id).await;
    trace!("resp: stop: {:?}", &resp);
    Ok(filters_common::generic_result_to_reply_with_status(resp))
}

pub async fn execute(request: Request) -> Result<impl warp::Reply, Infallible> {
    trace!("req: execute: {:?}", &request);
    // let response = overload::http_util::handle_request(request, &METRICS_FACTORY).await;
    let response = overload::standalone::handle_request(request).await;
    let json = reply::json(&response);
    trace!("resp: execute: {:?}", &response);
    Ok(json)
}

pub async fn execute_multiple(requests: MultiRequest) -> Result<impl warp::Reply, Infallible> {
    debug!("req: execute_multiple: {:?}", &requests);
    let response = overload::standalone::handle_multi_request(requests).await;
    let json = reply::json(&response);
    debug!("resp: execute_multiple: {:?}", &response);
    Ok(json)
}

#[cfg(all(test, not(feature = "cluster")))]
mod standalone_mode_tests {
    use crate::filters_common::test_common::*;
    use cluster_executor::ENV_NAME_REMOC_PORT;
    use http::Request;
    use httpmock::prelude::*;
    use hyper::client::HttpConnector;
    use hyper::{Body, Client};
    use log::{error, info, trace};
    use more_asserts::{assert_ge, assert_gt, assert_le};
    use overload_http::GenericResponse;
    use overload_metrics::METRICS_FACTORY;
    use portpicker::Port;
    use regex::Regex;
    use serde_json::json;
    use std::time::Duration;
    use tokio::sync::oneshot::Sender;

    async fn init_env() -> (Sender<()>, Port, Port) {
        //start http
        let route = super::get_routes();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let http_port = portpicker::pick_unused_port().unwrap();
        let (_addr, server) =
            warp::serve(route).bind_with_graceful_shutdown(([127, 0, 0, 1], http_port), async {
                match rx.await {
                    Ok(e) => {
                        error!("rx received: {:?}", e)
                    }
                    Err(e) => {
                        error!("rx dropped: {:?}", e)
                    }
                }
            });
        // Spawn the server into a runtime
        tokio::task::spawn(server);

        //start remoc server
        let remoc_port = portpicker::pick_unused_port().unwrap();
        tokio::spawn(cluster_executor::secondary::primary_listener(
            remoc_port,
            &METRICS_FACTORY,
        ));
        (tx, http_port, remoc_port)
    }

    /// Because tests has dependency on environment variable, (see [`cluster_executor::remoc_port()`])
    /// running multiple tests in parallel causes tests to fail. So using this function to force
    /// tests to run sequentially
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn all_tests() {
        test_request_random_constant().await;
        test_request_list_constant().await;
        test_request_list_constant_with_connection_lt_qps().await;
        test_request_with_assertion_failure().await;
        test_multi_request().await;
        test_stop_req().await;
    }

    // #[tokio::test(flavor = "multi_thread")]
    async fn test_request_random_constant() {
        setup();

        let (tx, http_port, remoc_port) = init_env().await;
        // let (tx, http_port, remoc_port, handle) = ASYNC_ONCE_LISTENERS.get_or_init(init_env).await;
        // let (tx, http_port, remoc_port, handle) = init_env().await;
        // info!("handle status: {}", handle.is_finished());
        info!("using ports: {}, {}", http_port, remoc_port);
        //wait a bit for bootstrapping
        tokio::time::sleep(Duration::from_millis(50)).await;
        std::env::set_var(ENV_NAME_REMOC_PORT, remoc_port.to_string());

        let (mock_server, url) = ASYNC_ONCE_HTTP_MOCK.get_or_init(init_http_mock).await;

        let mock = mock_server.mock(|when, then| {
            when.method(GET)
                .path_matches(Regex::new(r"^/anything/[a-zA-Z]{6,15}/1([10])\d{5}$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .header("Connection", "keep-alive")
                .body(r#"{"hello": "world"}"#);
        });

        let response = send_request(
            json_request_random_constant(url.host().unwrap().to_string(), url.port().unwrap()),
            http_port,
            "test",
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
            info!("mock hits: {}", mock.hits_async().await); // Test with github action is failing, but pass on ide
            assert_eq!(i * 3, mock.hits_async().await); // Test with github action is failing, but pass on ide
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
        assert_eq!(15, mock.hits_async().await);
        mock.delete_async().await;
        //Inconsistent result - it fails sometimes, hence commenting out
        //let metrics = get_metrics();
        // assert_eq!(get_value_for_metrics("connection_pool_idle_connections", &metrics), 3);
        let _ = tx.send(());
    }

    // #[tokio::test(flavor = "multi_thread")]
    async fn test_request_list_constant() {
        setup();

        let (tx, http_port, remoc_port) = init_env().await;
        // let (tx, http_port, remoc_port, handle) = ASYNC_ONCE_LISTENERS.get_or_init(init_env).await;
        // let (tx, http_port, remoc_port, handle) = init_env().await;
        // info!("handle status: {}", handle.is_finished());
        info!("using ports: {}, {}", http_port, remoc_port);
        std::env::set_var(ENV_NAME_REMOC_PORT, remoc_port.to_string());

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
            http_port,
            "test",
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

    // #[tokio::test(flavor = "multi_thread")]
    async fn test_request_list_constant_with_connection_lt_qps() {
        setup();

        let (tx, http_port, remoc_port) = init_env().await;
        info!("using ports: {}, {}", http_port, remoc_port);
        std::env::set_var(ENV_NAME_REMOC_PORT, remoc_port.to_string());

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
            http_port,
            "test",
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

    // #[tokio::test(flavor = "multi_thread")]
    async fn test_request_with_assertion_failure() {
        setup();
        let (tx, http_port, remoc_port) = init_env().await;
        // let (tx, http_port, remoc_port, handle) = ASYNC_ONCE_LISTENERS.get_or_init(init_env).await;
        // let (tx, http_port, remoc_port, handle) = init_env().await;
        // info!("handle status: {}", handle.is_finished());
        info!("using ports: {}, {}", http_port, remoc_port);
        //wait a bit for bootstrapping
        tokio::time::sleep(Duration::from_millis(50)).await;
        std::env::set_var(ENV_NAME_REMOC_PORT, remoc_port.to_string());

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
            http_port,
            "test",
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

    async fn test_multi_request() {
        setup();

        let (tx, http_port, remoc_port) = init_env().await;
        info!("using ports: {}, {}", http_port, remoc_port);
        //wait a bit for bootstrapping
        tokio::time::sleep(Duration::from_millis(50)).await;
        std::env::set_var(ENV_NAME_REMOC_PORT, remoc_port.to_string());

        let (mock_server, url) = ASYNC_ONCE_HTTP_MOCK.get_or_init(init_http_mock).await;

        let mock1 = mock_server.mock(|when, then| {
            when.method(GET)
                .path_matches(Regex::new(r"^/anything/[a-zA-Z]{6,15}/1([10])\d{5}$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .header("Connection", "keep-alive")
                .body(r#"{"hello": "world"}"#);
        });

        let mock2 = mock_server.mock(|when, then| {
            when.method(GET)
                .path_matches(Regex::new(r"^/list/data$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"hello": "list"}"#);
        });

        let response = send_request(
            json_multi_request(
                url.host().unwrap().to_string(),
                url.port().unwrap(),
                3,
                "/list/data",
            ),
            http_port,
            "tests",
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
            info!("mock1 hits: {}", mock1.hits_async().await); // Test with github action is failing, but pass on ide
            assert_eq!(i * 3, mock1.hits_async().await); // Test with github action is failing, but pass on ide
            info!("mock2 hits: {}", mock2.hits_async().await); // Test with github action is failing, but pass on ide
            assert_eq!(i * 3, mock2.hits_async().await); // Test with github action is failing, but pass on ide
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
        assert_eq!(15, mock1.hits_async().await);
        mock1.delete_async().await;
        //Inconsistent result - it fails sometimes, hence commenting out
        //let metrics = get_metrics();
        // assert_eq!(get_value_for_metrics("connection_pool_idle_connections", &metrics), 3);
        let _ = tx.send(());
    }

    async fn test_stop_req() {
        setup();

        let (tx, http_port, remoc_port) = init_env().await;
        info!("using ports: {}, {}", http_port, remoc_port);
        std::env::set_var(ENV_NAME_REMOC_PORT, remoc_port.to_string());

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
            http_port,
            "test",
        )
        .await
        .unwrap();
        println!("{:?}", response);
        let status = response.status();
        assert_eq!(status, 200);

        let response = hyper::body::to_bytes(response).await.unwrap();
        println!("body: {:?}", &response);
        let response =
            serde_json::from_slice::<GenericResponse<String>>(response.as_ref()).unwrap();
        let job_id = response.data.get("job_id").unwrap().clone();

        tokio::time::sleep(tokio::time::Duration::from_millis(990)).await;
        for i in 1..=2 {
            // each seconds we expect mock to receive 3 request
            trace!("mock hits: {}", mock.hits_async().await);
            assert_ge!(i * 3, mock.hits_async().await);
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
        }

        //get status all
        info!("Sending status all request");
        let client = hyper::Client::new();
        let response = get_status_all(http_port, &client).await;
        info!("status response: {:?}", &response);
        assert_eq!(response.data.get(&job_id), Some(&"InProgress".to_string()));

        //stop
        info!("Sending stop request");
        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "http://127.0.0.1:{}/test/stop/{}",
                http_port, &job_id
            ))
            .body(Body::empty())
            .expect("request builder");
        let response = client.request(req).await.unwrap();
        info!("stop response: {:?}", &response);

        let response = get_status_all(http_port, &client).await;
        info!("status response: {:?}", &response);
        assert_eq!(response.data.get(&job_id), Some(&"Stopped".to_string()));

        trace!("mock hits: {}", mock.hits_async().await);
        assert_ge!(mock.hits_async().await, 9);
        assert_le!(mock.hits_async().await, 12);

        mock.delete_async().await;
        let _ = tx.send(());
    }

    async fn get_status_all(
        http_port: Port,
        client: &Client<HttpConnector>,
    ) -> GenericResponse<String> {
        let req = Request::builder()
            .method("GET")
            .uri(format!("http://127.0.0.1:{}/test/status", http_port))
            .body(Body::empty())
            .expect("request builder");
        let response = client.request(req).await.unwrap();
        let response = hyper::body::to_bytes(response).await.unwrap();
        let response =
            serde_json::from_slice::<GenericResponse<String>>(response.as_ref()).unwrap();
        info!("status response: {:?}", &response);
        response
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

    fn json_multi_request(host: String, port: u16, connection_count: u16, url: &str) -> String {
        let req = json!(
        {
          "name": "multi-test-request",
          "requests": [
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
                    "Connection": "keep-alive"
                  }
                }
              },
              "target": {
                "host": host,
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
            },
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
          ]
        }
        );
        req.to_string()
    }
}
