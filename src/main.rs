use std::convert::Infallible;
use std::env;

use hyper::header::CONTENT_TYPE;
use hyper::{Body, Response};
use lazy_static::lazy_static;
use prometheus::{opts, register_counter, Counter, Encoder, TextEncoder};
use warp::{Filter, Reply};

use overload::http::handle_history_all;
use overload::http::request::{PagerOptions, Request};

lazy_static! {
    static ref HTTP_COUNTER: Counter = register_counter!(opts!(
        "overload_requests_total",
        "Total number of HTTP requests made."
    ))
    .unwrap();
}

#[tokio::main]
async fn main() {
    let log_level = env::var("log.level")
        .ok()
        .unwrap_or_else(|| "info".to_string());

    tracing_subscriber::fmt()
        .with_env_filter(log_level)
        .try_init()
        .unwrap();

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
        .and(warp::body::content_length_limit(1024 * 20))
        .and(warp::body::json())
        .and_then(|request: Request| async move {
            HTTP_COUNTER.inc();
            execute(request).await
        });

    let stop_req = warp::path!("test" / "stop" / String)
        .and_then(|job_id: String| async move { stop(job_id).await });

    let history = warp::path!("test" / "status")
        .and(warp::query::<PagerOptions>())
        .and_then(|pager_option: PagerOptions| async move {
            all_job(pager_option).await
            // ()
        });

    warp::serve(prometheus_metric.or(overload_req).or(stop_req).or(history))
        .run(([0, 0, 0, 0], 3030))
        .await;
}

async fn all_job(option: PagerOptions) -> Result<impl Reply, Infallible> {
    let status = handle_history_all(option.offset.unwrap_or(0), option.limit.unwrap_or(20)).await;
    Ok(warp::reply::json(&status))
}

// async fn execute<R, Q>(request: OverloadRequest<R, Q>) -> Result<impl Reply, Infallible>
//     where
//         R: ReqSpec + Iterator<Item=HttpReq> + Serialize + Clone,
//         Q: QPSSpec + Iterator<Item=i32> + Serialize + Copy,
// {
//     let json = warp::reply::json(&request);
//     overload::executor::execute(request).await;
//     Ok(json)
// }

async fn execute(request: Request) -> Result<impl Reply, Infallible> {
    let response = overload::http::handle_request(request).await;
    let json = warp::reply::json(&response);
    Ok(json)
}

async fn stop(job_id: String) -> Result<impl Reply, Infallible> {
    let resp = overload::http::stop(job_id).await;
    let json = warp::reply::json(&resp);
    Ok(json)
}

#[cfg(test)]
mod test {
    #![allow(unused_imports)]

    use futures_util::future::{self, ok};
    use k8s_openapi::api::core::v1::ReadNamespacedEndpointsOptional;
    use k8s_openapi::ListOptional;
    use kube::{Client, Config};
    use tdigest::TDigest;
    use tokio_test::{assert_ready, assert_ready_ok, task};
    use url::Url;

    #[test]
    #[ignore]
    fn digest_test() {
        let t = TDigest::new_with_size(100);
        let values: Vec<f64> = (1..=10).map(f64::from).collect();

        let t = t.merge_unsorted(values);

        let ans = t.estimate_quantile(0.99);
        println!("{}", ans);
        let expected: f64 = 990_000.0;
        let percentage: f64 = (expected - ans).abs() / expected;
        assert!(percentage < 0.01);
    }

    #[test]
    #[ignore]
    fn merge_digest_test() {
        let v1 = vec![37.0, 57.0, 73.0, 90.0, 99.0];
        let v2 = vec![28.0, 34.0, 74.0, 97.0, 106.0];
        let v3 = vec![15.0, 34.0, 37.0, 39.0, 86.0];
        let v6 = vec![8.0, 32.0, 70.0, 86.0, 93.0];
        let v4 = vec![39.0, 69.0, 82.0, 94.0, 100.0];
        let v5 = vec![71.0, 80.0, 91.0, 98.0, 100.0];
        let v10 = vec![62.0, 79.0, 88.0, 88.0, 94.0];
        let v7 = vec![6.0, 13.0, 18.0, 47.0, 98.0];
        let v8 = vec![8.0, 29.0, 50.0, 66.0, 90.0];
        let v9 = vec![47.0, 58.0, 66.0, 71.0, 93.0];
        let digest = TDigest::new_with_size(100)
            .merge_unsorted(v1)
            .merge_unsorted(v2)
            .merge_unsorted(v3)
            .merge_unsorted(v4)
            .merge_unsorted(v5)
            .merge_unsorted(v6)
            .merge_unsorted(v7)
            .merge_unsorted(v8)
            .merge_unsorted(v9)
            .merge_unsorted(v10);
        assert_eq!(digest.estimate_quantile(0.99), 94.0)
    }
}
