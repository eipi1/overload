#![cfg(feature = "cluster")]

use cluster_executor::primary::handle_request;
use cluster_executor::secondary::primary_listener;
use cluster_executor::stop_request_by_job_id;
use cluster_mode::InstanceMode::Primary;
use cluster_mode::{Cluster, RestClusterNode, RestClusterNodeId};
use httpmock::Mock;
use more_asserts::assert_ge;
use rust_cloud_discovery::{Port, ServiceInstance};
use std::collections::{HashMap, HashSet};
use std::process::{Child, Command};
use std::sync::{Arc, Once};
use std::time::Duration;
use uuid::Uuid;

struct SecondaryHandle {
    child: Child,
    count_file: String,
}

impl SecondaryHandle {
    fn read_count(&self) -> u64 {
        std::fs::read_to_string(&self.count_file)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for SecondaryHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.count_file);
    }
}

fn spawn_secondary() -> (SecondaryHandle, u16) {
    let port = portpicker::pick_unused_port().expect("no free port for secondary");
    let count_file = format!("/tmp/sec_count_{}_{}.txt", port, Uuid::new_v4());
    let _ = std::fs::write(&count_file, "0");

    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "secondary_worker", "--ignored", "--nocapture"])
        .env("SECONDARY_PORT", port.to_string())
        .env("COUNT_FILE", &count_file)
        .spawn()
        .expect("failed to spawn secondary child process");

    (SecondaryHandle { child, count_file }, port)
}

#[tokio::test]
#[ignore]
async fn secondary_worker() {
    let Ok(port_str) = std::env::var("SECONDARY_PORT") else {
        return;
    };
    let port: u16 = port_str.parse().unwrap();
    let count_file = std::env::var("COUNT_FILE").unwrap();

    let cf = count_file.clone();
    tokio::spawn(async move {
        let mut last_count = 0;
        let _ = std::fs::write(&cf, "0");
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut current = 0;
            for mf in overload_metrics::METRICS_FACTORY.registry().gather() {
                if mf.get_name() == "upstream_request_count" {
                    for m in mf.get_metric() {
                        current += m.get_counter().get_value() as u64;
                    }
                }
            }
            if current > last_count {
                last_count = current;
                let _ = std::fs::write(&cf, current.to_string());
            }
        }
    });

    primary_listener(port, &overload_metrics::METRICS_FACTORY).await;
}

fn make_cluster_node(remoc_port: u16) -> RestClusterNode {
    let id = Uuid::new_v4().to_string();
    let port1 = Port::new(
        Some("http-endpoint".to_string()),
        8080,
        "tcp".to_string(),
        Some("http".to_string()),
    );
    let port2 = Port::new(
        Some("tcp-remoc".to_string()),
        remoc_port as u32,
        "tcp".to_string(),
        Some("tcp".to_string()),
    );
    RestClusterNode::new(
        RestClusterNodeId::from(id.clone()),
        ServiceInstance::new(
            Some(id.clone()),
            Some("test".to_string()),
            Some("127.0.0.1".to_string()),
            Some(vec![port1, port2]),
            false,
            Some("http://127.0.0.1:8080".to_string()),
            HashMap::new(),
            Some("HTTP".to_string()),
        ),
    )
}

fn make_request(target_port: u16, duration: u32, qps: u32) -> (overload_http::Request, String) {
    let job_id = Uuid::new_v4().to_string();
    let req = serde_json::json!({
        "duration": duration,
        "name": job_id,
        "qps": {
            "ConstantRate": {
                "countPerSec": qps
            }
        },
        "req": {
            "RequestList": {
                "data": [
                    {
                        "method": "GET",
                        "url": "/test"
                    }
                ]
            }
        },
        "target": {
            "host": "127.0.0.1",
            "port": target_port,
            "protocol": "HTTP"
        }
    });
    (
        serde_json::from_value::<overload_http::Request>(req).unwrap(),
        job_id,
    )
}

fn get_cluster(ports: &[u16]) -> Arc<Cluster<RestClusterNode>> {
    let nodes: Vec<RestClusterNode> = ports.iter().map(|&p| make_cluster_node(p)).collect();
    // let instances: Vec<ServiceInstance> = nodes.iter().map(|n| n.service_instance().clone()).collect();
    let cluster = Cluster::_new(Primary, nodes.into_iter().collect::<HashSet<_>>());
    Arc::new(cluster)
}

static INIT: Once = Once::new();
pub fn init_logger() {
    INIT.call_once(|| {
        env_logger::init_from_env(
            env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "trace"),
        );
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn fair_split() {
    init_logger();
    let mock_server = httpmock::MockServer::start_async().await;
    let mock = mock_server.mock(|when, then| {
        when.method(httpmock::Method::GET).path("/test");
        then.status(200).body("ok");
    });

    let (sec1, p1) = spawn_secondary();
    let (sec2, p2) = spawn_secondary();
    let (sec3, p3) = spawn_secondary();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let cluster = get_cluster(&[p1, p2, p3]);
    let (duration, qps) = (4, 10);
    let (request, _job_id) = make_request(mock_server.port(), duration, qps);

    let handle = tokio::spawn(handle_request(request, cluster));

    // Check hit count matches expectation every second
    let hit_counts = collect_hit_counts(&mock, duration).await;

    assert_eq!(&hit_counts, &[10, 20, 30, 40]);

    handle.await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let total_hits = mock.hits_async().await;
    let count1 = sec1.read_count();
    let count2 = sec2.read_count();
    let count3 = sec3.read_count();

    println!(
        "fair_split - total: {}, sec1: {}, sec2: {}, sec3: {}",
        total_hits, count1, count2, count3
    );

    // Target receives about the expected total (4s * 10 qps = ~40)
    assert!(
        (35..=45).contains(&total_hits),
        "total hits {} not in 35..=45",
        total_hits
    );

    // Each secondary carries roughly a third
    assert!(
        (10..=17).contains(&count1),
        "sec1 count {} not in roughly a third",
        count1
    );
    assert!(
        (10..=17).contains(&count2),
        "sec2 count {} not in roughly a third",
        count2
    );
    assert!(
        (10..=17).contains(&count3),
        "sec3 count {} not in roughly a third",
        count3
    );
}

async fn collect_hit_counts(mock: &Mock<'_>, duration: u32) -> Vec<usize> {
    let mut hit_counts = vec![];
    for _ in 1..=duration {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let hits = mock.hits_async().await;
        hit_counts.push(hits);
    }
    hit_counts
}

#[tokio::test(flavor = "multi_thread")]
async fn lost_secondary() {
    init_logger();
    let mock_server = httpmock::MockServer::start_async().await;
    let mock = mock_server.mock(|when, then| {
        when.method(httpmock::Method::GET).path("/test");
        then.status(200).body("ok");
    });

    let (mut sec1, p1) = spawn_secondary();
    let (sec2, p2) = spawn_secondary();
    let (sec3, p3) = spawn_secondary();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let cluster = get_cluster(&[p1, p2, p3]);
    let (duration, qps) = (6, 9usize);
    let (request, _job_id) = make_request(mock_server.port(), duration, qps as u32);

    let executor = async {
        // put join in async block otherwise join will wait for both futures to complete
        let (_, _) = tokio::join!(handle_request(request, cluster), async {
            tokio::time::sleep(Duration::from_millis(2500)).await;
            sec1.kill();
        });
    };

    let (_, hit_res) = tokio::join!(executor, collect_hit_counts(&mock, duration));
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(hit_res[0..=1], [qps, qps * 2]);
    let request_in_lost_second = qps * 2 + ((qps / 3) * 2);
    assert_ge!(hit_res[2], request_in_lost_second);
    let less_req_due_to_dead_secondary = (qps * 3) - request_in_lost_second;
    println!(
        "less_req_due_to_dead_secondary: {}, original expectation: {}, actual request: {} ",
        less_req_due_to_dead_secondary,
        qps * 3,
        request_in_lost_second
    );
    assert_eq!(
        hit_res[3..],
        [
            qps * 4 - less_req_due_to_dead_secondary,
            qps * 5 - less_req_due_to_dead_secondary,
            qps * 6 - less_req_due_to_dead_secondary
        ]
    );

    let total_hits = mock.hits_async().await;
    let count1 = sec1.read_count();
    let count2 = sec2.read_count();
    let count3 = sec3.read_count();

    println!(
        "lost_secondary - total: {}, sec1 (killed): {}, sec2: {}, sec3: {}",
        total_hits, count1, count2, count3
    );

    // Target still receives about the full total (6s * 9 qps = ~54)
    assert!(
        (45..=60).contains(&total_hits),
        "total hits {} not near 54",
        total_hits
    );

    // The dead secondary sent only a small share
    assert!(
        count1 < 15,
        "killed secondary sent too many requests: {}",
        count1
    );

    // The two survivors each sent clearly more than a third (> 18)
    assert!(count2 > 18, "survivor 2 sent too few requests: {}", count2);
    assert!(count3 > 18, "survivor 3 sent too few requests: {}", count3);
}

#[tokio::test(flavor = "multi_thread")]
async fn stopped_job() {
    init_logger();
    let mock_server = httpmock::MockServer::start_async().await;
    let mock = mock_server.mock(|when, then| {
        when.method(httpmock::Method::GET).path("/test");
        then.status(200).body("ok");
    });

    let (_sec1, p1) = spawn_secondary();
    let (_sec2, p2) = spawn_secondary();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let (request, job_id) = make_request(mock_server.port(), 30, 4);

    let cluster = get_cluster(&[p1, p2]);

    let handle = tokio::spawn(handle_request(request, cluster));

    // Check hit count matches expectation every second:
    // Second 1
    tokio::time::sleep(Duration::from_secs(1)).await;
    let hits1 = mock.hits_async().await;
    println!("stopped_job - second 1: hits = {}", hits1);
    assert!(
        (2..=6).contains(&hits1),
        "stopped_job second 1: hits {} not within 2..=6",
        hits1
    );

    // Second 2
    tokio::time::sleep(Duration::from_secs(1)).await;
    let hits2 = mock.hits_async().await;
    println!("stopped_job - second 2: hits = {}", hits2);
    assert!(
        (5..=11).contains(&hits2),
        "stopped_job second 2: hits {} not within 5..=11",
        hits2
    );

    // Stop the job a few seconds in (at 2.5s)
    tokio::time::sleep(Duration::from_millis(500)).await;
    stop_request_by_job_id(&job_id).await;

    // Primary returns well before the planned duration (30s)
    let res = tokio::time::timeout(Duration::from_secs(10), handle).await;
    assert!(
        res.is_ok(),
        "handle_request did not finish within timeout after stop"
    );

    let hits_at_finish = mock.hits_async().await;
    // Some requests were sent before the stop, but well short of 30s * 4 = 120
    assert!(hits_at_finish > 0, "no requests sent before stop");
    assert!(
        hits_at_finish < 60,
        "too many requests sent despite stop: {}",
        hits_at_finish
    );

    // Nothing more reaches the target after the primary finishes
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let hits_after = mock.hits_async().await;
    assert_eq!(
        hits_after, hits_at_finish,
        "extra requests arrived after primary finished"
    );
}
