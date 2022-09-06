//noinspection Duplicates
#![allow(dead_code)]

use super::HttpReq;
use crate::executor::{ConnectionCount, QPS, set_job_status, should_stop};
use crate::generator::{request_generator_stream, ArraySpec, RequestGenerator, Target};
use crate::http_util::request::{ConcurrentConnectionRateSpec, RateSpec, RequestSpecEnum};
use crate::{ErrorCode, JobStatus};
use cluster_mode::{Cluster, RestClusterNode};
use futures_util::stream::FuturesUnordered;
use http::Request;
use hyper::client::{Client, HttpConnector};
use log::{error, info, trace};
use smallvec::SmallVec;
use std::collections::HashSet;
use std::convert::TryInto;
use std::sync::Arc;
use tokio_stream::StreamExt;


/// Run tests in cluster mode.
/// Requires `buckets` specification as it needs to be forwarded to
/// secondaries
//noinspection Duplicates
pub async fn cluster_execute_request_generator(
    request: RequestGenerator,
    job_id: String,
    cluster: Arc<Cluster>,
    buckets: SmallVec<[f64; 6]>,
) {
    let time_scale = request.time_scale() as usize;
    let shared_request_provider = request.shared_provider();
    let provider = request.request_provider();
    let provider: Option<RequestSpecEnum> = provider.try_into().ok();
    let target = request.target.clone();
    let stream = request_generator_stream(request);
    tokio::pin!(stream);
    let client = Client::new();
    {
        let mut write_guard = super::JOB_STATUS.write().await;
        write_guard.insert(job_id.clone(), JobStatus::InProgress);
    }
    // in cluster mode, bundle qps for a span of time, let's say 10 seconds, and distribute them to
    // secondaries
    let bundle_size: usize = 10 * time_scale; //10 seconds
    if cluster.is_primary().await {
        loop {
            //should we stop?
            if should_stop(&job_id).await {
                break;
            }
            let mut bundle: Vec<(QPS, Vec<HttpReq>, ConnectionCount)> = vec![];
            while let Some((qps, requests, connection_count)) = stream.next().await {
                match requests {
                    Ok(result) => {
                        bundle.push((qps, result, connection_count));
                        if bundle.len() == bundle_size {
                            break;
                        }
                    }
                    Err(err) => {
                        if let Some(sqlx::Error::Database(_)) = err.downcast_ref::<sqlx::Error>() {
                            error!(
                                "sqlx::Error::Database: {}. Unrecoverable error; stopping the job.",
                                &err.to_string()
                            );
                            // no need to exit the parent `loop` due to the error, `if bundle_len == 0 { break;}`
                            // will do the work and terminate executor
                            set_job_status(&job_id, JobStatus::Error(ErrorCode::SqliteOpenFailed))
                                .await;
                            break;
                        }
                    }
                };
            }
            let bundle_len = bundle.len();
            if bundle_len == 0 {
                trace!("bundle length 0, no more requests. exiting executor");
                break;
            }
            //if it's primary, cluster.secondaries() will always return some
            if let Some(secondaries) = cluster.secondaries().await {
                let secondary_count = secondaries.len();
                if secondary_count == 0 {
                    error!("No secondary. Stopping the job");
                    let mut guard = super::JOB_STATUS.write().await;
                    guard.insert(job_id.clone(), JobStatus::Stopped);
                    continue;
                }
                let (
                    qps,
                    reminder,
                    mut requests,
                    connection_count_to_secondaries,
                    connection_count_reminder,
                ) = calculate_req_per_secondary(&bundle, bundle_size, secondary_count);
                let mut to_secondaries = FuturesUnordered::new();
                let requests = requests.drain().cloned().collect::<Vec<HttpReq>>();
                // let requests = requests.
                for secondary in secondaries.iter() {
                    let request_enum: RequestSpecEnum = if shared_request_provider {
                        provider.clone().unwrap_or_else(|| requests.clone().into())
                    } else {
                        requests.clone().into()
                    };
                    let to_secondary = send_requests_to_secondary(
                        &client,
                        secondary,
                        &qps,
                        request_enum,
                        &connection_count_to_secondaries,
                        &target,
                        job_id.clone(),
                        &buckets,
                    );
                    to_secondaries.push(to_secondary);
                }
                let has_reminder = reminder.iter().sum::<u32>() != 0;
                if has_reminder {
                    if let Some(secondary) = secondaries.iter().next() {
                        trace!("sending reminders to secondary: {:?}", &secondary);
                        let request_enum: RequestSpecEnum = if shared_request_provider {
                            provider.clone().unwrap_or_else(|| requests.clone().into())
                        } else {
                            requests.clone().into()
                        };
                        let to_secondary = send_requests_to_secondary(
                            &client,
                            secondary,
                            &reminder,
                            request_enum,
                            &connection_count_reminder,
                            &target,
                            job_id.clone(),
                            &buckets,
                        );
                        to_secondaries.push(to_secondary);
                    }
                }
                while let Some(result) = to_secondaries.next().await {
                    crate::log_error!(result);
                }
            }

            if bundle_len < bundle_size {
                // that means while let.. got None before populating all the items,
                // i.e. no more item in the stream
                break;
            }
        }
        info!("primary - finished generating QPS");
        set_job_status(&job_id, JobStatus::Completed).await;
    } else {
        error!("Node is in secondary mode.");
        set_job_status(&job_id, JobStatus::Error(ErrorCode::SecondaryClusterNode)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_requests_to_secondary(
    client: &Client<HttpConnector>,
    node: &RestClusterNode,
    qps: &[u32],
    requests: RequestSpecEnum,
    connection_count_to_secondaries: &[u32],
    target: &Target,
    job_id: String,
    buckets: &SmallVec<[f64; 6]>,
) -> anyhow::Result<()> {
    let instance = node.service_instance();
    let uri = instance.uri().clone().expect("No uri");
    let request = to_test_request(
        qps,
        requests,
        connection_count_to_secondaries,
        target,
        job_id,
        buckets,
    );
    let request = serde_json::to_vec(&request)?;
    let request = Request::builder()
        .uri(format!("{}{}", uri, PATH_REQ_TO_SECONDARY))
        .method("POST")
        .body(request.into())?;
    client.request(request).await?;
    Ok(())
}

fn to_test_request(
    qps: &[u32],
    req: RequestSpecEnum,
    connection_count: &[u32],
    target: &Target,
    job_id: String,
    buckets: &SmallVec<[f64; 6]>,
) -> crate::http_util::request::Request {
    let duration = qps.len() as u32;
    let qps = ArraySpec::new(Vec::from(qps));
    let qps = RateSpec::ArraySpec(qps);
    let concurrent_connection = ArraySpec::new(Vec::from(connection_count));
    let concurrent_connection = Some(ConcurrentConnectionRateSpec::ArraySpec(
        concurrent_connection,
    ));

    crate::http_util::request::Request {
        name: Some(job_id),
        duration,
        req,
        qps,
        target: target.clone(),
        concurrent_connection,
        histogram_buckets: buckets.clone(),
    }
}

const PATH_REQ_TO_SECONDARY: &str = "/cluster/test";

#[allow(clippy::type_complexity)]
fn calculate_req_per_secondary(
    requests: &[(u32, Vec<HttpReq>, u32)],
    _bundle_size: usize,
    n_secondary: usize,
) -> (Vec<u32>, Vec<u32>, HashSet<&HttpReq>, Vec<u32>, Vec<u32>) {
    //array of qps, will be used to generate ArrayQPS
    let mut qps_to_secondaries = vec![];
    let mut qps_reminders = vec![];
    let mut req_to_secondaries = HashSet::new();
    // what connection rate each secondaries should use to send connection to upstream
    let mut connection_count_to_secondaries = vec![];
    let mut connection_count_reminder = vec![];

    for (qps, requests, connection_count) in requests {
        let q = qps / n_secondary as u32;
        let r = qps % n_secondary as u32;
        qps_to_secondaries.push(q);
        qps_reminders.push(r);
        req_to_secondaries.extend(requests);
        let c = connection_count / n_secondary as u32;
        let r = connection_count % n_secondary as u32;
        connection_count_to_secondaries.push(c);
        connection_count_reminder.push(r);
    }
    (
        qps_to_secondaries,
        qps_reminders,
        req_to_secondaries,
        connection_count_to_secondaries,
        connection_count_reminder,
    )
}

#[cfg(test)]
mod test {
    use crate::executor::cluster::{
        calculate_req_per_secondary, cluster_execute_request_generator,
    };
    use crate::generator::test::req_list_with_n_req;
    use crate::generator::{ConstantRate, RequestGenerator, Scheme, Target};
    use crate::HttpReq;
    use cluster_mode::{Cluster, InstanceMode, RestClusterNode};
    use rust_cloud_discovery::ServiceInstance;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_cluster_execute_request_generator() {
        let mut requests = vec![];
        for _ in 0..3 {
            requests.push(http_req_random());
        }
        let qps = ConstantRate { count_per_sec: 3 };
        let rg = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(3)),
            Box::new(qps),
            Target {
                host: "example.com".to_string(),
                port: 80,
                protocol: Scheme::HTTP,
            },
            None,
        );
        let job_id = "test_job".to_string();
        let instance = ServiceInstance::new(
            None,
            None,
            None,
            None,
            false,
            Some("http://httpbin.org/anything".to_string()),
            HashMap::new(),
            None,
        );
        let mut secondaries = HashSet::new();
        for _ in 0..3 {
            let node = RestClusterNode::new(Uuid::new_v4().to_string(), instance.clone());
            secondaries.insert(node);
        }
        let cluster = Arc::new(Cluster::_new(InstanceMode::Primary, secondaries));
        cluster_execute_request_generator(
            rg,
            job_id,
            cluster,
            crate::metrics::default_histogram_bucket(),
        )
        .await;
    }

    #[test]
    #[allow(clippy::vec_init_then_push)]
    fn test_calculate_req_per_secondary() {
        let mut requests = vec![];
        for _ in 0..3 {
            requests.push(http_req_random());
        }
        let mut qps_req = vec![];
        qps_req.push((4, requests.clone(), 4));
        qps_req.push((5, requests.clone(), 5));
        qps_req.push((6, requests.clone(), 6));

        let result = calculate_req_per_secondary(&qps_req, 3, 3);
        assert_eq!(result.0, vec![1, 1, 2]);
        assert_eq!(result.1, vec![1, 2, 0]);
        assert_eq!(result.3, vec![1, 1, 2]);
        assert_eq!(result.4, vec![1, 2, 0]);
    }

    fn http_req_random() -> HttpReq {
        let r = rand::random::<u8>();
        let uri = format!("http://httpbin.org/anything/{}", r);
        HttpReq::new(uri)
    }
}
