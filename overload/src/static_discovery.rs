use async_trait::async_trait;
use rust_cloud_discovery::{DiscoveryService, Port, ServiceInstance};
use std::collections::HashMap;
use std::error::Error;

pub const ENV_NAME_CLUSTER_STATIC_PEERS: &str = "CLUSTER_STATIC_PEERS";

#[derive(Clone, Debug)]
pub struct StaticDiscoveryService {
    instances: Vec<ServiceInstance>,
}

impl StaticDiscoveryService {
    pub fn new(peers_str: &str) -> Result<Self, String> {
        let instances = parse_peers(peers_str)?;
        Ok(Self { instances })
    }

    #[allow(dead_code)]
    pub fn from_env() -> Option<Result<Self, String>> {
        std::env::var(ENV_NAME_CLUSTER_STATIC_PEERS)
            .ok()
            .map(|s| Self::new(&s))
    }
}

pub fn parse_peers(peers_str: &str) -> Result<Vec<ServiceInstance>, String> {
    let mut instances = Vec::new();
    for peer in peers_str.split(',') {
        let peer = peer.trim();
        if peer.is_empty() {
            continue;
        }
        let parts: Vec<&str> = peer.split(':').collect();
        if parts.len() != 3 {
            return Err(format!(
                "Invalid peer format: expected host:http_port:remoc_port, got {}",
                peer
            ));
        }
        let host = parts[0].trim();
        let http_port: u32 = parts[1]
            .trim()
            .parse()
            .map_err(|e| format!("Invalid http_port in {}: {}", peer, e))?;
        let remoc_port: u32 = parts[2]
            .trim()
            .parse()
            .map_err(|e| format!("Invalid remoc_port in {}: {}", peer, e))?;

        let instance = ServiceInstance::new(
            Some(format!("{}:{}", host, http_port)),
            Some("overload".to_string()),
            Some(host.to_string()),
            Some(vec![
                Port::new(
                    Some("http-endpoint".to_string()),
                    http_port,
                    "TCP".to_string(),
                    None,
                ),
                Port::new(
                    Some(cluster_executor::remoc_port_name()),
                    remoc_port,
                    "TCP".to_string(),
                    None,
                ),
            ]),
            false,
            Some(format!("http://{}:{}", host, http_port)),
            HashMap::new(),
            Some("http".to_string()),
        );
        instances.push(instance);
    }
    Ok(instances)
}

#[async_trait]
impl DiscoveryService for StaticDiscoveryService {
    async fn discover_instances(&self) -> Result<Vec<ServiceInstance>, Box<dyn Error>> {
        Ok(self.instances.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_peers_valid() {
        let peers_str = "127.0.0.1:3030:3031, 10.0.0.2:8080:8081";
        let instances = parse_peers(peers_str).unwrap();
        assert_eq!(instances.len(), 2);

        let i0 = &instances[0];
        assert_eq!(i0.instance_id().as_deref(), Some("127.0.0.1:3030"));
        assert_eq!(i0.uri().as_deref(), Some("http://127.0.0.1:3030"));
        assert_eq!(i0.scheme().as_deref(), Some("http"));
        assert_eq!(i0.host().as_deref(), Some("127.0.0.1"));

        let ports0 = i0.get_ports().as_ref().unwrap();
        assert_eq!(ports0.len(), 2);
        assert_eq!(ports0[0].get_name().as_deref(), Some("http-endpoint"));
        assert_eq!(ports0[0].get_port(), 3030);
        assert_eq!(ports0[0].get_protocol(), "TCP");
        assert_eq!(
            ports0[1].get_name().as_deref(),
            Some(cluster_executor::remoc_port_name().as_str())
        );
        assert_eq!(ports0[1].get_port(), 3031);
        assert_eq!(ports0[1].get_protocol(), "TCP");

        let i1 = &instances[1];
        assert_eq!(i1.instance_id().as_deref(), Some("10.0.0.2:8080"));
        assert_eq!(i1.uri().as_deref(), Some("http://10.0.0.2:8080"));
        assert_eq!(i1.scheme().as_deref(), Some("http"));
        assert_eq!(i1.host().as_deref(), Some("10.0.0.2"));

        let ports1 = i1.get_ports().as_ref().unwrap();
        assert_eq!(ports1[0].get_name().as_deref(), Some("http-endpoint"));
        assert_eq!(ports1[0].get_port(), 8080);
        assert_eq!(ports1[0].get_protocol(), "TCP");
        assert_eq!(ports1[1].get_port(), 8081);
        assert_eq!(ports1[1].get_protocol(), "TCP");
    }

    #[test]
    fn test_parse_peers_invalid() {
        assert!(parse_peers("invalid").is_err());
        assert!(parse_peers("127.0.0.1:3030").is_err());
        assert!(parse_peers("127.0.0.1:not_a_port:3031").is_err());
        assert!(parse_peers("127.0.0.1:3030:not_a_port").is_err());
    }

    #[test]
    fn test_parse_peers_empty() {
        let instances = parse_peers("").unwrap();
        assert!(instances.is_empty());
    }
}
