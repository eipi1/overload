//noinspection Duplicates
#![allow(dead_code)]

use http::Request;
use hyper::client::{Client, HttpConnector};
use log::{error, trace};
use std::collections::HashSet;
use std::sync::Arc;
use tokio_stream::StreamExt;

use super::HttpReq;
use crate::generator::{request_generator_stream, ArrayQPS, RequestGenerator};
use crate::http_util::request::{QPSSpec, RequestSpecEnum};
use crate::JobStatus;
use cluster_mode::{Cluster, RestClusterNode};
use futures_util::stream::FuturesUnordered;

//noinspection Duplicates
pub async fn cluster_execute_request_generator(
    request: RequestGenerator,
    job_id: String,
    cluster: Arc<Cluster>,
) {
    let time_scale = request.time_scale() as usize;
    let stream = request_generator_stream(request);
    tokio::pin!(stream);
    // let client = Arc::new(Client::new());
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
            let stop = {
                let read_guard = super::JOB_STATUS.read().await;
                matches!(read_guard.get(&job_id), Some(JobStatus::Stopped))
            };
            if stop {
                break;
            }
            let mut bundle = vec![];
            while let Some((qps, requests)) = stream.next().await {
                bundle.push((qps, requests));
                if bundle.len() == bundle_size {
                    break;
                }
            }
            let bundle_len = bundle.len();
            if bundle_len == 0 {
                trace!("bundle length 0, no more requests. exiting executor");
                break;
            }
            //if it's primary, cluster.secondaries() will always return some
            if let Some(secondaries) = cluster.secondaries().await {
                let len = secondaries.len();
                if len == 0 {
                    error!("No secondary. Stopping the job");
                    let mut guard = super::JOB_STATUS.write().await;
                    guard.insert(job_id.clone(), JobStatus::Stopped);
                    continue;
                }
                let (qps, reminder, mut requests) =
                    calculate_req_per_secondary(&bundle, bundle_size, len);
                let mut to_secondaries = FuturesUnordered::new();
                let requests = requests.drain().cloned().collect::<Vec<HttpReq>>();
                // let requests = requests.
                for secondary in secondaries.iter() {
                    let to_secondary = send_requests_to_secondary(
                        &client,
                        secondary,
                        &qps,
                        requests.clone(),
                        job_id.clone(),
                    );
                    to_secondaries.push(to_secondary);
                }
                let has_reminder = reminder.iter().sum::<u32>() != 0;
                if has_reminder {
                    if let Some(secondary) = secondaries.iter().next() {
                        trace!("sending reminders to secondary: {:?}", &secondary);
                        let to_secondary = send_requests_to_secondary(
                            &client,
                            &secondary,
                            &reminder,
                            requests,
                            job_id.clone(),
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
    } else {
        error!("Node is in secondary mode.");
    }
    {
        let mut write_guard = super::JOB_STATUS.write().await;
        if let Some(status) = write_guard.get(&job_id) {
            let job_finished = matches!(
                status,
                JobStatus::Stopped | JobStatus::Failed | JobStatus::Completed
            );
            if !job_finished {
                write_guard.insert(job_id.clone(), JobStatus::Completed);
            }
        } else {
            error!("Job Status should be present")
        }
    }
}

async fn send_requests_to_secondary(
    client: &Client<HttpConnector>,
    node: &RestClusterNode,
    qps: &[u32],
    // requests: &HashSet<&HttpReq>,
    requests: Vec<HttpReq>,
    job_id: String,
) -> anyhow::Result<()> {
    let instance = node.service_instance();
    let uri = instance.uri().clone().expect("No uri");
    let request = to_test_request(qps, requests.into(), job_id);
    let request = serde_json::to_vec(&request)?;
    let request = Request::builder()
        .uri(format!("{}{}", uri, PATH_REQ_TO_SECONDARY))
        .method("POST")
        .body(request.into())?;
    client.request(request).await?;
    Ok(())
}

// fn to_test_request(qps: &[u32], requests: &HashSet<&HttpReq>) {
fn to_test_request(
    qps: &[u32],
    req: RequestSpecEnum,
    job_id: String,
) -> crate::http_util::request::Request {
    let duration = qps.len() as u32;
    let qps = ArrayQPS::new(Vec::from(qps));
    let qps = QPSSpec::ArrayQPS(qps);

    crate::http_util::request::Request {
        name: Some(job_id),
        duration,
        req,
        qps,
    }
}

const PATH_REQ_TO_SECONDARY: &str = "/cluster/test";

fn calculate_req_per_secondary(
    requests: &[(u32, Vec<HttpReq>)],
    _bundle_size: usize,
    n_secondary: usize,
) -> (Vec<u32>, Vec<u32>, HashSet<&HttpReq>) {
    //array of qps, will be used to generate ArrayQPS
    let mut qps_to_secondaries = vec![];
    let mut qps_reminders = vec![];
    let mut req_to_secondaries = HashSet::new();

    for (qps, requests) in requests {
        let q = qps / n_secondary as u32;
        let r = qps % n_secondary as u32;
        qps_to_secondaries.push(q);
        qps_reminders.push(r);
        req_to_secondaries.extend(requests);
    }
    (qps_to_secondaries, qps_reminders, req_to_secondaries)
}

#[cfg(test)]
mod test {
    use crate::executor::cluster::{
        calculate_req_per_secondary, cluster_execute_request_generator,
    };
    use crate::generator::{ConstantQPS, RequestGenerator};
    use crate::HttpReq;
    use cluster_mode::{Cluster, InstanceMode, RestClusterNode};
    use rust_cloud_discovery::ServiceInstance;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use uuid::Uuid;
    use crate::generator::test::req_list_with_n_req;

    #[tokio::test]
    async fn test_cluster_execute_request_generator() {
        let mut requests = vec![];
        for _ in 0..3 {
            requests.push(http_req_random());
        }
        let qps = ConstantQPS { qps: 3 };
        let rg = RequestGenerator::new(3, Box::new(req_list_with_n_req(3)), Box::new(qps));
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
        cluster_execute_request_generator(rg, job_id, cluster).await;
    }

    #[test]
    fn test_calculate_req_per_secondary() {
        let mut requests = vec![];
        for _ in 0..3 {
            requests.push(http_req_random());
        }
        let mut qps_req = vec![];
        qps_req.push((4, requests.clone()));
        qps_req.push((5, requests.clone()));
        qps_req.push((6, requests.clone()));

        calculate_req_per_secondary(&qps_req, 3, 3);
        //todo verify result
    }

    fn http_req_random() -> HttpReq {
        let r = rand::random::<u8>();
        let uri = format!("http://httpbin.org/anything/{}", r);
        HttpReq::new(uri)
    }
}
