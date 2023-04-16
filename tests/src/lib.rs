#[cfg(test)]
mod tests {
    use env_logger::Env;
    use httpmock::Method::GET;
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
    use tokio::sync::OnceCell;
    use tokio::time::sleep;

    pub static TEST_PATH: &str = "/test";
    pub const FILE_UPLOAD_PATH: &str = "/test/requests-bin";
    pub static METRICS_PATH: &str = "/metrics";

    static ONCE: Once = Once::new();
    fn init_logger() {
        ONCE.call_once(|| {
            env_logger::Builder::from_env(Env::default().default_filter_or("info"))
                .format_timestamp_millis()
                .init();
        });
    }

    pub fn address() -> &'static str {
        if !cfg!(feature = "cluster") {
            "http://localhost:3030"
        } else {
            "http://10.152.183.175:3030"
        }
    }
    fn target_host() -> &'static str {
        if !cfg!(feature = "cluster") {
            "127.0.0.1"
        } else {
            "172.17.0.1"
        }
    }

    fn target_port() -> u16 {
        2080
    }

    fn cluster_mode() -> bool {
        cfg!(feature = "cluster")
    }

    fn instance_count() -> usize {
        if !cfg!(feature = "cluster") {
            1
        } else {
            3
        }
    }

    pub fn resource_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/resources")
    }

    #[cfg(not(feature = "cluster"))]
    #[tokio::test]
    #[ignore]
    async fn test_scenarios_1() {
        let path = "test-assertion.json";
        init_logger();

        let (test_spec, job_id) = send_test_req(path).await;

        let duration = test_duration(&test_spec);
        let qps_expectation = qps_expectation(&test_spec);
        sleep(Duration::from_secs(1)).await;
        for i in 1..duration - 1 {
            assert_request_count(i as usize, &job_id, &qps_expectation).await;
            sleep(Duration::from_secs(1)).await;
        }
    }

    #[cfg(not(feature = "cluster"))]
    #[rstest]
    #[case("test-assertion-fail-2.json")]
    #[case("test-assertion-fail.json")]
    #[case("test-generator-with-assertion.json")]
    #[case("test-array-generator-with-assertion.json")]
    #[case("test-generator-url-param-with-assertion.json")]
    #[case("test-connection-keep-alive-default.json")]
    #[case("test-connection-keep-alive.json")]
    #[case("test-connection-keep-alive-ttl.json")]
    #[case("test-connection-keep-alive-ttl-2.json")]
    #[case("test-connection-keep-alive-max-req.json")]
    #[tokio::test]
    #[ignore]
    async fn test_scenarios_assertion(#[case] path: &str) {
        // let path = "test-generator-with-assertion.json";
        init_logger();

        let (test_spec, job_id) = send_test_req(path).await;

        let duration = test_duration(&test_spec) as usize;
        let qps_expectation = qps_expectation(&test_spec);
        let failure_expectation = assert_failure_expectation(&test_spec);
        let conn_success_expectation = connection_success_expectation(&test_spec);
        let conn_dropped_expectation = connection_dropped_expectation(&test_spec);

        sleep(Duration::from_secs(1)).await;
        for i in 1..duration - 1 {
            assert_request_count(i, &job_id, &qps_expectation).await;
            assert_assertion_failure_count(i, &job_id, &failure_expectation).await;
            assert_connection_success_count(i, &job_id, &conn_success_expectation).await;
            assert_connection_dropped_count(i, &job_id, &conn_dropped_expectation).await;
            sleep(Duration::from_secs(1)).await;
        }
    }

    #[cfg(not(feature = "cluster"))]
    #[tokio::test]
    #[ignore]
    async fn test_scenarios_file_data() {
        let path = "test-with-file-data.json";
        init_logger();

        let path = resource_dir().join(path);
        let mut test_spec = serde_json::from_reader::<_, Value>(File::open(path).unwrap()).unwrap();

        let file_name = test_spec.get("dataFileName").unwrap().as_str().unwrap();
        let file_id = upload_file(file_name).await;

        let request = test_spec.get_mut("request").unwrap();

        let _ = request
            .get_mut("req")
            .and_then(|v| v.get_mut("RequestFile"))
            .and_then(|v| v.get_mut("file_name"))
            .map(|v| *v = Value::String(file_id));

        let job_id = send_test_req_with_json(address(), request.clone()).await;

        let duration = test_duration(&test_spec) as usize;
        let qps_expectation = qps_expectation(&test_spec);
        // let failure_expectation = assert_failure_expectation(&test_spec);
        //
        sleep(Duration::from_secs(1)).await;
        for i in 1..duration - 1 {
            assert_request_count(i, &job_id, &qps_expectation).await;
            // assert_assertion_failure_count(i,&job_id, &failure_expectation).await;
            sleep(Duration::from_secs(1)).await;
        }
    }

    #[rstest]
    #[case("error-lua-parse-failure.json")]
    #[case("error-no-file.json")]
    #[case("error-wrong-constraint.json")]
    #[tokio::test]
    #[ignore]
    async fn test_error_requests(#[case] path: &str) {
        // let path = "error-wrong-constraint.json";
        init_logger();

        let address = address();
        let resource_dir = resource_dir();
        println!("{:?},{:?},{:?}", path, address, resource_dir);
        let path = resource_dir.join(path);
        let mut test_spec = serde_json::from_reader::<_, Value>(File::open(path).unwrap()).unwrap();

        let request = test_spec.get_mut("request").unwrap();

        let client = reqwest::Client::new();
        set_target(request);
        let resp = client
            .post(format!("{}{}", address, TEST_PATH))
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
    // #[case("test-with-split-file-data-4.json")]
    #[tokio::test]
    #[ignore]
    async fn scenarios_with_file_and_mock_verification(#[case] path: &str) {
        // let path = "test-with-split-file-data-3.json";
        init_logger();

        let (mock_server, url) = ASYNC_ONCE_HTTP_MOCK.get_or_init(init_http_mock).await;
        println!("url - {:?}", &url);

        let mut mocks = Vec::with_capacity(20);

        for i in 0..20 {
            let mock = mock_server.mock(|when, then| {
                when.method(GET).path(format!("/{path}/file/{i}"));
                then.status(200)
                    .header("content-type", "application/json")
                    .header("Connection", "keep-alive")
                    .body(r#"{"hello": "world"}"#);
            });
            mocks.push(mock);
        }

        let path = resource_dir().join(path);
        let mut test_spec = serde_json::from_reader::<_, Value>(File::open(path).unwrap()).unwrap();

        let file_name = test_spec.get("dataFileName").unwrap().as_str().unwrap();
        let file_id = upload_file(file_name).await;

        let request = test_spec.get_mut("request").unwrap();

        let _ = request
            .get_mut("req")
            .and_then(|v| {
                let file = v.get("SplitRequestFile");
                if file.is_some() {
                    v.get_mut("SplitRequestFile")
                } else {
                    // drop(file);
                    v.get_mut("RequestFile")
                }
            })
            .and_then(|v| v.get_mut("file_name"))
            .map(|v| *v = Value::String(file_id));

        let job_id = send_test_req_with_json_target(
            address(),
            request.clone(),
            target_host().to_string(),
            url.port().unwrap(),
        )
        .await;

        let duration = test_duration(&test_spec) as usize;
        let qps_expectation = qps_expectation(&test_spec);
        let failure_expectation = assert_failure_expectation(&test_spec);
        sleep(Duration::from_secs(1)).await;
        for i in 1..duration - 1 {
            if !cluster_mode() {
                assert_request_count(i, &job_id, &qps_expectation).await;
            }
            assert_assertion_failure_count(i, &job_id, &failure_expectation).await;
            sleep(Duration::from_secs(1)).await;
        }

        sleep(Duration::from_secs(1_u64)).await;
        let mock_expectation = mock_expectation(&test_spec);
        for (id, expectation) in mock_expectation {
            let hit = mocks
                .get(id.parse::<usize>().unwrap())
                .unwrap()
                .hits_async()
                .await;
            assert_eq!(
                hit, expectation as usize,
                "assertion failure - mock:{id}, hit:{hit}, expectation: {expectation}"
            );
        }
    }

    #[cfg(feature = "cluster")]
    pub async fn init_http_mock() -> (httpmock::MockServer, url::Url) {
        let mock_server = httpmock::MockServer::connect_async("localhost:3080").await;
        let url = url::Url::parse(&mock_server.base_url()).unwrap();
        (mock_server, url)
    }

    #[cfg(not(feature = "cluster"))]
    pub async fn init_http_mock() -> (httpmock::MockServer, url::Url) {
        let mock_server = httpmock::MockServer::start_async().await;
        let url = url::Url::parse(&mock_server.base_url()).unwrap();
        (mock_server, url)
    }

    pub static ASYNC_ONCE_HTTP_MOCK: OnceCell<(httpmock::MockServer, url::Url)> =
        OnceCell::const_new();

    async fn upload_file(file_name: &str) -> String {
        let address = address();
        let file = tokio::fs::File::open(resource_dir().join(file_name))
            .await
            .unwrap();

        let client = reqwest::Client::new();
        let res = client
            .post(
                Url::from_str(address)
                    .unwrap()
                    .join(FILE_UPLOAD_PATH)
                    .unwrap(),
            )
            .header("content-length", file.metadata().await.unwrap().len())
            // .body(file_to_body(file))
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

    async fn assert_request_count(i: usize, job_id: &str, qps_expectation: &[u64]) {
        let metrics = get_all_metrics().await;
        let metrics = filter_metrics(metrics, "upstream_request_count");
        let metrics = filter_metrics(metrics, job_id);
        println!("{}", &metrics);
        assert_metrics_is_in_range(
            &metrics,
            (cumulative_sum::<u64>(qps_expectation, i, 0))
                ..(cumulative_sum::<u64>(qps_expectation, i + 2, 0)),
        );
    }

    #[allow(dead_code)]
    async fn assert_connection_success_count(i: usize, job_id: &str, expectation: &[u64]) {
        if expectation.is_empty() {
            return;
        }
        let metrics = get_all_metrics().await;
        let metrics = filter_metrics(metrics, "connection_pool_new_connection_success");
        let metrics = filter_metrics(metrics, job_id);
        println!("{}", &metrics);
        assert_metrics_is_in_range(
            &metrics,
            (cumulative_sum::<u64>(expectation, i, 0))
                ..(cumulative_sum::<u64>(expectation, i + 2, 0) + 1),
        );
    }

    #[allow(dead_code)]
    async fn assert_connection_dropped_count(i: usize, job_id: &str, expectation: &[u64]) {
        if expectation.is_empty() {
            return;
        }
        let metrics = get_all_metrics().await;
        let metrics = filter_metrics(metrics, "connection_pool_connection_dropped");
        let metrics = filter_metrics(metrics, job_id);
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
    ) {
        let metrics = get_all_metrics().await;
        info!("{}", &metrics);
        let metrics = filter_metrics(metrics, "assertion_failure");
        let metrics = filter_metrics(metrics, job_id);
        println!("{}", &metrics);
        for (k, expectation) in failure_expectation {
            let metrics = filter_metrics(metrics.clone(), &format!("assertion_id=\"{}\"", k));
            assert_metrics_is_in_range(
                &metrics,
                (cumulative_sum::<u64>(expectation, i, 0))
                    ..(cumulative_sum::<u64>(expectation, i + 2, 0)),
            );
            let metrics = filter_metrics(metrics.clone(), &format!("assertion_id=\"{}\"", k));
            assert_metrics_is_in_range(
                &metrics,
                (cumulative_sum::<u64>(expectation, i, 0))
                    ..(cumulative_sum::<u64>(expectation, i + 2, 0)),
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

    fn filter_metrics(metrics: String, filter: &str) -> String {
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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
    async fn send_test_req(path: &str) -> (Value, String) {
        let address = address();
        let resource_dir = resource_dir();
        println!("{:?},{:?},{:?}", path, address, resource_dir);
        let path = resource_dir.join(path);
        let test_spec = serde_json::from_reader::<_, Value>(File::open(path).unwrap()).unwrap();

        let request = test_spec.get("request").unwrap();

        let job_id = send_test_req_with_json(address, request.clone()).await;
        (test_spec, job_id)
    }

    fn set_target(request: &mut Value) {
        let host = target_host().to_string();
        let port = target_port();
        set_target_to(request, host, port);
    }

    fn set_target_to(request: &mut Value, host: String, port: u16) {
        let target = request.get_mut("target").unwrap();
        let host_val = target.get_mut("host").unwrap();
        *host_val = Value::String(host);
        let port_val = target.get_mut("port").unwrap();
        *port_val = Value::Number(port.into());
    }

    #[allow(dead_code)]
    async fn send_test_req_with_json(address: &str, request: Value) -> String {
        send_test_req_with_json_target(address, request, target_host().to_string(), target_port())
            .await
    }

    async fn send_test_req_with_json_target(
        address: &str,
        mut request: Value,
        host: String,
        port: u16,
    ) -> String {
        set_target_to(&mut request, host, port);
        let client = reqwest::Client::new();
        let res = client
            .post(format!("{}{}", address, TEST_PATH))
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
        info!("expectation range: {:?}", &range);
        let metrics_val = get_value_for_metrics(metrics);
        info!("metrics_val: {metrics_val}");
        if metrics_val == 0 {
            assert_eq!(0, range.start);
        } else {
            assert!(range.contains(&((metrics_val * instance_count() as i64) as u64)));
        }
    }

    async fn get_all_metrics() -> String {
        let address = address();
        let url = Url::from_str(address).unwrap().join(METRICS_PATH).unwrap();
        let client = reqwest::Client::new();
        client.get(url).send().await.unwrap().text().await.unwrap()
    }

    pub fn get_value_for_metrics(metrics: &str) -> i64 {
        info!("getting value for metrics: {}", metrics);
        return metrics
            .rsplit_once(' ')
            .map_or(0, |(_, count)| count.parse::<i64>().unwrap());
    }
}
