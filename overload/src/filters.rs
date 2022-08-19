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

#[cfg(feature = "cluster")]
pub fn overload_req(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(warp::path("test").and(warp::path::end()))
        .and(warp::body::content_length_limit(1024 * 1024))
        .and(warp::body::json())
        .and_then(move |request: Request| {
            let tmp = cluster.clone();
            async move { execute_cluster(request, tmp).await }
        })
}
#[cfg(not(feature = "cluster"))]
pub fn overload_req() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(warp::path("test").and(warp::path::end()))
        .and(warp::body::content_length_limit(1024 * 1024))
        .and(warp::body::json())
        .and_then(|request: Request| async move { execute(request).await })
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

#[cfg(feature = "cluster")]
pub fn stop_req(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("test" / "stop" / String).and_then(move |job_id: String| {
        let tmp = cluster.clone();
        async move { stop(job_id, tmp).await }
    })
}

#[cfg(not(feature = "cluster"))]
pub fn stop_req() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("test" / "stop" / String)
        .and_then(|job_id: String| async move { stop(job_id).await })
}

#[cfg(feature = "cluster")]
pub fn history(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("test" / "status")
        // should replace with untagged enum JobStatusQueryParams, but doesn't work due to
        // https://github.com/nox/serde_urlencoded/issues/66
        .and(warp::query::<HashMap<String, String>>())
        .and_then(move |pager_option: HashMap<String, String>| {
            let tmp = cluster.clone();
            async move {
                let option = JobStatusQueryParams::try_from(pager_option);
                let result: Result<reply::WithStatus<reply::Json>, Infallible> = match option {
                    Ok(option) => {
                        trace!("req: all_job: {:?}", &option);
                        let status = { handle_history_all(option, tmp) }.await;
                        trace!("resp: all_job: {:?}", &status);
                        Ok(generic_result_to_reply_with_status(status))
                    }
                    Err(e) => Ok(generic_error_to_reply_with_status(e)),
                };
                result
            }
        })
}

#[cfg(not(feature = "cluster"))]
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
                    Ok(generic_result_to_reply_with_status(status))
                }
                Err(e) => Ok(generic_error_to_reply_with_status(e)),
            };
            result
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
pub fn info(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "info").and_then(move || {
        let tmp = cluster.clone();
        // let mode=cluster_mode;
        async move { cluster_info(tmp, true).await }
    })
}

#[cfg(feature = "cluster")]
pub fn request_vote(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    // let tmp = cluster.clone();
    warp::path!("cluster" / "raft" / "request-vote" / String / usize).and_then(
        move |node_id, term| {
            let tmp = cluster.clone();
            // let mode = cluster_up;
            async move { cluster_request_vote(tmp, true, node_id, term).await }
        },
    )
}

#[cfg(feature = "cluster")]
pub fn request_vote_response(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "raft" / "vote" / usize / bool).and_then(move |term, vote| {
        let tmp = cluster.clone();
        async move { cluster_request_vote_response(tmp, true, term, vote).await }
    })
}

#[cfg(feature = "cluster")]
pub fn heartbeat(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "raft" / "beat" / String / usize).and_then(move |leader_node, term| {
        let tmp = cluster.clone();
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
async fn execute_cluster(
    request: Request,
    cluster: Arc<Cluster>,
) -> Result<impl Reply, Infallible> {
    trace!(
        "req: execute_cluster: {}",
        serde_json::to_string(&request).unwrap()
    );
    let response = overload::http_util::handle_request_cluster(request, cluster.clone()).await;
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

#[cfg(not(feature = "cluster"))]
async fn stop(job_id: String) -> Result<impl Reply, Infallible> {
    let resp = overload::http_util::stop(job_id).await;
    trace!("resp: stop: {:?}", &resp);
    Ok(generic_result_to_reply_with_status(resp))
}

#[cfg(feature = "cluster")]
async fn stop(job_id: String, cluster: Arc<Cluster>) -> Result<impl Reply, Infallible> {
    let resp = overload::http_util::stop(job_id, cluster.clone()).await;
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
mod test_common {

    use httpmock::prelude::*;
    use hyper::{Body, Client, Request};
    use log::info;
    use prometheus::Encoder;
    use regex::Regex;
    use serde_json::json;
    use std::env;
    use std::sync::Once;
    use tokio::sync::OnceCell;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

            tracing_subscriber::fmt()
                .with_env_filter(format!(
                    "overload={},rust_cloud_discovery={},cloud_discovery_kubernetes={},cluster_mode={},\
                    almost_raft={}, hyper={}, httpmock={}",
                    "trace", "info", "info", "info", "info", "info","trace"
                ))
                .try_init()
                .unwrap();
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
}

#[cfg(all(test, feature = "cluster"))]
mod cluster_test {
    use async_trait::async_trait;
    use cluster_mode::Cluster;
    use http::Request;
    use httpmock::Method::POST;
    use hyper::Body;
    use log::info;
    use overload::JobStatus;
    use regex::Regex;
    use rust_cloud_discovery::DiscoveryClient;
    use rust_cloud_discovery::DiscoveryService;
    use rust_cloud_discovery::ServiceInstance;
    use std::error::Error;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use uuid::Uuid;
    use warp::Filter;

    pub struct TestDiscoverService {
        instances: Vec<ServiceInstance>,
    }

    #[async_trait]
    impl DiscoveryService for TestDiscoverService {
        /// Return list of Kubernetes endpoints as `ServiceInstance`s
        async fn discover_instances(&self) -> Result<Vec<ServiceInstance>, Box<dyn Error>> {
            Ok(self.instances.clone())
        }
    }

    async fn start_overload_at_port(
        port: u16,
        discovery_client: DiscoveryClient<TestDiscoverService>,
    ) -> tokio::sync::oneshot::Sender<()> {
        info!("Running in cluster mode");

        let cluster = Arc::new(Cluster::new(10 * 1000));

        tokio::spawn(cluster_mode::start_cluster(
            cluster.clone(),
            discovery_client,
        ));

        info!("spawning executor init");
        tokio::spawn(overload::executor::init());

        let upload_binary_file = super::upload_binary_file();

        let stop_req = super::stop_req(cluster.clone());

        let history = super::history(cluster.clone());

        // #[cfg(feature = "cluster")]
        let overload_req_secondary = super::overload_req_secondary();

        // cluster-mode configurations
        // #[cfg(feature = "cluster")]
        let info = super::info(cluster.clone());

        // #[cfg(feature = "cluster")]
        let request_vote = super::request_vote(cluster.clone());

        // #[cfg(feature = "cluster")]
        let request_vote_response = super::request_vote_response(cluster.clone());

        // #[cfg(feature = "cluster")]
        let heartbeat = super::heartbeat(cluster.clone());

        let prometheus_metric = super::prometheus_metric();
        let overload_req = super::overload_req(cluster);
        let routes = prometheus_metric
            .or(overload_req)
            .or(stop_req)
            .or(history)
            .or(upload_binary_file);
        // #[cfg(feature = "cluster")]
        let routes = routes
            .or(info)
            .or(request_vote)
            .or(request_vote_response)
            .or(heartbeat)
            .or(overload_req_secondary);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let (_addr, server) =
            warp::serve(routes).bind_with_graceful_shutdown(([127, 0, 0, 1], port), async {
                rx.await.ok();
            });
        // Spawn the server into a runtime
        tokio::task::spawn(server);
        tx
    }

    fn get_discovery_service() -> TestDiscoverService {
        let mut instances = vec![];
        let instance = ServiceInstance::new(
            Some(Uuid::new_v4().to_string()),
            Some(String::from_str("test").unwrap()),
            Some(String::from_str("127.0.0.1").unwrap()),
            Some(3030),
            false,
            Some("http://127.0.0.1:3030".to_string()),
            std::collections::HashMap::new(),
            Some(String::from_str("HTTP").unwrap()),
        );

        instances.push(instance);
        let instance = ServiceInstance::new(
            Some(Uuid::new_v4().to_string()),
            Some(String::from_str("test").unwrap()),
            Some(String::from_str("127.0.0.1").unwrap()),
            Some(3031),
            false,
            Some("http://127.0.0.1:3031".to_string()),
            std::collections::HashMap::new(),
            Some(String::from_str("HTTP").unwrap()),
        );
        instances.push(instance);
        let instance = ServiceInstance::new(
            Some(Uuid::new_v4().to_string()),
            Some(String::from_str("test").unwrap()),
            Some(String::from_str("127.0.0.1").unwrap()),
            Some(3032),
            false,
            Some("http://127.0.0.1:3032".to_string()),
            std::collections::HashMap::new(),
            Some(String::from_str("HTTP").unwrap()),
        );
        instances.push(instance);

        let instance = ServiceInstance::new(
            Some(Uuid::new_v4().to_string()),
            Some(String::from_str("test").unwrap()),
            Some(String::from_str("127.0.0.1").unwrap()),
            Some(3033),
            false,
            Some("http://127.0.0.1:3033".to_string()),
            std::collections::HashMap::new(),
            Some(String::from_str("HTTP").unwrap()),
        );
        instances.push(instance);
        TestDiscoverService { instances }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_requests() {
        info!("cluster: test_request_random_constant");
        super::test_common::setup();

        let (mock_server, url) = super::test_common::ASYNC_ONCE_HTTP_MOCK
            .get_or_init(super::test_common::init_http_mock)
            .await;

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path_matches(Regex::new(r"^/anything/[a-zA-Z]{6,15}/1(1|0)\d{5}$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .header("Connection", "keep-alive")
                .body(r#"{"hello": "world"}"#);
        });

        let ds = get_discovery_service();
        let dc = DiscoveryClient::new(ds);
        let _tx1 = start_overload_at_port(3030, dc).await;

        let ds = get_discovery_service();
        let dc = DiscoveryClient::new(ds);
        let _tx2 = start_overload_at_port(3031, dc).await;

        let ds = get_discovery_service();
        let dc = DiscoveryClient::new(ds);
        let _tx3 = start_overload_at_port(3032, dc).await;

        let ds = get_discovery_service();
        let dc = DiscoveryClient::new(ds);
        let _tx4 = start_overload_at_port(3033, dc).await;

        tokio::time::sleep(Duration::from_millis(30000)).await;

        for _ in 0..10 {
            let response = crate::filters::test_common::send_request(
                crate::filters::test_common::json_request_random_constant(
                    url.host().unwrap().to_string(),
                    url.port().unwrap(),
                ),
            )
            .await
            .unwrap();
            println!("{:?}", response);
            let status = response.status();
            let response = hyper::body::to_bytes(response).await.unwrap();
            info!("body: {:?}", response);
            let response = serde_json::from_slice::<overload::Response>(&response).unwrap();
            info!("body: {:?}", &response);
            match response.get_status() {
                JobStatus::Error(_) => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(5000)).await;
                }
                _ => {
                    assert_eq!(status, 200);
                    break;
                }
            }
        }

        //primary node bundles 10 requests together and then sends to secondary, so wait for min(10,duration) seconds
        tokio::time::sleep(tokio::time::Duration::from_millis(5000)).await;
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        for i in 1..6 {
            //each seconds we expect mock to receive 3 request
            assert_eq!(i * 3, mock.hits_async().await);
            info!("mock hit: {}", mock.hits_async().await);
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
        mock.delete_async().await;

        //test csv file
        let mut csv_requests = vec![];
        let mut mocks = vec![];
        csv_requests.push(r#""url","method","body","headers""#.to_string());
        for i in 1..=15 {
            // csv_requests.push(format!(r#"http://127.0.0.1:{}/anything","POST","{{\"some\":\"random data\",\"second-key\":\"more data\"}}","{{\"Authorization\":\"Bearer 123\"}}"#, url.port().unwrap()));
            csv_requests.push(format!(r#""http://127.0.0.1:{}/{}","POST","{{\"some\":\"random data\",\"second-key\":\"more data\"}}","{{}}""#, url.port().unwrap(), i));
            let mock = mock_server.mock(|when, then| {
                when.method(POST).path(format!("/{}", i));
                then.status(200)
                    .header("content-type", "application/json")
                    .header("Connection", "keep-alive")
                    .body(r#"{"hello": "1"}"#);
            });
            mocks.push(mock);
        }

        let csv_requests = csv_requests.iter().fold("".to_string(), |prev, current| {
            info!("csv request: {}", &current);
            format!("{}\n{}", prev, current)
        });

        info!("uploading file...");
        let contents = csv_requests.into_bytes();
        // sqlite.read_to_end(&mut contents).await.unwrap();
        let client = hyper::Client::new();
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1:3030/test/requests-bin")
            .body(hyper::Body::from(contents))
            .expect("request builder failed");
        let response = client.request(req).await.unwrap();
        let response = hyper::body::to_bytes(response).await.unwrap();
        println!("body: {:?}", &response);
        let response = serde_json::from_slice::<serde_json::Value>(&response).unwrap();
        println!("body: {:?}", &response);
        assert_eq!(
            response
                .get("valid_count")
                .unwrap()
                .as_str()
                .unwrap()
                .parse::<u16>()
                .unwrap(),
            15
        );
        let json = json_request_with_file(
            url.host().unwrap().to_string(),
            url.port().unwrap(),
            response.get("file").unwrap().as_str().unwrap().to_string(),
        );
        let response = crate::filters::test_common::send_request(json)
            .await
            .unwrap();
        let status = response.status();
        let response = hyper::body::to_bytes(response).await.unwrap();
        info!("body: {:?}", &response);
        let response = serde_json::from_slice::<overload::Response>(&response).unwrap();
        info!("body: {:?}", &response);
        assert_eq!(status, 200);

        tokio::time::sleep(tokio::time::Duration::from_millis(15000)).await;
        let mut i = 0;
        for mock in mocks {
            //each seconds we expect mock to receive 3 request
            // assert_eq!(1, mock.hits_async().await);
            let hits = mock.hits_async().await;
            info!("mock hit: {}", hits);
            i += hits;
            mock.delete_async().await;
        }
        assert_eq!(15, i);

        let _ = std::fs::remove_file("csv_requests_for_test.sqlite");
    }

    fn json_request_with_file(host: String, port: u16, filename: String) -> String {
        let req = serde_json::json!({
            "duration": 5,
            "req": {
                "RequestFile": {
                   "file_name": filename
                }
            },
            "qps": {
                "ConstantRate": {
                   "countPerSec": 3
                }
            },
            "target": {
                "host":host,
                "port": port,
                "protocol": "HTTP"
            }
        });
        serde_json::to_string(&req).unwrap()
    }
}

#[cfg(all(test, not(feature = "cluster")))]
mod standalone_mode_tests {
    use httpmock::prelude::*;
    use hyper::{Body, Client, Request};
    use log::info;
    use prometheus::Encoder;
    use regex::Regex;
    use serde_json::json;
    use std::sync::Once;
    use tokio::sync::OnceCell;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::METRICS_FACTORY;

    pub async fn init_env() -> (MockServer, url::Url, tokio::sync::oneshot::Sender<()>) {
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

    pub static ASYNC_ONCE: OnceCell<(MockServer, url::Url, tokio::sync::oneshot::Sender<()>)> =
        OnceCell::const_new();

    #[tokio::test(flavor = "multi_thread")]
    async fn test_request_random_constant() {
        super::test_common::setup();
        let (_, _, _) = ASYNC_ONCE.get_or_init(init_env).await;

        let (mock_server, url) = super::test_common::ASYNC_ONCE_HTTP_MOCK
            .get_or_init(super::test_common::init_http_mock)
            .await;

        let mock = mock_server.mock(|when, then| {
            when.method(GET)
                .path_matches(Regex::new(r"^/anything/[a-zA-Z]{6,15}/1(1|0)\d{5}$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .header("Connection", "keep-alive")
                .body(r#"{"hello": "world"}"#);
        });

        let response = crate::filters::test_common::send_request(
            crate::filters::test_common::json_request_random_constant(
                url.host().unwrap().to_string(),
                url.port().unwrap(),
            ),
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
        //Inconsistent result - it fails sometimes, hence commenting out
        //let metrics = get_metrics();
        // assert_eq!(get_value_for_metrics("connection_pool_idle_connections", &metrics), 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_request_list_constant() {
        super::test_common::setup();
        let (_, _, _) = ASYNC_ONCE.get_or_init(init_env).await;

        let (mock_server, url) = super::test_common::ASYNC_ONCE_HTTP_MOCK
            .get_or_init(super::test_common::init_http_mock)
            .await;

        let mock = mock_server.mock(|when, then| {
            when.method(GET)
                .path_matches(Regex::new(r"^/list/data$").unwrap());
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"hello": "list"}"#);
        });

        let response = crate::filters::test_common::send_request(json_request_list_constant(
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
                "countPerSec": 3
              }
            },
            "concurrentConnection": {
                "ConstantRate": {
                    "countPerSec": 6
                  }
            }
          }
        );
        req.to_string()
    }

    #[allow(dead_code)]
    fn get_metrics() -> String {
        let encoder = prometheus::TextEncoder::new();
        let metrics = METRICS_FACTORY.registry().gather();
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
