pub mod request;

#[cfg(feature = "cluster")]
use crate::executor::cluster;
use crate::executor::{execute_request_generator, get_job_status, send_stop_signal};
use crate::http_util::request::Request;
#[cfg(feature = "cluster")]
use crate::ErrorCode;
use crate::{JobStatus, Response};
#[cfg(feature = "cluster")]
use cluster_mode::Cluster;
#[cfg(feature = "cluster")]
use hyper::client::HttpConnector;
#[cfg(feature = "cluster")]
use hyper::{Body, Client};
#[cfg(feature = "cluster")]
use log::{error, trace};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
#[cfg(feature = "cluster")]
use std::sync::Arc;
use uuid::Uuid;

pub async fn handle_request(request: Request) -> Response {
    let job_id = job_id(&request);
    let generator = request.into();

    tokio::spawn(execute_request_generator(generator, job_id.clone()));
    Response::new(job_id, JobStatus::Starting)
}

#[cfg(feature = "cluster")]
pub async fn handle_request_cluster(request: Request, cluster: Arc<Cluster>) -> Response {
    let job_id = job_id(&request);
    if !cluster.is_active().await {
        Response::new(job_id, JobStatus::Error(ErrorCode::InactiveCluster))
    } else if cluster.is_primary().await {
        let generator = request.into();
        tokio::spawn(cluster::cluster_execute_request_generator(
            generator,
            job_id.clone(),
            cluster,
        ));
        Response::new(job_id, JobStatus::Starting)
    } else {
        //forward request to primary
        let client = Client::new();
        let job_id = request
            .name
            .clone()
            .unwrap_or_else(|| "unknown_job".to_string());
        match forward_test_request(request, cluster, client).await {
            Ok(resp) => resp,
            Err(err) => {
                error!("{}", err);
                unknown_error_resp(job_id)
            }
        }
    }
}
#[cfg(feature = "cluster")]
async fn forward_test_request(
    request: Request,
    cluster: Arc<Cluster>,
    client: Client<HttpConnector>,
) -> anyhow::Result<Response> {
    let primaries = cluster
        .primaries()
        .await
        .ok_or_else(|| anyhow::anyhow!("Primary returns None"))?;
    if let Some(primary) = primaries.iter().next() {
        let uri = primary
            .service_instance()
            .uri()
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Invalid ServiceInstance URI"))?;
        trace!("forwarding request to primary: {}", &uri);
        let req = hyper::Request::builder()
            .uri(format!("{}/test", &uri))
            .method("POST")
            .body(Body::from(serde_json::to_string(&request)?))?;
        let resp = client.request(req).await?;
        let bytes = hyper::body::to_bytes(resp.into_body()).await?;
        let resp = serde_json::from_slice::<Response>(bytes.as_ref())?;
        return Ok(resp);
    };
    Ok(unknown_error_resp(
        request.name.unwrap_or_else(|| "unknown_job".to_string()),
    ))
}

#[cfg(feature = "cluster")]
fn unknown_error_resp(job_id: String) -> Response {
    Response::new(job_id, JobStatus::Error(ErrorCode::Others))
}

//todo verify for cluster mode. using job id as name for secondary request
fn job_id(request: &Request) -> String {
    request
        .name
        .clone()
        .map_or(Uuid::new_v4().to_string(), |n| {
            let uuid = Regex::new(
                r"\b[0-9a-f]{8}\b-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-\b[0-9a-f]{12}\b$",
            )
            .unwrap();
            if uuid.is_match(&n) {
                n
            } else {
                let mut name = n;
                name.push('-');
                name.push_str(Uuid::new_v4().to_string().as_str());
                name
            }
        })
}

pub async fn stop(job_id: String) -> GenericResponse {
    let result = send_stop_signal(job_id.clone()).await;
    GenericResponse {
        job_id,
        message: result,
    }
}

pub async fn handle_history_all(
    offset: usize,
    limit: usize,
) -> HashMap<String, JobStatus, RandomState> {
    get_job_status(offset, limit).await
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenericResponse {
    job_id: String,
    message: String,
}
