#[cfg(feature = "cluster")]
use cloud_discovery_kubernetes::KubernetesDiscoverService;
#[cfg(feature = "cluster")]
use cluster_mode::Cluster;
use lazy_static::lazy_static;
use log::info;
use overload::data_dir;
#[cfg(feature = "cluster")]
use rust_cloud_discovery::DiscoveryClient;
use std::env;
#[cfg(feature = "cluster")]
use std::sync::Arc;
use warp::Filter;
mod filters;

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

    let upload_binary_file = filters::upload_binary_file();

    let stop_req = filters::stop_req();

    let history = filters::history();

    #[cfg(feature = "cluster")]
    let overload_req_secondary = filters::overload_req_secondary();

    // cluster-mode configurations
    #[cfg(feature = "cluster")]
    let info = filters::info();

    #[cfg(feature = "cluster")]
    let request_vote = filters::request_vote();

    #[cfg(feature = "cluster")]
    let request_vote_response = filters::request_vote_response();

    #[cfg(feature = "cluster")]
    let heartbeat = filters::heartbeat();

    let prometheus_metric = filters::prometheus_metric();
    let overload_req = filters::overload_req();
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
