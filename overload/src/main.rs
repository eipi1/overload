#![allow(deprecated)]
#[cfg(feature = "cluster")]
use cloud_discovery_kubernetes::KubernetesDiscoverService;
use cluster_executor::{remoc_port, REMOC_PORT};
#[cfg(feature = "cluster")]
use cluster_mode::ClusterConfig;
use log::info;
use overload::data_dir;
#[cfg(feature = "cluster")]
use rust_cloud_discovery::DiscoveryClient;
#[cfg(feature = "cluster")]
use std::env;
#[cfg(feature = "cluster")]
use std::num::NonZeroUsize;
#[cfg(feature = "cluster")]
use std::str::FromStr;

#[cfg_attr(feature = "cluster", path = "filters_cluster.rs")]
mod filters;
mod filters_common;

#[cfg(feature = "cluster")]
const ENV_NAME_CLUSTER_UPDATE_INTERVAL: &str = "CLUSTER_UPDATE_INTERVAL";
#[cfg(feature = "cluster")]
const ENV_NAME_CLUSTER_ELECTION_TIMEOUT: &str = "CLUSTER_ELECTION_TIMEOUT";
#[cfg(feature = "cluster")]
const ENV_NAME_CLUSTER_MAX_NODE: &str = "CLUSTER_MAX_NODE";
#[cfg(feature = "cluster")]
const ENV_NAME_CLUSTER_MIN_NODE: &str = "CLUSTER_MIN_NODE";

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
                let config = cluster_config();
                info!("cluster configuration - connection_timeout:{}, election_timeout:{}, update_interval: {}, max_node: {}, min_node:{}",
                    &config.connection_timeout, &config.election_timeout, &config.update_interval,&config.max_node,&config.min_node);
                tokio::spawn(cluster_mode::start_cluster(
                    filters::CLUSTER.clone(),
                    discovery_client,
                    config,
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

#[cfg(feature = "cluster")]
fn cluster_config() -> ClusterConfig {
    let mut config = ClusterConfig::default();
    if let Some(value) = get_env::<u64, _>(ENV_NAME_CLUSTER_UPDATE_INTERVAL, |v| *v > 10) {
        config.update_interval = value * 1000;
    }

    if let Some(value) = get_env::<u64, _>(ENV_NAME_CLUSTER_ELECTION_TIMEOUT, |v| *v > 10) {
        config.election_timeout = value * 1000;
    }

    if let Some(value) =
        get_env::<NonZeroUsize, _>(ENV_NAME_CLUSTER_MAX_NODE, |value| value.get() > 4)
    {
        config.max_node = value;
    }

    if let Some(value) =
        get_env::<NonZeroUsize, _>(ENV_NAME_CLUSTER_MIN_NODE, |value| value.get() > 4)
    {
        config.min_node = value;
    }
    config
}

#[cfg(feature = "cluster")]
fn get_env<T, P>(env_var: &str, filter: P) -> Option<T>
where
    T: FromStr,
    P: FnOnce(&T) -> bool,
{
    env::var(env_var)
        .ok()
        .and_then(|s| T::from_str(&s).ok())
        .filter(filter)
}
