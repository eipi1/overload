pub mod request;

#[cfg(feature = "cluster")]
use crate::executor::cluster;
use crate::executor::{execute_request_generator, get_job_status, send_stop_signal};
use crate::http_util::request::{Request, RequestGeneric};
#[cfg(feature = "cluster")]
use crate::ErrorCode;
use crate::{JobStatus, Response};
use bytes::Buf;
#[cfg(feature = "cluster")]
use cluster_mode::Cluster;
use futures_util::TryStreamExt;
#[cfg(feature = "cluster")]
use hyper::client::HttpConnector;
#[cfg(feature = "cluster")]
use hyper::{Body, Client};
#[allow(unused_imports)]
use log::{error, trace};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
#[cfg(feature = "cluster")]
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio_stream::StreamExt;
use uuid::Uuid;
use warp::multipart::{FormData, Part};
use std::fmt::{Display, Formatter, Debug};
use std::fmt;

pub async fn handle_request(request: Request) -> Response {
    let job_id = job_id(&request);
    // let generator = request.into();
    let request = req_to_req_gen(request);
    let generator = request.into();

    tokio::spawn(execute_request_generator(generator, job_id.clone()));
    Response::new(job_id, JobStatus::Starting)
}

fn req_to_req_gen(request: Request) -> RequestGeneric{
    todo!()
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

pub async fn stop(job_id: String) -> TestJobResponse {
    let result = send_stop_signal(job_id.clone()).await;
    TestJobResponse {
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

pub async fn upload_file(form: FormData) -> anyhow::Result<GenericResponse> {
    let parts: Vec<Part> = form.try_collect().await?;
    trace!("{:?}", &parts);
    for part in parts {
        if part.filename().is_some() {
            let content_type = part.content_type();
            trace!("uploading file: content type: {:?}", content_type);
            let file_name = Uuid::new_v4().to_string();
            let mut file = File::create(format!("/tmp/{}", &file_name)).await?;
            let mut part = part.stream();
            while let Some(b) = part.next().await {
                let mut b = b?;
                while b.has_remaining() {
                    let data = b.chunk();
                    let read = data.len();
                    file.write_all(data).await?;
                    b.advance(read);
                }
            }
            let mut response = GenericResponse::default();
            response.data.insert("file".to_string(), file_name);
            return Ok(response);
        }
    }
    let error = GenericError {
        message: "Unknown error.".to_string(),
        ..Default::default()
    };
    Err(anyhow::anyhow!(error))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestJobResponse {
    job_id: String,
    message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenericResponse {
    #[serde(flatten)]
    pub data: HashMap<String, String>,
}

impl Default for GenericResponse {
    fn default() -> Self {
        GenericResponse {
            data: HashMap::with_capacity(2),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenericError {
    pub error_code: u16,
    pub message: String,
    #[serde(flatten)]
    pub data: HashMap<String, String>,
}

impl Default for GenericError {
    fn default() -> Self {
        GenericError{
            error_code: u16::MAX,
            message: String::new(),
            data: HashMap::new(),
        }
    }
}
impl Display for GenericError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {})", self.error_code, self.message, self.data.len())
    }
}
