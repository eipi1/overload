#![allow(unused_imports)]

use bytes::Buf;
use cfg_if::cfg_if;
#[cfg(feature = "cluster")]
use cloud_discovery_kubernetes::KubernetesDiscoverService;
#[cfg(feature = "cluster")]
use cluster_mode::{get_cluster_info, Cluster};
use futures_util::Stream;
use http::StatusCode;
use hyper::header::CONTENT_TYPE;
use hyper::{Body, Response};
use lazy_static::lazy_static;
use log::{info, trace};
use overload::http_util::request::{JobStatusQueryParams, Request};
use overload::http_util::{handle_history_all, GenericError, GenericResponse};
use overload::metrics::MetricsFactory;
use overload::{data_dir, http_util};
use prometheus::{Encoder, TextEncoder};
#[cfg(feature = "cluster")]
use rust_cloud_discovery::DiscoveryClient;
use serde::Serialize;
#[cfg(feature = "cluster")]
use serde_json::Value;
use std::collections::HashMap;
use std::convert::{Infallible, TryFrom};
use std::env;
#[cfg(feature = "cluster")]
use std::sync::Arc;
#[cfg(feature = "cluster")]
use warp::reply::{Json, WithStatus};
use warp::{reply, Filter, Reply};

lazy_static! {
    static ref METRICS_FACTORY: MetricsFactory = MetricsFactory::default();
}

#[cfg(feature = "cluster")]
use crate::CLUSTER;

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

pub fn overload_req() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(warp::path("test").and(warp::path::end()))
        .and(warp::body::content_length_limit(1024 * 1024))
        .and(warp::body::json())
        .and_then(|request: Request| async move {
            cfg_if! {
                if #[cfg(feature = "cluster")] {
                    execute_cluster(request).await
                } else {
                    execute(request).await
                }
            }
        })
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
            all_job(pager_option).await
            // ()
        })
}

#[cfg(feature = "cluster")]
pub fn overload_req_secondary(
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(
            warp::path("cluster")
                .and(warp::path("test"))
                .and(warp::path::end()),
        )
        .and(warp::body::content_length_limit(1024 * 1024))
        .and(warp::body::json())
        .and_then(|request: Request| async move { execute(request).await })
}

#[cfg(feature = "cluster")]
pub fn info() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "info").and_then(|| {
        let tmp = CLUSTER.clone();
        // let mode=cluster_mode;
        async move { cluster_info(tmp, true).await }
    })
}

#[cfg(feature = "cluster")]
pub fn request_vote() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "raft" / "request-vote" / String / usize).and_then(|node_id, term| {
        let tmp = CLUSTER.clone();
        // let mode = cluster_up;
        async move { cluster_request_vote(tmp, true, node_id, term).await }
    })
}

#[cfg(feature = "cluster")]
pub fn request_vote_response(
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "raft" / "vote" / usize / bool).and_then(|term, vote| {
        let tmp = CLUSTER.clone();
        async move { cluster_request_vote_response(tmp, true, term, vote).await }
    })
}

#[cfg(feature = "cluster")]
pub fn heartbeat() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "raft" / "beat" / String / usize).and_then(|leader_node, term| {
        let tmp = CLUSTER.clone();
        async move { cluster_heartbeat(tmp, true, leader_node, term).await }
    })
}

async fn execute(request: Request) -> Result<impl Reply, Infallible> {
    trace!("req: execute: {:?}", &request);
    let response = overload::http_util::handle_request(request, &METRICS_FACTORY).await;
    let json = reply::json(&response);
    trace!("resp: execute: {:?}", &response);
    Ok(json)
}

#[cfg(feature = "cluster")]
async fn execute_cluster(request: Request) -> Result<impl Reply, Infallible> {
    trace!("req: execute_cluster: {:?}", &request);
    let response = overload::http_util::handle_request_cluster(request, CLUSTER.clone()).await;
    let json = reply::json(&response);
    trace!("resp: execute_cluster: {:?}", &response);
    Ok(json)
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

async fn all_job(option: HashMap<String, String>) -> Result<impl Reply, Infallible> {
    let option = JobStatusQueryParams::try_from(option);
    match option {
        Ok(option) => {
            trace!("req: all_job: {:?}", &option);
            let status = {
                #[cfg(feature = "cluster")]
                {
                    handle_history_all(option, CLUSTER.clone())
                }
                #[cfg(not(feature = "cluster"))]
                {
                    handle_history_all(option)
                }
            }
            .await;
            trace!("resp: all_job: {:?}", &status);
            Ok(generic_result_to_reply_with_status(status))
        }
        Err(e) => Ok(generic_error_to_reply_with_status(e)),
    }
}

async fn stop(job_id: String) -> Result<impl Reply, Infallible> {
    let resp = {
        #[cfg(not(feature = "cluster"))]
        {
            overload::http_util::stop(job_id)
        }
        #[cfg(feature = "cluster")]
        {
            overload::http_util::stop(job_id, CLUSTER.clone())
        }
    }
    .await;
    trace!("resp: stop: {:?}", &resp);
    Ok(generic_result_to_reply_with_status(resp))
}
#[cfg(feature = "cluster")]
async fn cluster_info(cluster: Arc<Cluster>, cluster_mode: bool) -> Result<impl Reply, Infallible> {
    if !cluster_mode {
        let err = no_cluster_err();
        Ok(err)
    } else {
        let info = get_cluster_info(cluster).await;
        let json = reply::with_status(reply::json(&info), StatusCode::OK);
        Ok(json)
    }
}

#[cfg(feature = "cluster")]
async fn cluster_request_vote(
    cluster: Arc<Cluster>,
    cluster_mode: bool,
    requester_node_id: String,
    term: usize,
) -> Result<impl Reply, Infallible> {
    trace!(
        "req: cluster_request_vote: requester: {}, term: {}",
        &requester_node_id,
        &term
    );
    if !cluster_mode {
        let err = no_cluster_err();
        Ok(err)
    } else {
        cluster
            .accept_raft_request_vote(requester_node_id, term)
            .await;
        let json = reply::with_status(reply::json(&Value::Null), StatusCode::OK);
        Ok(json)
    }
}

#[cfg(feature = "cluster")]
async fn cluster_request_vote_response(
    cluster: Arc<Cluster>,
    cluster_mode: bool,
    term: usize,
    vote: bool,
) -> Result<impl Reply, Infallible> {
    trace!(
        "req: cluster_request_vote_response: term: {}, vote: {}",
        &term,
        &vote
    );
    if !cluster_mode {
        let err = no_cluster_err();
        Ok(err)
    } else {
        cluster.accept_raft_request_vote_resp(term, vote).await;
        let json = reply::with_status(reply::json(&Value::Null), StatusCode::OK);
        Ok(json)
    }
}

#[cfg(feature = "cluster")]
async fn cluster_heartbeat(
    cluster: Arc<Cluster>,
    cluster_mode: bool,
    leader_node_id: String,
    term: usize,
) -> Result<impl Reply, Infallible> {
    trace!(
        "req: cluster_heartbeat: leader: {}, term: {}",
        &leader_node_id,
        &term
    );
    if !cluster_mode {
        let err = no_cluster_err();
        Ok(err)
    } else {
        cluster.accept_raft_heartbeat(leader_node_id, term).await;
        let json = reply::with_status(reply::json(&Value::Null), StatusCode::OK);
        Ok(json)
    }
}

fn generic_result_to_reply_with_status<T: Serialize>(
    status: Result<GenericResponse<T>, GenericError>,
) -> reply::WithStatus<reply::Json> {
    match status {
        Ok(resp) => reply::with_status(reply::json(&resp), StatusCode::OK),
        Err(err) => generic_error_to_reply_with_status(err),
    }
}

fn generic_error_to_reply_with_status(err: GenericError) -> reply::WithStatus<reply::Json> {
    let status_code = err.error_code;
    reply::with_status(
        reply::json(&err),
        StatusCode::from_u16(status_code).unwrap(),
    )
}

#[cfg(feature = "cluster")]
fn no_cluster_err() -> WithStatus<Json> {
    let error_msg: Value = serde_json::from_str("{\"error\":\"Cluster is not running\"}").unwrap();
    reply::with_status(reply::json(&error_msg), StatusCode::NOT_FOUND)
}

#[cfg(test)]
mod integration_tests {
    use httpmock::prelude::*;
    use hyper::{Body, Client, Request};
    use log::info;
    use regex::Regex;
    use serde_json::json;
    use std::sync::Once;
    use tokio::sync::OnceCell;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn init_env() -> (MockServer, url::Url, tokio::sync::oneshot::Sender<()>) {
        let wire_mock = wiremock::MockServer::start().await;
        let wire_mock_uri = wire_mock.uri();
        let url = url::Url::parse(&wire_mock_uri).unwrap();

        let route = super::overload_req();

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let (_addr, server) =
            warp::serve(route).bind_with_graceful_shutdown(([127, 0, 0, 1], 3030), async {
                rx.await.ok();
            });
        // Spawn the server into a runtime
        tokio::task::spawn(server);

        (wire_mock, url, tx)
    }

    static ASYNC_ONCE: OnceCell<(MockServer, url::Url, tokio::sync::oneshot::Sender<()>)> =
        OnceCell::const_new();

    async fn init_http_mock() -> (httpmock::MockServer, url::Url) {
        let mock_server = httpmock::MockServer::start_async().await;
        let url = url::Url::parse(&mock_server.base_url()).unwrap();
        (mock_server, url)
    }

    static ASYNC_ONCE_HTTP_MOCK: OnceCell<(httpmock::MockServer, url::Url)> = OnceCell::const_new();

    static ONCE: Once = Once::new();

    fn setup() {
        ONCE.call_once(|| {
            tracing_subscriber::fmt()
                .with_env_filter("trace")
                .try_init()
                .unwrap();
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_request_random_constant() {
        setup();
        let (_, _, _) = ASYNC_ONCE.get_or_init(init_env).await;

        let (mock_server, url) = ASYNC_ONCE_HTTP_MOCK.get_or_init(init_http_mock).await;

        let mock = mock_server.mock(|when, then| {
            when.method(GET)
                .path_matches(Regex::new(r"^/anything/[a-zA-Z]{6,15}/1(1|0)\d{5}$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .header("Connection", "keep-alive")
                .body(r#"{"hello": "world"}"#);
        });

        let response = send_request(json_request_random_constant(
            url.host().unwrap().to_string(),
            url.port().unwrap(),
        ))
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
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_request_list_constant() {
        setup();
        let (_, _, _) = ASYNC_ONCE.get_or_init(init_env).await;

        let (mock_server, url) = ASYNC_ONCE_HTTP_MOCK.get_or_init(init_http_mock).await;

        let mock = mock_server.mock(|when, then| {
            when.method(GET)
                .path_matches(Regex::new(r"^/list/data$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"hello": "list"}"#);
        });

        let response = send_request(json_request_list_constant(
            url.host().unwrap().to_string(),
            url.port().unwrap(),
        ))
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
    }

    fn json_request_list_constant(host: String, port: u16) -> String {
        let req = json!(
        {
            "duration": 5,
            "req": {
                "RequestList": {
                    "data": [
                      {
                        "method": "GET",
                        "url": "/list/data"
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
                "qps": 3
              }
            }
          }
        );
        req.to_string()
    }

    fn send_request(body: String) -> hyper::client::ResponseFuture {
        let client = Client::new();
        let req = Request::builder()
            .method("POST")
            .uri("http://localhost:3030/test")
            .body(Body::from(body))
            .expect("request builder");
        client.request(req)
    }

    fn json_request_random_constant(host: String, port: u16) -> String {
        let req = json!(
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
                "qps": 3
              }
            }
          }
        );
        req.to_string()
    }
}
