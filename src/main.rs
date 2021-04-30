use cfg_if::cfg_if;
#[cfg(feature = "cluster")]
use cloud_discovery_kubernetes::KubernetesDiscoverService;
#[cfg(feature = "cluster")]
use cluster_mode::{get_cluster_info, Cluster};
#[cfg(feature = "cluster")]
use http::StatusCode;
use hyper::header::CONTENT_TYPE;
use hyper::{Body, Response};
use lazy_static::lazy_static;
use log::{info, trace};
use overload::http_util;
use overload::http_util::handle_history_all;
use overload::http_util::request::{PagerOptions, Request};
use prometheus::{opts, register_counter, Counter, Encoder, TextEncoder};
#[cfg(feature = "cluster")]
use rust_cloud_discovery::DiscoveryClient;
#[cfg(feature = "cluster")]
use serde_json::Value;
use std::convert::Infallible;
use std::env;
use std::fmt::Debug;
#[cfg(feature = "cluster")]
use std::sync::Arc;
use warp::multipart::FormData;
use warp::reject::Reject;
#[cfg(feature = "cluster")]
use warp::reply::{Json, WithStatus};
use warp::{Filter, Rejection, Reply};

lazy_static! {
    static ref HTTP_COUNTER: Counter = register_counter!(opts!(
        "overload_requests_total",
        "Total number of HTTP requests made."
    ))
    .unwrap();
}

#[cfg(feature = "cluster")]
lazy_static! {
    static ref CLUSTER: Arc<Cluster> = Arc::new(Cluster::new(10 * 1000));
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    //init logging
    let log_level = env::var_os("log.level")
        .map(|v| v.into_string().ok())
        .flatten()
        .unwrap_or_else(|| "trace".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(format!(
            "overload={},rust_cloud_discovery={},cloud_discovery_kubernetes={},cluster_mode={},almost_raft={}",
            &log_level, &log_level, &log_level, &log_level, &log_level
        ))
        .try_init()
        .unwrap();
    info!("log level: {}", &log_level);

    // for var in env::vars() {
    //     info!("var:{:?}", var);
    // }
    // for var in env::vars_os() {
    //     info!("var_os:{:?}", var);
    // }

    //todo get from k8s itself
    #[cfg(feature = "cluster")]
    let service = env::var("app.k8s.service")
        .ok()
        .unwrap_or_else(|| "overload".to_string());
    #[cfg(feature = "cluster")]
    let namespace = env::var("app.k8s.namespace")
        .ok()
        .unwrap_or_else(|| "default".to_string());

    let mut _cluster_up = true;
    #[cfg(feature = "cluster")]
    {
        info!("Running in cluster mode");
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
        let metrics = prometheus::gather();
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
            HTTP_COUNTER.inc();
            cfg_if! {
                if #[cfg(feature = "cluster")] {
                    execute_cluster(request).await
                } else {
                    execute(request).await
                }
            }
        });

    let upload_req_file = warp::post()
        .and(
            warp::path("test")
                .and(warp::path("requests"))
                .and(warp::path::end()),
        )
        .and(warp::multipart::form().max_length(20 * 1024 * 1024))
        .and_then(upload_req_file);

    let stop_req = warp::path!("test" / "stop" / String)
        .and_then(|job_id: String| async move { stop(job_id).await });

    let history = warp::path!("test" / "status")
        .and(warp::query::<PagerOptions>())
        .and_then(|pager_option: PagerOptions| async move {
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
        .and_then(|request: Request| async move {
            HTTP_COUNTER.inc();
            execute(request).await
        });

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
        .or(upload_req_file);
    #[cfg(feature = "cluster")]
    let routes = routes
        .or(info)
        .or(request_vote)
        .or(request_vote_response)
        .or(heartbeat)
        .or(overload_req_secondary);

    // let test_zero_copy = warp::post()
    //     .and(
    //         warp::path("zero")
    //             .and(warp::path("copy"))
    //             .and(warp::path::end()),
    //     )
    //     .and(warp::body::bytes())
    //     .and_then(|zc: Bytes| async move { zero_copy_async(zc).await });
    // .map(|zc: Bytes| {
    //     let zc= serde_json::from_slice::<ZC>(zc.as_ref());
    //     format!("ACK: {:?}", zc)
    // });

    warp::serve(routes).run(([0, 0, 0, 0], 3030)).await;
}

// #[derive(Debug, serde::Deserialize, serde::Serialize)]
// struct ZC<'a> {
//     #[serde(borrow)]
//     pub id: &'a str,
//     // pub id: Cow<'a, str>,
//     #[serde(borrow)]
//     // pub other: Cow<'a, [u8]>,-features
//     pub other: &'a [u8],
// }
//
// async fn zero_copy_async(option: Bytes) -> Result<impl Reply, Infallible> {
//     // option.as_ref().to_owned();
//     // let zc = serde_json::from_slice::<ZC>(&option.as_ref().to_owned()).unwrap();
//     // format!("ACK: {:?}", zc)
//     tokio::spawn(some_shit(option));
//     Ok(warp::reply::json(&"{}"))
// }

// async fn some_shit(zc: Bytes) {
//     let zc = serde_json::from_slice::<ZC>(zc.as_ref()).unwrap();
//     println!("{:?}", &zc);
// }

async fn all_job(option: PagerOptions) -> Result<impl Reply, Infallible> {
    trace!("req: all_job: {:?}", &option);
    let status = handle_history_all(option.offset.unwrap_or(0), option.limit.unwrap_or(20)).await;
    trace!("resp: all_job: {:?}", &status);
    Ok(warp::reply::json(&status))
}

async fn upload_req_file(form: FormData) -> Result<impl Reply, Rejection> {
    trace!("req: upload_req_file");
    let result = http_util::upload_file(form).await;
    match result {
        Ok(resp) => {
            Ok(warp::reply::json(&resp))
        }
        Err(e) => Err(warp::reject::custom(Rejectable{err:e})),
    }
}

// #[cfg(not(feature = "cluster"))]
async fn execute(request: Request) -> Result<impl Reply, Infallible> {
    trace!("req: execute: {:?}", &request);
    let response = overload::http_util::handle_request(request).await;
    let json = warp::reply::json(&response);
    trace!("resp: execute: {:?}", &response);
    Ok(json)
}

#[cfg(feature = "cluster")]
async fn execute_cluster(request: Request) -> Result<impl Reply, Infallible> {
    trace!("req: execute_cluster: {:?}", &request);
    let response = overload::http_util::handle_request_cluster(request, CLUSTER.clone()).await;
    let json = warp::reply::json(&response);
    trace!("resp: execute_cluster: {:?}", &response);
    Ok(json)
}

async fn stop(job_id: String) -> Result<impl Reply, Infallible> {
    let resp = overload::http_util::stop(job_id).await;
    let json = warp::reply::json(&resp);
    Ok(json)
}
#[cfg(feature = "cluster")]
async fn cluster_info(cluster: Arc<Cluster>, cluster_mode: bool) -> Result<impl Reply, Infallible> {
    if !cluster_mode {
        let err = no_cluster_err();
        Ok(err)
    } else {
        let info = get_cluster_info(cluster).await;
        // let json = warp::reply::json(&info);
        let json = warp::reply::with_status(warp::reply::json(&info), StatusCode::OK);
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
        let json = warp::reply::with_status(warp::reply::json(&Value::Null), StatusCode::OK);
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
        let json = warp::reply::with_status(warp::reply::json(&Value::Null), StatusCode::OK);
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
        let json = warp::reply::with_status(warp::reply::json(&Value::Null), StatusCode::OK);
        Ok(json)
    }
}

#[cfg(feature = "cluster")]
fn no_cluster_err() -> WithStatus<Json> {
    let error_msg: Value = serde_json::from_str("{\"error\":\"Cluster is not running\"}").unwrap();
    warp::reply::with_status(warp::reply::json(&error_msg), StatusCode::NOT_FOUND)
}

#[derive(Debug)]
struct Rejectable<T> {
    err: T,
}
impl<T: Debug + Sync + Send + 'static> Reject for Rejectable<T> {}
