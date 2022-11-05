#![allow(deprecated)]
#[cfg(feature = "cluster")]
use cloud_discovery_kubernetes::KubernetesDiscoverService;
use cluster_executor::{remoc_port, REMOC_PORT};
use log::info;
use overload::data_dir;
#[cfg(feature = "cluster")]
use rust_cloud_discovery::DiscoveryClient;
#[cfg(feature = "cluster")]
use std::env;

#[cfg_attr(feature = "cluster", path = "filters_cluster.rs")]
mod filters;
mod filters_common;

#[tokio::main]
async fn main() {
    //init logging
    tracing_subscriber::fmt::init();
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
    tokio::spawn(overload::init());

    let remoc_port = REMOC_PORT.get_or_init(remoc_port);
    tokio::spawn(cluster_executor::secondary::primary_listener(
        *remoc_port,
        &overload_metrics::METRICS_FACTORY,
    ));

    let routes = filters::get_routes();
    info!("staring server...");
    warp::serve(routes).run(([0, 0, 0, 0], 3030)).await;
}
