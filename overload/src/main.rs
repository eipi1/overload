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
lazy_static! {
    static ref CLUSTER: Arc<Cluster> = Arc::new(Cluster::new(10 * 1000));
}

#[tokio::main]
async fn main() {
    //init logging
    let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(format!(
            "overload={},rust_cloud_discovery={},cloud_discovery_kubernetes={},cluster_mode={},\
            almost_raft={}",
            &log_level, &log_level, &log_level, &log_level, &log_level
        ))
        .try_init()
        .unwrap();
    info!("log level: {}", &log_level);
    info!("data directory: {}", data_dir());

    let mut _cluster_up = true;
    #[cfg(feature = "cluster")]
    {
        info!("Running in cluster mode");

        //todo get from k8s itself
        #[cfg(feature = "cluster")]
        let service = env::var("K8S_ENDPOINT_NAME")
            .ok()
            .unwrap_or_else(|| "overload".to_string());
        #[cfg(feature = "cluster")]
        let namespace = env::var("K8S_NAMESPACE_NAME")
            .ok()
            .unwrap_or_else(|| "default".to_string());

        info!("k8s endpoint name: {}", &service);
        info!("k8s namespace: {}", namespace);

        // initialize cluster
        // init discovery service
        let k8s = KubernetesDiscoverService::init(service, namespace).await;

        match k8s {
            Ok(k8s) => {
                let discovery_client = DiscoveryClient::new(k8s);
                tokio::spawn(cluster_mode::start_cluster(
                    CLUSTER.clone(),
                    discovery_client,
                ));
            }
            Err(e) => {
                _cluster_up = false;
                info!("error initializing kubernetes: {}", e.to_string());
            }
        }
    }

    info!("spawning executor init");
    tokio::spawn(overload::executor::init());

    let prometheus_metric = warp::get().and(warp::path("metrics")).map(|| {
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
    });
    let overload_req = warp::post()
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
        });

    let upload_binary_file = warp::post()
        .and(
            warp::path("test")
                .and(warp::path("requests-bin"))
                .and(warp::path::end()),
        )
        .and(warp::body::content_length_limit(1024 * 1024 * 32))
        .and(warp::body::stream())
        .and_then(upload_binary_file);

    let stop_req = warp::path!("test" / "stop" / String)
        .and_then(|job_id: String| async move { stop(job_id).await });

    let history = warp::path!("test" / "status")
        // should replace with untagged enum JobStatusQueryParams, but doesn't work due to
        // https://github.com/nox/serde_urlencoded/issues/66
        .and(warp::query::<HashMap<String, String>>())
        .and_then(|pager_option: HashMap<String, String>| async move {
            all_job(pager_option).await
            // ()
        });

    #[cfg(feature = "cluster")]
    let overload_req_secondary = warp::post()
        .and(
            warp::path("cluster")
                .and(warp::path("test"))
                .and(warp::path::end()),
        )
        .and(warp::body::content_length_limit(1024 * 1024))
        .and(warp::body::json())
        .and_then(|request: Request| async move { execute(request).await });

    // cluster-mode configurations
    #[cfg(feature = "cluster")]
    let info = warp::path!("cluster" / "info").and_then(|| {
        let tmp = CLUSTER.clone();
        // let mode=cluster_mode;
        async move { cluster_info(tmp, true).await }
    });
    #[cfg(feature = "cluster")]
    let request_vote = warp::path!("cluster" / "raft" / "request-vote" / String / usize).and_then(
        |node_id, term| {
            let tmp = CLUSTER.clone();
            // let mode = cluster_up;
            async move { cluster_request_vote(tmp, true, node_id, term).await }
        },
    );

    #[cfg(feature = "cluster")]
    let request_vote_response =
        warp::path!("cluster" / "raft" / "vote" / usize / bool).and_then(|term, vote| {
            let tmp = CLUSTER.clone();
            async move { cluster_request_vote_response(tmp, true, term, vote).await }
        });

    #[cfg(feature = "cluster")]
    let heartbeat =
        warp::path!("cluster" / "raft" / "beat" / String / usize).and_then(|leader_node, term| {
            let tmp = CLUSTER.clone();
            async move { cluster_heartbeat(tmp, true, leader_node, term).await }
        });

    let routes = prometheus_metric
        .or(overload_req)
        .or(stop_req)
        .or(history)
        .or(upload_binary_file);
    #[cfg(feature = "cluster")]
    let routes = routes
        .or(info)
        .or(request_vote)
        .or(request_vote_response)
        .or(heartbeat)
        .or(overload_req_secondary);
    warp::serve(routes).run(([0, 0, 0, 0], 3030)).await;
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

/// should use shared storage, no need to forward upload request
async fn upload_binary_file<S, B>(data: S) -> Result<impl Reply, Infallible>
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

// #[cfg(not(feature = "cluster"))]
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
