use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

static USED_PORTS: Mutex<Option<HashSet<u16>>> = Mutex::new(None);

pub fn pick_unused_port() -> u16 {
    let mut guard = USED_PORTS.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    loop {
        if let Some(port) = portpicker::pick_unused_port()
            && set.insert(port)
        {
            return port;
        }
    }
}

#[derive(Clone)]
pub struct EchoTarget {
    port: u16,
    hits: Arc<Mutex<HashMap<String, usize>>>,
    _shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl EchoTarget {
    pub async fn start() -> Self {
        let hits = Arc::new(Mutex::new(HashMap::new()));
        let hits_clone = hits.clone();

        let make_svc = make_service_fn(move |_conn| {
            let hits = hits_clone.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
                    let hits = hits.clone();
                    async move {
                        let path = req.uri().path().to_string();
                        {
                            let mut map = hits.lock().unwrap();
                            *map.entry(path).or_insert(0) += 1;
                        }

                        let method = req.method().as_str().to_string();
                        let host_hdr = req
                            .headers()
                            .get(hyper::header::HOST)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("127.0.0.1")
                            .to_string();
                        let url = format!("http://{}{}", host_hdr, req.uri());

                        let mut headers = HashMap::new();
                        for (k, v) in req.headers() {
                            if let Ok(v_str) = v.to_str() {
                                headers.insert(k.as_str().to_string(), v_str.to_string());
                            }
                        }

                        let body_bytes = hyper::body::to_bytes(req.into_body())
                            .await
                            .unwrap_or_default();
                        let data = String::from_utf8_lossy(&body_bytes).to_string();
                        let json: serde_json::Value =
                            serde_json::from_str(&data).unwrap_or(serde_json::Value::Null);

                        let resp_obj = serde_json::json!({
                            "data": data,
                            "headers": headers,
                            "json": json,
                            "method": method,
                            "url": url,
                        });

                        let resp_bytes = serde_json::to_vec(&resp_obj).unwrap();
                        let response = Response::builder()
                            .status(StatusCode::OK)
                            .header(hyper::header::CONTENT_TYPE, "application/json")
                            .header(hyper::header::CONNECTION, "keep-alive")
                            .body(Body::from(resp_bytes))
                            .unwrap();
                        Ok::<_, Infallible>(response)
                    }
                }))
            }
        });

        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let server = Server::bind(&addr).serve(make_svc);
        let port = server.local_addr().port();

        let (tx, rx) = oneshot::channel();
        let graceful = server.with_graceful_shutdown(async {
            rx.await.ok();
        });

        tokio::spawn(graceful);

        Self {
            port,
            hits,
            _shutdown: Arc::new(Mutex::new(Some(tx))),
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn hits(&self, path: &str) -> usize {
        let map = self.hits.lock().unwrap();
        map.get(path).copied().unwrap_or(0)
    }
}

impl Drop for EchoTarget {
    fn drop(&mut self) {
        if let Ok(mut guard) = self._shutdown.lock()
            && let Some(tx) = guard.take()
        {
            let _ = tx.send(());
        }
    }
}

#[cfg(not(feature = "cluster"))]
pub struct StandaloneServer {
    child: std::process::Child,
    data_dir: std::path::PathBuf,
    api_addr: String,
    metrics_addr: String,
}

#[cfg(not(feature = "cluster"))]
impl StandaloneServer {
    pub async fn start() -> Self {
        let http_port = pick_unused_port();
        let remoc_port = pick_unused_port();

        let data_dir =
            std::env::temp_dir().join(format!("overload-e2e-standalone-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("failed to create standalone temp data dir");

        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_overload"));
        cmd.env("HTTP_ENDPOINT_PORT", http_port.to_string())
            .env("CLUSTER_COM_PORT", remoc_port.to_string())
            .env("DATA_DIR", data_dir.to_str().unwrap());

        if std::env::var("OVERLOAD_TEST_SERVER_LOGS").is_err() {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }

        let child = cmd.spawn().expect("failed to spawn overload binary");

        let api_addr = format!("http://127.0.0.1:{}", http_port);
        let metrics_addr = format!("http://127.0.0.1:{}", http_port);

        // Wait until /metrics answers
        let client = reqwest::Client::new();
        let start = Instant::now();
        let timeout = Duration::from_secs(15);
        let mut ready = false;
        while start.elapsed() < timeout {
            let res = client
                .get(format!("{}/metrics", &metrics_addr))
                .send()
                .await;
            if let Ok(res) = res
                && res.status().is_success()
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        if !ready {
            panic!(
                "Standalone server failed to become ready at {} within 15s",
                &metrics_addr
            );
        }

        Self {
            child,
            data_dir,
            api_addr,
            metrics_addr,
        }
    }

    pub fn api_addr(&self) -> String {
        self.api_addr.clone()
    }

    pub fn metrics_addr(&self) -> String {
        self.metrics_addr.clone()
    }
}
#[cfg(not(feature = "cluster"))]
impl Drop for StandaloneServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

#[cfg(feature = "cluster")]
pub struct ClusterHandle {
    pub api_addr: String,
    pub metrics_addrs: Vec<String>,
}

#[cfg(feature = "cluster")]
static CLUSTER: tokio::sync::OnceCell<ClusterHandle> = tokio::sync::OnceCell::const_new();

#[cfg(feature = "cluster")]
impl ClusterHandle {
    pub async fn instance() -> &'static ClusterHandle {
        CLUSTER
            .get_or_init(|| async { ClusterHandle::start().await })
            .await
    }

    async fn start() -> Self {
        let mut http_ports = Vec::new();
        let mut remoc_ports = Vec::new();
        for _ in 0..4 {
            http_ports.push(pick_unused_port());
            remoc_ports.push(pick_unused_port());
        }

        let peers_str = (0..4)
            .map(|i| format!("127.0.0.1:{}:{}", http_ports[i], remoc_ports[i]))
            .collect::<Vec<_>>()
            .join(",");

        let data_dir =
            std::env::temp_dir().join(format!("overload-e2e-cluster-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("failed to create shared cluster temp data dir");
        let data_dir_str = data_dir.to_str().unwrap().to_string();

        let parent_pid = std::process::id();
        let exe_path = env!("CARGO_BIN_EXE_overload");

        for i in 0..4 {
            let http_port = http_ports[i];
            let remoc_port = remoc_ports[i];

            let script = r#"
"$EXE" &
CHILD=$!
while kill -0 "$PARENT" 2>/dev/null && kill -0 "$CHILD" 2>/dev/null; do
    sleep 1
done
kill -9 "$CHILD" 2>/dev/null
wait "$CHILD" 2>/dev/null
rm -rf "$DATA_DIR" 2>/dev/null
"#;
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg(script);
            cmd.env("PARENT", parent_pid.to_string())
                .env("EXE", exe_path)
                .env("HTTP_ENDPOINT_PORT", http_port.to_string())
                .env("CLUSTER_COM_PORT", remoc_port.to_string())
                .env("DATA_DIR", &data_dir_str)
                .env("CLUSTER_STATIC_PEERS", &peers_str)
                .env("CLUSTER_UPDATE_INTERVAL", "11")
                .env("CLUSTER_ELECTION_TIMEOUT", "11");

            if std::env::var("OVERLOAD_TEST_SERVER_LOGS").is_err() {
                cmd.stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
            }

            let _ = cmd
                .spawn()
                .expect("failed to spawn cluster node watchdog")
                .wait();
        }

        // Wait until POST /test stops returning InactiveCluster or NoSecondary, using a harmless 1-second job.
        // Keep that probe job's target alive for about 3 seconds, so the secondaries don't hammer a dead port.
        let probe_target = EchoTarget::start().await;
        let probe_port = probe_target.port();

        let probe_req = serde_json::json!({
            "duration": 1,
            "name": "cluster-probe",
            "qps": {
                "ConstantRate": {
                    "countPerSec": 1
                }
            },
            "req": {
                "RequestList": {
                    "data": [
                        {
                            "method": "GET",
                            "url": "/probe"
                        }
                    ]
                }
            },
            "target": {
                "host": "127.0.0.1",
                "port": probe_port,
                "protocol": "HTTP"
            }
        });

        let client = reqwest::Client::new();
        let start = Instant::now();
        let timeout = Duration::from_secs(60);
        let mut primary_port = http_ports[0];
        let mut ready = false;

        while start.elapsed() < timeout {
            for &port in &http_ports {
                let url = format!("http://127.0.0.1:{}/test", port);
                if let Ok(res) = client.post(&url).json(&probe_req).send().await
                    && let Ok(text) = res.text().await
                    && !text.contains("InactiveCluster")
                    && !text.contains("NoSecondary")
                    && (text.contains("Starting") || text.contains("job_id"))
                {
                    primary_port = port;
                    ready = true;
                    break;
                }
            }
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        if !ready {
            panic!("Cluster failed to form and become ready within 60s");
        }

        // Keep that probe job's target alive for about 3 seconds
        tokio::time::sleep(Duration::from_secs(3)).await;
        drop(probe_target);

        let api_addr = format!("http://127.0.0.1:{}", primary_port);
        let metrics_addrs = http_ports
            .iter()
            .map(|p| format!("http://127.0.0.1:{}", p))
            .collect();

        Self {
            api_addr,
            metrics_addrs,
        }
    }
}

pub struct TestEnv {
    api_addr: String,
    metrics_addrs: Vec<String>,
    pub target: EchoTarget,
    #[cfg(not(feature = "cluster"))]
    #[allow(dead_code)]
    server: Option<StandaloneServer>,
}

impl TestEnv {
    pub async fn start() -> Self {
        let target = EchoTarget::start().await;

        #[cfg(not(feature = "cluster"))]
        {
            let server = StandaloneServer::start().await;
            let api_addr = server.api_addr();
            let metrics_addrs = vec![server.metrics_addr()];
            Self {
                api_addr,
                metrics_addrs,
                target,
                server: Some(server),
            }
        }

        #[cfg(feature = "cluster")]
        {
            let cluster = ClusterHandle::instance().await;
            Self {
                api_addr: cluster.api_addr.clone(),
                metrics_addrs: cluster.metrics_addrs.clone(),
                target,
            }
        }
    }

    pub fn api_address(&self) -> &str {
        &self.api_addr
    }

    pub fn metrics_addresses(&self) -> &[String] {
        &self.metrics_addrs
    }

    pub fn target_port(&self) -> u16 {
        self.target.port()
    }
}
