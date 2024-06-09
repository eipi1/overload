#![allow(opaque_hidden_inferred_bound)]
#![allow(deprecated)]
use crate::filters_common;
use crate::filters_common::prometheus_metric;
use bytes::{Buf, Bytes, BytesMut};
use cluster_executor::data_dir_path;
use cluster_mode::{get_cluster_info, Cluster};
use futures_util::future::Either;
use futures_util::{FutureExt, Stream, StreamExt};
use http::StatusCode;
use hyper::Body;
use lazy_static::lazy_static;
use log::trace;
use overload::cluster::handle_history_all;
use overload::data_dir;
use overload_http::{GenericError, JobStatusQueryParams, MultiRequest, Request};
use serde_json::Value;
use std::collections::HashMap;
use std::convert::{Infallible, TryFrom};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use tokio::fs::File;
use tokio::io::AsyncSeekExt;
use warp::reply::{Json, Response, WithStatus};
use warp::{reply, Filter};

lazy_static! {
    pub static ref CLUSTER: Arc<Cluster> = Arc::new(Cluster::new());
}

pub fn get_routes() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    get_routes_with_cluster(CLUSTER.clone())
}
pub fn get_routes_with_cluster(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let upload_binary_file = upload_binary_file(cluster.clone());
    let upload_csv_file = upload_csv_file(cluster.clone());
    let upload_sqlite_file = upload_sqlite_file(cluster.clone());
    let stop_req = stop_req(cluster.clone());
    let history = history(cluster.clone());
    // let overload_req_secondary = overload_req_secondary(cluster.clone());
    let overload_req = overload_req(cluster.clone());
    let overload_multi_req = overload_multi_req(cluster.clone());

    let info = info(cluster.clone());
    let request_vote = request_vote(cluster.clone());
    let request_vote_response = request_vote_response(cluster.clone());
    let heartbeat = heartbeat(cluster);
    let download_data_file = download_req_from_secondaries();

    let prometheus_metric = prometheus_metric();

    let routes = prometheus_metric
        .or(overload_req)
        .or(overload_multi_req)
        .or(stop_req)
        .or(history)
        .or(upload_binary_file)
        .or(upload_csv_file)
        .or(upload_sqlite_file);

    routes
        .or(info)
        .or(request_vote)
        .or(request_vote_response)
        .or(heartbeat)
        // .or(overload_req_secondary)
        .or(download_data_file)
}

pub fn overload_req(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(warp::path("test").and(warp::path::end()))
        .and(warp::body::content_length_limit(1024 * 1024 * 2))
        .and(warp::body::json())
        .and_then(move |request: Request| {
            let tmp = cluster.clone();
            async move { execute_cluster(request, tmp).await }
        })
}

pub fn overload_multi_req(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(warp::path("tests").and(warp::path::end()))
        .and(warp::body::content_length_limit(1024 * 1024 * 5))
        .and(warp::body::json())
        .and_then(move |request: MultiRequest| {
            let tmp = cluster.clone();
            async move { execute_multi_req_cluster(request, tmp).await }
        })
}

pub fn upload_binary_file(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(
            warp::path("test")
                .and(warp::path("requests-bin"))
                .and(warp::path::end()),
        )
        .and(warp::body::content_length_limit(1024 * 1024 * 1024))
        .and(warp::header::<u64>("content-length"))
        .and(warp::body::stream())
        .and_then(move |content_len, stream| {
            let tmp = cluster.clone();
            async move { upload_csv_file_handler(stream, tmp, content_len).await }
        })
}

pub fn upload_csv_file(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(
            warp::path("request-file")
                .and(warp::path("csv"))
                .and(warp::path::end()),
        )
        .and(warp::body::content_length_limit(1024 * 1024 * 1024))
        .and(warp::header::<u64>("content-length"))
        .and(warp::body::stream())
        .and_then(move |content_len, stream| {
            let tmp = cluster.clone();
            async move { upload_csv_file_handler(stream, tmp, content_len).await }
        })
}

pub fn upload_sqlite_file(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(
            warp::path("request-file")
                .and(warp::path("sqlite"))
                .and(warp::path::end()),
        )
        .and(warp::body::content_length_limit(1024 * 1024 * 1024))
        .and(warp::header::<u64>("content-length"))
        .and(warp::body::stream())
        .and_then(move |content_len, stream| {
            let tmp = cluster.clone();
            async move { upload_sqlite_file_handler(stream, tmp, content_len).await }
        })
}

pub fn stop_req(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("test" / "stop" / String).and_then(move |job_id: String| {
        let tmp = cluster.clone();
        async move { stop(job_id, tmp).await }
    })
}

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
                        Ok(filters_common::generic_result_to_reply_with_status(status))
                    }
                    Err(e) => Ok(filters_common::generic_error_to_reply_with_status(e)),
                };
                result
            }
        })
}

pub fn info(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "info").and_then(move || {
        let tmp = cluster.clone();
        // let mode=cluster_mode;
        async move { cluster_info(tmp, true).await }
    })
}

pub fn request_vote(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "raft" / "request-vote" / String / usize).and_then(
        move |node_id, term| {
            let tmp = cluster.clone();
            // let mode = cluster_up;
            async move { cluster_request_vote(tmp, true, node_id, term).await }
        },
    )
}

pub fn request_vote_response(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "raft" / "vote" / usize / bool).and_then(move |term, vote| {
        let tmp = cluster.clone();
        async move { cluster_request_vote_response(tmp, true, term, vote).await }
    })
}

pub fn heartbeat(
    cluster: Arc<Cluster>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "raft" / "beat" / String / usize).and_then(move |leader_node, term| {
        let tmp = cluster.clone();
        async move { cluster_heartbeat(tmp, true, leader_node, term).await }
    })
}

async fn execute_cluster(
    request: Request,
    cluster: Arc<Cluster>,
) -> Result<impl warp::Reply, Infallible> {
    trace!(
        "req: execute_cluster: {}",
        serde_json::to_string(&request).unwrap()
    );
    let response = overload::cluster::handle_request_cluster(request, cluster.clone()).await;
    let json = reply::json(&response);
    trace!("resp: execute_cluster: {:?}", &response);
    Ok(json)
}

async fn execute_multi_req_cluster(
    request: MultiRequest,
    cluster: Arc<Cluster>,
) -> Result<impl warp::Reply, Infallible> {
    trace!(
        "req: execute_multi_req_cluster: {}",
        serde_json::to_string(&request).unwrap()
    );
    let response = overload::cluster::handle_multi_request_cluster(request, cluster.clone()).await;
    let json = reply::json(&response);
    trace!("resp: execute_multi_req_cluster: {:?}", &response);
    Ok(json)
}

/// should use shared storage, no need to forward upload request
async fn upload_csv_file_handler<S, B>(
    data: S,
    cluster: Arc<Cluster>,
    content_len: u64,
) -> Result<impl warp::Reply, Infallible>
where
    S: Stream<Item = Result<B, warp::Error>> + Unpin + Send + Sync + 'static,
    B: Buf + Send + Sync,
{
    let data_dir = data_dir();
    trace!("req: upload_binary_file_handler");
    let result = overload::cluster::handle_file_upload(data, &data_dir, cluster, content_len).await;
    match result {
        Ok(r) => Ok(reply::with_status(reply::json(&r), StatusCode::OK)),
        Err(e) => Ok(reply::with_status(
            reply::json(&e),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// should use shared storage, no need to forward upload request
async fn upload_sqlite_file_handler<S, B>(
    data: S,
    cluster: Arc<Cluster>,
    content_len: u64,
) -> Result<impl warp::Reply, Infallible>
where
    S: Stream<Item = Result<B, warp::Error>> + Unpin + Send + Sync + 'static,
    B: Buf + Send + Sync,
{
    let path = data_dir_path();
    let data_dir = path.to_str().unwrap();
    let result = overload::cluster::save_sqlite(data, data_dir, cluster, content_len).await;
    match result {
        Ok(r) => Ok(reply::with_status(reply::json(&r), StatusCode::OK)),
        Err(e) => Ok(reply::with_status(
            reply::json(&e),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

async fn stop(job_id: String, cluster: Arc<Cluster>) -> Result<impl warp::Reply, Infallible> {
    let resp = overload::cluster::stop(job_id, cluster.clone()).await;
    trace!("resp: stop: {:?}", &resp);
    Ok(filters_common::generic_result_to_reply_with_status(resp))
}

async fn cluster_info(
    cluster: Arc<Cluster>,
    cluster_mode: bool,
) -> Result<impl warp::Reply, Infallible> {
    if !cluster_mode {
        let err = no_cluster_err();
        Ok(err)
    } else {
        let info = get_cluster_info(cluster).await;
        let json = reply::with_status(reply::json(&info), StatusCode::OK);
        Ok(json)
    }
}

async fn cluster_request_vote(
    cluster: Arc<Cluster>,
    cluster_mode: bool,
    requester_node_id: String,
    term: usize,
) -> Result<impl warp::Reply, Infallible> {
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

async fn cluster_request_vote_response(
    cluster: Arc<Cluster>,
    cluster_mode: bool,
    term: usize,
    vote: bool,
) -> Result<impl warp::Reply, Infallible> {
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

async fn cluster_heartbeat(
    cluster: Arc<Cluster>,
    cluster_mode: bool,
    leader_node_id: String,
    term: usize,
) -> Result<impl warp::Reply, Infallible> {
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

fn no_cluster_err() -> WithStatus<Json> {
    let error_msg: Value = serde_json::from_str("{\"error\":\"Cluster is not running\"}").unwrap();
    reply::with_status(reply::json(&error_msg), StatusCode::NOT_FOUND)
}

pub fn download_req_from_secondaries(
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("cluster" / "data-file" / String)
        .and_then(|filename| async move { response_file(filename).await })
}

async fn response_file(filename: String) -> Result<impl warp::Reply, Infallible> {
    let file = format!("{}/{}", data_dir(), filename);
    trace!("received download request for file: {}", &filename);
    let body = body_from_file(&file).await;
    Ok(match body {
        Ok(body) => {
            let response = Response::new(body);
            warp::reply::with_status(response, StatusCode::OK)
        }
        Err(e) => warp::reply::with_status(
            Response::new(Body::from(serde_json::to_string(&e).unwrap())),
            StatusCode::from_u16(e.error_code).unwrap(),
        ),
    })
}

async fn body_from_file(file: &str) -> Result<Body, GenericError> {
    let file = tokio::fs::File::open(&file).await?;
    let metadata = file.metadata().await?;
    let stream = file_stream(file, 8_192, (0, metadata.len()));
    Ok(Body::wrap_stream(stream))
}

fn file_stream(
    mut file: File,
    buf_size: usize,
    (start, end): (u64, u64),
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    use std::io::SeekFrom;

    let seek = async move {
        if start != 0 {
            file.seek(SeekFrom::Start(start)).await?;
        }
        Ok(file)
    };

    seek.into_stream()
        .map(move |result| {
            let mut buf = BytesMut::new();
            let mut len = end - start;
            let mut f = match result {
                Ok(f) => f,
                Err(f) => {
                    return Either::Left(futures_util::stream::once(futures_util::future::err(f)))
                }
            };

            Either::Right(futures_util::stream::poll_fn(move |cx| {
                if len == 0 {
                    return Poll::Ready(None);
                }
                reserve_at_least(&mut buf, buf_size);

                let n = match futures_core::ready!(tokio_util::io::poll_read_buf(
                    Pin::new(&mut f),
                    cx,
                    &mut buf
                )) {
                    Ok(n) => n as u64,
                    Err(err) => {
                        tracing::debug!("file read error: {}", err);
                        return Poll::Ready(Some(Err(err)));
                    }
                };

                if n == 0 {
                    tracing::debug!("file read found EOF before expected length");
                    return Poll::Ready(None);
                }

                let mut chunk = buf.split().freeze();
                if n > len {
                    chunk = chunk.split_to(len as usize);
                    len = 0;
                } else {
                    len -= n;
                }

                Poll::Ready(Some(Ok(chunk)))
            }))
        })
        .flatten()
}

fn reserve_at_least(buf: &mut BytesMut, cap: usize) {
    if buf.capacity() - buf.len() < cap {
        buf.reserve(cap);
    }
}

#[cfg(all(test, feature = "cluster"))]
mod cluster_test {
    use crate::filters::{download_req_from_secondaries, get_routes_with_cluster};
    use crate::filters_common::test_common::*;
    use async_trait::async_trait;
    use bytes::Buf;
    use cluster_mode::{Cluster, ClusterConfig};
    use csv_async::AsyncReaderBuilder;
    use http::Request;
    use httpmock::Method::POST;
    use hyper::body::to_bytes;
    use hyper::Body;
    use log::info;
    use overload::file_uploader::csv_reader_to_sqlite;
    use overload::{init, JobStatus};
    use regex::Regex;
    use rust_cloud_discovery::DiscoveryService;
    use rust_cloud_discovery::ServiceInstance;
    use rust_cloud_discovery::{DiscoveryClient, Port};
    use sha2::Digest;
    use std::error::Error;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::oneshot::Sender;
    use tokio::task::JoinHandle;
    use tracing::trace;
    use uuid::Uuid;
    use warp::Filter;

    fn start_warp_with_route(
        route: impl Filter<Extract = impl warp::Reply, Error = warp::Rejection>
            + Send
            + Sync
            + Clone
            + 'static,
    ) -> (Sender<()>, JoinHandle<()>) {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let (_addr, server) =
            warp::serve(route).bind_with_graceful_shutdown(([127, 0, 0, 1], 3030), async {
                rx.await.ok();
            });
        // Spawn the server into a runtime
        let handle = tokio::task::spawn(server);
        (tx, handle)
    }

    // #[serial_test::serial]
    #[tokio::test]
    #[ignore]
    async fn test_download_req_from_secondaries() {
        setup();
        let (tx, join_handle) = start_warp_with_route(download_req_from_secondaries());
        let filename = "005bbaa3-61cf-4ed3-a0f9-771d05a0c3cd";
        let file_path = format!("/tmp/{}", filename);
        generate_sqlite_file(&file_path).await;
        let client = hyper::Client::new();
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "http://127.0.0.1:3030{}/{}",
                overload_http::PATH_REQUEST_DATA_FILE_DOWNLOAD,
                filename
            ))
            .body(Body::empty())
            .expect("request builder");
        let response = client.request(req).await.unwrap();
        trace!("{:?}", &response);
        assert_eq!(response.status(), 200);
        let bytes = to_bytes(response).await.unwrap();
        let mut reader = bytes.reader();

        let _ = tx.send(());

        let mut file = std::fs::File::open(&file_path).unwrap();
        let mut sha256 = sha2::Sha256::new();
        std::io::copy(&mut file, &mut sha256).unwrap();
        let hash_1 = sha256.finalize();

        let mut sha256 = sha2::Sha256::new();
        std::io::copy(&mut reader, &mut sha256).unwrap();
        let hash_2 = sha256.finalize();
        assert_eq!(hash_1, hash_2);
        let _ = tokio::fs::remove_file(&file_path).await;

        //ensure that server is terminated. Otherwise other tests may get port already in use error
        join_handle.abort();
    }

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

        let cluster = Arc::new(Cluster::new());

        tokio::spawn(cluster_mode::start_cluster(
            cluster.clone(),
            discovery_client,
            ClusterConfig::default(),
        ));

        info!("spawning executor init");
        tokio::spawn(init());

        let routes = get_routes_with_cluster(cluster);

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
        let port1 = Port::new(
            Some("http-endpoint".to_string()),
            3030_u32,
            "tcp".to_string(),
            Some("http".to_string()),
        );
        let port2 = Port::new(
            Some("tcp-remoc".to_string()),
            4030_u32,
            "tcp".to_string(),
            Some("tcp".to_string()),
        );
        let ports = vec![port1, port2];
        let instance = ServiceInstance::new(
            Some(Uuid::new_v4().to_string()),
            Some(String::from_str("test").unwrap()),
            Some(String::from_str("127.0.0.1").unwrap()),
            Some(ports),
            false,
            Some("http://127.0.0.1:3030".to_string()),
            std::collections::HashMap::new(),
            Some(String::from_str("HTTP").unwrap()),
        );

        instances.push(instance);
        let port1 = Port::new(
            Some("http-endpoint".to_string()),
            3031_u32,
            "tcp".to_string(),
            Some("http".to_string()),
        );
        let port2 = Port::new(
            Some("tcp-remoc".to_string()),
            4030_u32,
            "tcp".to_string(),
            Some("tcp".to_string()),
        );
        let ports = vec![port1, port2];
        let instance = ServiceInstance::new(
            Some(Uuid::new_v4().to_string()),
            Some(String::from_str("test").unwrap()),
            Some(String::from_str("127.0.0.1").unwrap()),
            Some(ports),
            false,
            Some("http://127.0.0.1:3031".to_string()),
            std::collections::HashMap::new(),
            Some(String::from_str("HTTP").unwrap()),
        );
        instances.push(instance);

        let port1 = Port::new(
            Some("http-endpoint".to_string()),
            3032_u32,
            "tcp".to_string(),
            Some("http".to_string()),
        );
        let port2 = Port::new(
            Some("tcp-remoc".to_string()),
            4030_u32,
            "tcp".to_string(),
            Some("tcp".to_string()),
        );
        let ports = vec![port1, port2];
        let instance = ServiceInstance::new(
            Some(Uuid::new_v4().to_string()),
            Some(String::from_str("test").unwrap()),
            Some(String::from_str("127.0.0.1").unwrap()),
            Some(ports),
            false,
            Some("http://127.0.0.1:3032".to_string()),
            std::collections::HashMap::new(),
            Some(String::from_str("HTTP").unwrap()),
        );
        instances.push(instance);

        let port1 = Port::new(
            Some("http-endpoint".to_string()),
            3033_u32,
            "tcp".to_string(),
            Some("http".to_string()),
        );
        let port2 = Port::new(
            Some("tcp-remoc".to_string()),
            4030_u32,
            "tcp".to_string(),
            Some("tcp".to_string()),
        );
        let ports = vec![port1, port2];
        let instance = ServiceInstance::new(
            Some(Uuid::new_v4().to_string()),
            Some(String::from_str("test").unwrap()),
            Some(String::from_str("127.0.0.1").unwrap()),
            Some(ports),
            false,
            Some("http://127.0.0.1:3033".to_string()),
            std::collections::HashMap::new(),
            Some(String::from_str("HTTP").unwrap()),
        );
        instances.push(instance);
        TestDiscoverService { instances }
    }

    // Connection pool is now mutable, restricting usage to only one executor.
    // But as this test uses same process to simulate multiple nodes (using tokio::spawn),
    // it prevents other nodes from accessing pool while a node is already using it.
    // Disabled this test case until I can figure out how to do it.
    #[tokio::test(flavor = "multi_thread")]
    // #[serial_test::serial]
    #[ignore]
    async fn test_requests() {
        info!("cluster: test_request_random_constant");
        setup();

        let (mock_server, url) = ASYNC_ONCE_HTTP_MOCK.get_or_init(init_http_mock).await;

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
            let response = send_request(
                json_request_random_constant(url.host().unwrap().to_string(), url.port().unwrap()),
                3030,
                "test",
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
        let response = send_request(json, 3030, "test").await.unwrap();
        let status = response.status();
        let response = hyper::body::to_bytes(response).await.unwrap();
        info!("body: {:?}", &response);
        let response = serde_json::from_slice::<overload::Response>(&response).unwrap();
        info!("body: {:?}", &response);
        assert_eq!(status, 200);

        tokio::time::sleep(tokio::time::Duration::from_millis(21000)).await;
        let mut i = 0;
        for mock in mocks {
            //each seconds we expect mock to receive 3 request
            // assert_eq!(1, mock.hits_async().await);
            let hits = mock.hits_async().await;
            info!("mock hit: {}", hits);
            i += hits;
            mock.delete_async().await;
        }
        assert_eq!(45, i);

        let _ = std::fs::remove_file("csv_requests_for_test.sqlite");
    }

    fn json_request_with_file(host: String, port: u16, filename: String) -> String {
        let req = serde_json::json!({
            "duration": 15,
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

    pub async fn generate_sqlite_file(file_path: &str) {
        let csv_data = r#""url","method","body","headers"
"http://httpbin.org/anything/11","GET","","{}"
"http://httpbin.org/anything/13","GET","","{}"
"http://httpbin.org/anything","POST","{\"some\":\"random data\",\"second-key\":\"more data\"}","{\"Authorization\":\"Bearer 123\"}"
"http://httpbin.org/bearer","GET","","{\"Authorization\":\"Bearer 123\"}"
"#;
        let reader = AsyncReaderBuilder::new()
            .escape(Some(b'\\'))
            .create_deserializer(csv_data.as_bytes());
        let _ = csv_reader_to_sqlite(reader, file_path.to_string()).await;
    }
}
