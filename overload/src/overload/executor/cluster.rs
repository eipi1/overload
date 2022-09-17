//noinspection Duplicates

use super::HttpReq;
use crate::executor::{set_job_status, should_stop, ConnectionCount, QPS};
use crate::generator::{request_generator_stream, ArraySpec, RequestGenerator, Target};
use crate::http_util::request::{ConcurrentConnectionRateSpec, RateSpec, RequestSpecEnum};
use crate::{ErrorCode, JobStatus};
use cluster_mode::{Cluster, RestClusterNode};
use futures_util::stream::FuturesUnordered;
use http::Request;
use hyper::client::{Client, HttpConnector};
use log::{debug, error, info, trace};
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
    // in cluster mode, bundle qps for a period, let's say 10 seconds, and distribute them to
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
                        debug!("[cluster_execute_request_generator] - [{}] - qps: {}, request len: {}, connection count: {}", &job_id, qps, result.len(), connection_count);
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
                        error!(
                            "[cluster_execute_request_generator] - [{}] - error: {}",
                            &job_id, err
                        );
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
                let (mut qps, mut requests, mut connection_count_to_secondaries) =
                    calculate_req_per_secondary(&bundle, bundle_size, secondary_count);
                let mut to_secondaries = FuturesUnordered::new();
                let requests = requests.drain().cloned().collect::<Vec<HttpReq>>();

                for secondary in secondaries.iter() {
                    let request_enum: RequestSpecEnum = if shared_request_provider {
                        provider.clone().unwrap_or_else(|| requests.clone().into())
                    } else {
                        requests.clone().into()
                    };
                    let to_secondary = send_requests_to_secondary(
                        &client,
                        secondary,
                        qps.pop().unwrap(),
                        request_enum,
                        connection_count_to_secondaries.pop().unwrap(),
                        &target,
                        job_id.clone(),
                        &buckets,
                    );
                    to_secondaries.push(to_secondary);
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
    qps: Vec<QPS>,
    requests: RequestSpecEnum,
    connection_count_to_secondaries: Vec<QPS>,
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
    qps: Vec<QPS>,
    req: RequestSpecEnum,
    connection_count: Vec<QPS>,
    target: &Target,
    job_id: String,
    buckets: &SmallVec<[f64; 6]>,
) -> crate::http_util::request::Request {
    let duration = qps.len() as u32;
    let qps = ArraySpec::new(qps);
    let qps = RateSpec::ArraySpec(qps);
    let concurrent_connection = ArraySpec::new(connection_count);
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

fn calculate_req_per_secondary(
    requests: &[(QPS, Vec<HttpReq>, ConnectionCount)],
    bundle_size: usize,
    n_secondary: usize,
) -> (Vec<Vec<QPS>>, HashSet<&HttpReq>, Vec<Vec<QPS>>) {
    //array of qps, will be used to generate ArrayQPS
    let mut qps_per_secondary = vec![];
    let mut qps_remainders = vec![];
    let mut req_to_secondaries = HashSet::new();
    let mut connection_count_per_secondary = vec![];
    let mut connection_count_remainder = vec![];

    for (qps, requests, connection_count) in requests {
        debug!(
            "[calculate_req_per_secondary] - qps: {}, requests len: {}, connection count: {}",
            qps,
            requests.len(),
            connection_count
        );
        let q = qps / n_secondary as QPS;
        let r = qps % n_secondary as QPS;
        qps_per_secondary.push(q);
        qps_remainders.push(r);
        req_to_secondaries.extend(requests);
        let c = connection_count / n_secondary as u32;
        let r = connection_count % n_secondary as u32;
        connection_count_per_secondary.push(c);
        connection_count_remainder.push(r);
    }

    let mut qps = vec![vec![0; bundle_size]; n_secondary];
    let mut conn_count = vec![vec![0; bundle_size]; n_secondary];
    let iter = qps_per_secondary.iter().zip(qps_remainders.iter()).zip(
        connection_count_per_secondary
            .iter()
            .zip(connection_count_remainder.iter()),
    );

    for (p, ((qps_, qps_remainder), (conn_count_, conn_remainder))) in iter.enumerate() {
        for i in 0..n_secondary {
            let q = qps.get_mut(i).unwrap();
            let qps_for_this_sec = if i < *qps_remainder as usize {
                *qps_ + 1
            } else {
                *qps_
            };
            let _ = std::mem::replace(&mut q[p], qps_for_this_sec);

            let c = conn_count.get_mut(i).unwrap();
            let count_for_this_sec = if i < *conn_remainder as usize {
                *conn_count_ + 1
            } else {
                *conn_count_
            };
            let _ = std::mem::replace(&mut c[p], count_for_this_sec);
        }
    }

    (qps, req_to_secondaries, conn_count)
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
        qps_req.push((0, requests.clone(), 2));
        qps_req.push((2, requests.clone(), 0));
        qps_req.push((5, requests.clone(), 6));
        qps_req.push((6, requests.clone(), 7));
        qps_req.push((7, requests.clone(), 8));
        qps_req.push((8, requests.clone(), 5));

        let result = calculate_req_per_secondary(&qps_req, 6, 4);
        assert_eq!(result.0.get(0), Some(&vec![0, 1, 2, 2, 2, 2]));
        assert_eq!(result.0.get(1), Some(&vec![0, 1, 1, 2, 2, 2]));
        assert_eq!(result.0.get(2), Some(&vec![0, 0, 1, 1, 2, 2]));
        assert_eq!(result.0.get(3), Some(&vec![0, 0, 1, 1, 1, 2]));
        assert_eq!(result.0.get(4), None);

        assert_eq!(result.2.get(0), Some(&vec![1, 0, 2, 2, 2, 2]));
        assert_eq!(result.2.get(1), Some(&vec![1, 0, 2, 2, 2, 1]));
        assert_eq!(result.2.get(2), Some(&vec![0, 0, 1, 2, 2, 1]));
        assert_eq!(result.2.get(3), Some(&vec![0, 0, 1, 1, 2, 1]));
        assert_eq!(result.2.get(4), None);
    }

    fn http_req_random() -> HttpReq {
        let r = rand::random::<u8>();
        let uri = format!("http://httpbin.org/anything/{}", r);
        HttpReq::new(uri)
    }
}
