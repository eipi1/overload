#[cfg(feature = "cluster")]
use cloud_discovery_kubernetes::KubernetesDiscoverService;
use log::info;
use overload::data_dir;
#[cfg(feature = "cluster")]
use rust_cloud_discovery::DiscoveryClient;
use std::env;

#[cfg_attr(feature = "cluster", path = "filters_cluster.rs")]
mod filters;
mod filters_common;

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
                    filters::CLUSTER.clone(),
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

    let routes = filters::get_routes();
    info!("staring server...");
    warp::serve(routes).run(([0, 0, 0, 0], 3030)).await;
}
