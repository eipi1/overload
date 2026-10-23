mod harness;

use env_logger::Env;
use harness::TestEnv;
use log::info;
use reqwest::{Body, Url};
use rstest::rstest;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::ops::{Add, Range};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Once;
use std::time::Duration;
use tokio::time::sleep;

pub static TEST_PATH: &str = "/test";
pub const PATH_FILE_UPLOAD: &str = "/request-file/csv";
pub const PATH_FILE_UPLOAD_SQLITE: &str = "/request-file/sqlite";
pub static METRICS_PATH: &str = "/metrics";

static ONCE: Once = Once::new();
fn init_logger() {
    ONCE.call_once(|| {
        env_logger::Builder::from_env(Env::default().default_filter_or("info"))
            .format_timestamp_millis()
            .init();
    });
}

pub fn resource_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/resources")
}

#[tokio::test]
async fn test_scenarios_1() {
    let path = "test-assertion.json";
    init_logger();
    let env = TestEnv::start().await;

    let (test_spec, job_id) = send_test_req(&env, path).await;

    let duration = test_duration(&test_spec);
    let qps_expectation = qps_expectation(&test_spec);

    wait_for_job_start(&env, &job_id).await;
    sleep(Duration::from_secs(1)).await;
    for i in 1..duration - 1 {
        let all_metrics = get_all_metrics(&env).await;
        assert_request_count(i as usize, &job_id, &qps_expectation, &all_metrics).await;
        sleep(Duration::from_secs(1)).await;
    }
}

#[rstest]
#[case("test-assertion-fail-2.json")]
#[case("test-assertion-fail.json")]
#[case("test-generator-with-assertion.json")]
#[case("test-array-generator-with-assertion.json")]
#[case("test-generator-const-obj-with-assertion.json")]
#[case("test-generator-url-param-with-assertion.json")]
#[case("test-connection-keep-alive-default.json")]
#[case("test-connection-keep-alive.json")]
#[case("test-connection-keep-alive-ttl.json")]
#[case("test-connection-keep-alive-ttl-2.json")]
#[case("test-connection-keep-alive-max-req.json")]
#[case("test-load-generation-immediate.json")]
#[case("test-load-generation-batch.json")]
#[case("json-template-get.json")]
#[case("json-template-post.json")]
#[case("json-template-post-2.json")]
#[case("json-template-post-null-body.json")]
#[tokio::test(flavor = "multi_thread")]
async fn test_scenarios_assertion(#[case] path: &str) {
    init_logger();
    let env = TestEnv::start().await;

    let (test_spec, job_id) = send_test_req(&env, path).await;

    let duration = test_duration(&test_spec) as usize;
    let qps_expectation = qps_expectation(&test_spec);
    let failure_expectation = assert_failure_expectation(&test_spec);
    let conn_success_expectation =
        scale_connections(connection_success_expectation(&test_spec), &test_spec);
    let conn_dropped_expectation =
        scale_connections(connection_dropped_expectation(&test_spec), &test_spec);

    wait_for_job_start(&env, &job_id).await;
    sleep(Duration::from_secs(1)).await;
    for i in 1..duration - 1 {
        let all_metrics = get_all_metrics(&env).await;
        assert_request_count(i, &job_id, &qps_expectation, &all_metrics).await;
        assert_assertion_failure_count(i, &job_id, &failure_expectation, &all_metrics).await;
        assert_connection_success_count(i, &job_id, &conn_success_expectation, &all_metrics).await;
        assert_connection_dropped_count(i, &job_id, &conn_dropped_expectation, &all_metrics).await;
        sleep(Duration::from_secs(1)).await;
    }
}

#[tokio::test]
async fn test_scenarios_file_data() {
    let path = "test-with-file-data.json";
    init_logger();
    let env = TestEnv::start().await;

    let file_path = resource_dir().join(path);
    let mut test_spec =
        serde_json::from_reader::<_, Value>(File::open(file_path).unwrap()).unwrap();

    let file_name = test_spec.get("dataFileName").unwrap().as_str().unwrap();
    let file_id = upload_file(&env, file_name).await;

    let request = test_spec.get_mut("request").unwrap();

    let _ = request
        .get_mut("req")
        .and_then(|v| v.get_mut("RequestFile"))
        .and_then(|v| v.get_mut("file_name"))
        .map(|v| *v = Value::String(file_id));

    let job_id = send_test_req_with_json(&env, request.clone()).await;

    let duration = test_duration(&test_spec) as usize;
    let qps_expectation = qps_expectation(&test_spec);

    wait_for_job_start(&env, &job_id).await;
    sleep(Duration::from_secs(1)).await;
    for i in 1..duration - 1 {
        let all_metrics = get_all_metrics(&env).await;
        assert_request_count(i, &job_id, &qps_expectation, &all_metrics).await;
        sleep(Duration::from_secs(1)).await;
    }
}

#[rstest]
#[case("error-lua-parse-failure.json")]
#[case("error-no-file.json")]
#[case("error-wrong-constraint.json")]
#[tokio::test]
async fn test_error_requests(#[case] path: &str) {
    init_logger();
    let env = TestEnv::start().await;

    let file_path = resource_dir().join(path);
    let mut test_spec =
        serde_json::from_reader::<_, Value>(File::open(file_path).unwrap()).unwrap();

    let request = test_spec.get_mut("request").unwrap();

    let client = reqwest::Client::new();
    set_target(&env, request);
    let resp = client
        .post(format!("{}{}", env.api_address(), TEST_PATH))
        .json(&request)
        .send()
        .await
        .unwrap();
    let resp = resp.text().await.unwrap();
    info!("text resp: {}", &resp);
    let error = test_spec
        .get("expectation")
        .unwrap()
        .get("errorContains")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(resp.contains(error));
}

#[rstest]
#[case("test-with-split-file-data.json")]
#[case("test-with-split-file-data-2.json")]
#[case("test-with-split-file-data-3.json")]
#[case("test-with-split-file-data-sqlite.json")]
#[tokio::test]
async fn scenarios_with_file_and_mock_verification(#[case] path: &str) {
    init_logger();
    let env = TestEnv::start().await;

    let file_path = resource_dir().join(path);
    let mut test_spec =
        serde_json::from_reader::<_, Value>(File::open(file_path).unwrap()).unwrap();

    let file_name = test_spec.get("dataFileName").unwrap().as_str().unwrap();
    let file_id = upload_file(&env, file_name).await;

    let request = test_spec.get_mut("request").unwrap();

    let _ = request
        .get_mut("req")
        .and_then(|v| {
            let file = v.get("SplitRequestFile");
            if file.is_some() {
                v.get_mut("SplitRequestFile")
            } else {
                v.get_mut("RequestFile")
            }
        })
        .and_then(|v| v.get_mut("file_name"))
        .map(|v| *v = Value::String(file_id));

    let job_id = send_test_req_with_json(&env, request.clone()).await;

    let duration = test_duration(&test_spec) as usize;
    let qps_expectation = qps_expectation(&test_spec);
    let failure_expectation = assert_failure_expectation(&test_spec);

    wait_for_job_start(&env, &job_id).await;
    sleep(Duration::from_secs(1)).await;
    for i in 1..duration - 1 {
        let all_metrics = get_all_metrics(&env).await;
        assert_request_count(i, &job_id, &qps_expectation, &all_metrics).await;
        assert_assertion_failure_count(i, &job_id, &failure_expectation, &all_metrics).await;
        sleep(Duration::from_secs(1)).await;
    }

    sleep(Duration::from_secs(1)).await;
    let mock_expectation = mock_expectation(&test_spec);
    for (id, expectation) in mock_expectation {
        let hit = env.target.hits(&format!("/{path}/file/{id}"));
        assert_eq!(
            hit, expectation as usize,
            "assertion failure - mock:{id}, hit:{hit}, expectation: {expectation}"
        );
    }
}

#[rstest]
#[case("test-with-split-file-data-3-post.json")]
#[case("test-with-split-file-data-sqlite-post.json")]
#[tokio::test]
async fn scenarios_with_post_file_and_mock_verification(#[case] path: &str) {
    init_logger();
    let env = TestEnv::start().await;

    let file_path = resource_dir().join(path);
    let mut test_spec =
        serde_json::from_reader::<_, Value>(File::open(file_path).unwrap()).unwrap();

    let file_name = test_spec.get("dataFileName").unwrap().as_str().unwrap();
    let file_id = upload_file(&env, file_name).await;

    let request = test_spec.get_mut("request").unwrap();

    let _ = request
        .get_mut("req")
        .and_then(|v| {
            let file = v.get("SplitRequestFile");
            if file.is_some() {
                v.get_mut("SplitRequestFile")
            } else {
                v.get_mut("RequestFile")
            }
        })
        .and_then(|v| v.get_mut("file_name"))
        .map(|v| *v = Value::String(file_id));

    let job_id = send_test_req_with_json(&env, request.clone()).await;

    let duration = test_duration(&test_spec) as usize;
    let qps_expectation = qps_expectation(&test_spec);
    let failure_expectation = assert_failure_expectation(&test_spec);

    wait_for_job_start(&env, &job_id).await;
    sleep(Duration::from_secs(1)).await;
    for i in 1..duration - 1 {
        let all_metrics = get_all_metrics(&env).await;
        assert_request_count(i, &job_id, &qps_expectation, &all_metrics).await;
        assert_assertion_failure_count(i, &job_id, &failure_expectation, &all_metrics).await;
        sleep(Duration::from_secs(1)).await;
    }

    sleep(Duration::from_secs(1)).await;
    let mock_expectation = mock_expectation(&test_spec);
    for (id, expectation) in mock_expectation {
        let hit = env.target.hits(&format!("/{path}/file/{id}"));
        assert_eq!(
            hit, expectation as usize,
            "assertion failure - mock:{id}, hit:{hit}, expectation: {expectation}"
        );
    }
}

async fn wait_for_job_start(env: &TestEnv, job_id: &str) {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(10);
    while start.elapsed() < timeout {
        let metrics = get_all_metrics(env).await;
        let job_metrics = filter_metrics(&metrics, "upstream_request_count");
        let job_metrics = filter_metrics(&job_metrics, job_id);
        let val = get_value_for_metrics(&job_metrics);
        if val > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("job {} failed to start within 10s", job_id);
}

fn configured_concurrent_connections(test_spec: &Value) -> Option<u64> {
    let req = test_spec.get("request")?;
    let cc = req.get("concurrentConnection")?;
    if let Some(n) = cc.as_u64() {
        return Some(n);
    }
    if let Some(cr) = cc.get("ConstantRate")
        && let Some(count) = cr.get("countPerSec").and_then(|v| v.as_u64())
    {
        return Some(count);
    }
    None
}

fn scale_connections(expectations: Vec<u64>, test_spec: &Value) -> Vec<u64> {
    if !cfg!(feature = "cluster") || expectations.is_empty() {
        return expectations;
    }
    let secondaries = 3u64;
    if let Some(configured) = configured_concurrent_connections(test_spec)
        && configured > 0
        && configured < secondaries
    {
        assert_eq!(
            secondaries % configured,
            0,
            "secondaries ({}) must divide evenly by configured ({})",
            secondaries,
            configured
        );
        let factor = secondaries / configured;
        return expectations.into_iter().map(|v| v * factor).collect();
    }
    expectations
}

async fn upload_file(env: &TestEnv, file_name: &str) -> String {
    let address = env.api_address();
    let file = tokio::fs::File::open(resource_dir().join(file_name))
        .await
        .unwrap();
    let remote_path = if file_name.ends_with(".sqlite") {
        PATH_FILE_UPLOAD_SQLITE
    } else {
        PATH_FILE_UPLOAD
    };

    let client = reqwest::Client::new();
    let res = client
        .post(Url::from_str(address).unwrap().join(remote_path).unwrap())
        .header("content-length", file.metadata().await.unwrap().len())
        .body(Body::from(file))
        .send()
        .await
        .unwrap();
    let resp = res.text().await.unwrap();
    info!("text resp: {}", &resp);
    let test_resp: Value = serde_json::from_str(&resp).unwrap();
    let file_id = test_resp.get("file").and_then(|v| v.as_str()).unwrap();
    file_id.to_string()
}

async fn assert_request_count(i: usize, job_id: &str, qps_expectation: &[u64], metrics: &str) {
    let metrics = filter_metrics(metrics, "upstream_request_count");
    let metrics = filter_metrics(&metrics, job_id);
    assert_metrics_is_in_range(
        &metrics,
        (cumulative_sum::<u64>(qps_expectation, i, 0))
            ..(cumulative_sum::<u64>(qps_expectation, i + 2, 0) + 1),
    );
}

async fn assert_connection_success_count(
    i: usize,
    job_id: &str,
    expectation: &[u64],
    metrics: &str,
) {
    if expectation.is_empty() {
        return;
    }
    let metrics = filter_metrics(metrics, "connection_pool_new_connection_success");
    let metrics = filter_metrics(&metrics, job_id);
    println!("{}", &metrics);
    assert_metrics_is_in_range(
        &metrics,
        (cumulative_sum::<u64>(expectation, i, 0))
            ..(cumulative_sum::<u64>(expectation, i + 2, 0) + 1),
    );
}

async fn assert_connection_dropped_count(
    i: usize,
    job_id: &str,
    expectation: &[u64],
    metrics: &str,
) {
    if expectation.is_empty() {
        return;
    }
    let metrics = filter_metrics(metrics, "connection_pool_connection_dropped");
    let metrics = filter_metrics(&metrics, job_id);
    println!("{}", &metrics);
    assert_metrics_is_in_range(
        &metrics,
        (cumulative_sum::<u64>(expectation, i, 0))
            ..(cumulative_sum::<u64>(expectation, i + 2, 0) + 1),
    );
}

async fn assert_assertion_failure_count(
    i: usize,
    job_id: &str,
    failure_expectation: &HashMap<&String, Vec<u64>>,
    metrics: &str,
) {
    let metrics = filter_metrics(metrics, "assertion_failure");
    let metrics = filter_metrics(&metrics, job_id);
    info!("{}", &metrics);
    for (k, expectation) in failure_expectation {
        info!(
            "asserting assertions - key:{k}, expectation: {:?}",
            expectation
        );
        let metrics = filter_metrics(&metrics, &format!("assertion_id=\"{}\"", k));
        assert_metrics_is_in_range(
            &metrics,
            (cumulative_sum::<u64>(expectation, i, 0))
                ..(cumulative_sum::<u64>(expectation, i + 2, 0) + 1),
        );
    }
}

fn assert_failure_expectation(test_spec: &Value) -> HashMap<&String, Vec<u64>> {
    test_spec
        .get("expectation")
        .and_then(|v| v.get("assertionFailure"))
        .and_then(|v| v.as_object())
        .map(|v| {
            v.iter()
                .map(|(k, v)| {
                    (
                        k,
                        v.as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_u64().unwrap())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default()
}

fn mock_expectation(test_spec: &Value) -> HashMap<&String, i64> {
    test_spec
        .get("expectation")
        .and_then(|v| v.get("mock"))
        .and_then(|v| v.as_object())
        .map(|v| {
            v.iter()
                .map(|(k, v)| (k, v.as_i64().unwrap()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default()
}

fn filter_metrics(metrics: &str, filter: &str) -> String {
    metrics
        .lines()
        .filter(|m| m.contains(filter))
        .fold("".to_string(), |mut p, c| {
            p.push('\n');
            p.push_str(c);
            p
        })
}

fn test_duration(test_spec: &Value) -> u64 {
    test_spec
        .get("request")
        .and_then(|v| v.get("duration"))
        .and_then(|v| v.as_u64())
        .unwrap()
}

fn qps_expectation(test_spec: &Value) -> Vec<u64> {
    test_spec
        .get("expectation")
        .and_then(|v| v.get("qps"))
        .and_then(|v| v.as_array())
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect::<Vec<_>>()
}

fn connection_success_expectation(test_spec: &Value) -> Vec<u64> {
    let default = vec![];
    test_spec
        .get("expectation")
        .and_then(|v| v.get("connectionSuccess"))
        .and_then(|v| v.as_array())
        .unwrap_or(&default)
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect::<Vec<_>>()
}

fn connection_dropped_expectation(test_spec: &Value) -> Vec<u64> {
    let default = vec![];
    test_spec
        .get("expectation")
        .and_then(|v| v.get("connectionDropped"))
        .and_then(|v| v.as_array())
        .unwrap_or(&default)
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect::<Vec<_>>()
}

async fn send_test_req(env: &TestEnv, path: &str) -> (Value, String) {
    let path = resource_dir().join(path);
    let test_spec = serde_json::from_reader::<_, Value>(File::open(path).unwrap()).unwrap();

    let request = test_spec.get("request").unwrap();

    let job_id = send_test_req_with_json(env, request.clone()).await;
    (test_spec, job_id)
}

fn set_target(env: &TestEnv, request: &mut Value) {
    set_target_to(request, "127.0.0.1".to_string(), env.target_port());
}

fn set_target_to(request: &mut Value, host: String, port: u16) {
    let target = request.get_mut("target").unwrap();
    let host_val = target.get_mut("host").unwrap();
    *host_val = Value::String(host);
    let port_val = target.get_mut("port").unwrap();
    *port_val = Value::Number(port.into());
}

async fn send_test_req_with_json(env: &TestEnv, request: Value) -> String {
    send_test_req_with_json_target(env, request, "127.0.0.1".to_string(), env.target_port()).await
}

async fn send_test_req_with_json_target(
    env: &TestEnv,
    mut request: Value,
    host: String,
    port: u16,
) -> String {
    set_target_to(&mut request, host, port);
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{}{}", env.api_address(), TEST_PATH))
        .json(&request)
        .send()
        .await
        .unwrap();
    let resp = res.text().await.unwrap();
    info!("text resp: {}", &resp);

    let test_resp: Value = serde_json::from_str(&resp).unwrap();
    let job_id = test_resp.get("job_id").and_then(|v| v.as_str()).unwrap();
    job_id.to_string()
}

fn cumulative_sum<T: Add + Copy + Add<Output = T>>(vec: &[T], till: usize, default: T) -> T {
    vec[0..till].iter().fold(default, |b, x| b + *x)
}

fn assert_metrics_is_in_range(metrics: &str, range: Range<u64>) {
    let metrics_val = get_value_for_metrics(metrics);
    let metrics = metrics.trim();
    info!(
        "metrics: {metrics}, expectation range: {:?}, metrics_val: {metrics_val}",
        &range
    );
    if metrics_val == 0 {
        assert_eq!(0, range.start);
    } else {
        assert!(
            range.contains(&(metrics_val as u64)),
            "range assertion failed. {metrics}: expectation range: {:?}, metrics_val: {metrics_val}",
            range
        );
    }
}

async fn get_all_metrics(env: &TestEnv) -> String {
    let client = reqwest::Client::new();
    let mut all = String::new();
    for addr in env.metrics_addresses() {
        let url = Url::from_str(addr).unwrap().join(METRICS_PATH).unwrap();
        if let Ok(resp) = client.get(url).send().await
            && let Ok(text) = resp.text().await
        {
            all.push_str(&text);
            all.push('\n');
        }
    }
    all
}

pub fn get_value_for_metrics(metrics: &str) -> i64 {
    metrics
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                None
            } else {
                trimmed
                    .rsplit_once(' ')
                    .and_then(|(_, count)| count.parse::<i64>().ok())
            }
        })
        .sum()
}
