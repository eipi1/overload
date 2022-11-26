use std::collections::{BTreeMap, HashMap};
use std::env;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};
use std::time::Instant;

use anyhow::Result as AnyResult;
use futures_core::future::BoxFuture;
use futures_core::Stream;
use futures_util::FutureExt;
use http::{StatusCode, Uri};
use hyper::body::Bytes;
use hyper::client::conn::ResponseFuture;
use hyper::Error;
use lazy_static::lazy_static;
use log::{error, info, trace};
use once_cell::sync::OnceCell;
use remoc::rch::base::{Receiver, SendError, Sender};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio::sync::RwLock;

use overload_http::{
    ConcurrentConnectionRateSpec, Elastic, HttpReq, RateSpecEnum, Request, RequestSpecEnum, Target,
};
use overload_metrics::Metrics;
use response_assert::ResponseAssertion;

use crate::rate_spec::RateScheme;
use crate::request_providers::RequestProvider;

mod connection;
#[cfg(feature = "cluster")]
pub mod primary;
mod rate_spec;
mod request_providers;
pub mod secondary;
#[cfg(not(feature = "cluster"))]
pub mod standalone;

pub const CYCLE_LENGTH_IN_MILLIS: i128 = 1_000; //1 seconds
pub const DEFAULT_REMOC_PORT: u16 = 3031;
pub const DEFAULT_REMOC_PORT_NAME: &str = "tcp-remoc";
pub const ENV_NAME_REMOC_PORT: &str = "CLUSTER_COM_PORT";
pub const ENV_NAME_REMOC_PORT_NAME: &str = "CLUSTER_COM_PORT_NAME";
pub static REMOC_PORT: OnceCell<u16> = OnceCell::new();
pub static REMOC_PORT_NAME: OnceCell<String> = OnceCell::new();

lazy_static! {
    pub(crate) static ref JOB_STATUS: RwLock<BTreeMap<String, JobStatus>> =
        RwLock::new(BTreeMap::new());
}

pub fn remoc_port() -> u16 {
    env::var(ENV_NAME_REMOC_PORT)
        .map_err(|_| ())
        .and_then(|port| u16::from_str(&port).map_err(|_| ()))
        .unwrap_or(DEFAULT_REMOC_PORT)
}

pub fn remoc_port_name() -> String {
    env::var(ENV_NAME_REMOC_PORT_NAME).unwrap_or_else(|_| DEFAULT_REMOC_PORT_NAME.to_string())
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone)]
pub enum JobStatus {
    Starting,
    InProgress,
    Stopped,
    Completed,
    Failed,
    Error(ErrorCode),
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone)]
pub enum ErrorCode {
    InactiveCluster,
    NoSecondary,
    SqliteOpenFailed,
    PreparationFailed,
    SecondaryClusterNode,
    RequestFileNotFound,
    Others,
}

#[derive(Debug, Serialize, Deserialize)]
struct RateMessage {
    qps: u32,
    connections: u32,
}

#[derive(Serialize, Deserialize, Debug)]
struct Metadata {
    primary_host: String,
}

#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Debug)]
pub(crate) enum MessageFromPrimary {
    Metadata(Metadata),
    Request(Request),
    Rates(RateMessage),
    Stop,
    Finished,
}

type ReqProvider = Box<dyn RequestProvider + Send>;

pub enum ProviderOrFuture {
    Provider(ReqProvider),
    Future(BoxFuture<'static, (ReqProvider, AnyResult<Vec<HttpReq>>)>),
    Dummy,
}

/// Based on the QPS policy, generate requests to be sent to the application that's being tested.
#[must_use = "futures do nothing unless polled"]
#[allow(dead_code)]
pub struct RequestGenerator {
    // for how long test should run
    duration: u32,
    // How time should be scaled. Default value is 1, means will generate qps+request once
    // every second.
    // Doesn't support custom value for now.
    time_scale: u8,
    // total number of requests to be generated
    total: u32,
    // requests generated so far, initially 0
    current_count: u32,
    current_qps: u32,
    shared_provider: bool,
    pub(crate) target: Target,
    qps_scheme: Box<dyn RateScheme + Send>,
    concurrent_connection: Box<dyn RateScheme + Send>,
    pub(crate) response_assertion: Option<ResponseAssertion>,
}

//todo use failure rate for adaptive qps control
//https://micrometer.io/docs/concepts#rate-aggregation
impl RequestGenerator {
    pub fn new(
        duration: u32,
        requests: ReqProvider,
        qps_scheme: Box<dyn RateScheme + Send>,
        target: Target,
        concurrent_connection: Option<Box<dyn RateScheme + Send>>,
        response_assertion: Option<ResponseAssertion>,
    ) -> Self {
        let time_scale: u8 = 1;
        let total = duration * time_scale as u32;
        let concurrent_connection =
            concurrent_connection.unwrap_or_else(|| Box::new(Elastic::default()));
        RequestGenerator {
            duration,
            time_scale,
            total,
            current_qps: 0,
            shared_provider: requests.shared(),
            qps_scheme,
            current_count: 0,
            target,
            concurrent_connection,
            response_assertion,
        }
    }
}

#[allow(clippy::from_over_into)]
impl Into<RequestGenerator> for Request {
    fn into(self) -> RequestGenerator {
        let qps: Box<dyn RateScheme + Send> = match self.qps {
            RateSpecEnum::ConstantRate(qps) => Box::new(qps),
            RateSpecEnum::Linear(qps) => Box::new(qps),
            RateSpecEnum::ArraySpec(qps) => Box::new(qps),
            RateSpecEnum::Steps(qps) => Box::new(qps),
        };
        let req: Box<dyn RequestProvider + Send> = match self.req {
            RequestSpecEnum::RequestList(req) => Box::new(req),
            RequestSpecEnum::RequestFile(mut req) => {
                //todo why rename here?
                // req.file_name = format!("{}/{}.sqlite", data_dir(), &req.file_name);
                let mut path = data_dir_path().join(&req.file_name);
                path.set_extension("sqlite");
                req.file_name = path.to_str().unwrap().to_string();
                Box::new(req)
            }
            RequestSpecEnum::RandomDataRequest(req) => Box::new(req),
        };

        let connection_rate = if let Some(connection_rate_spec) = self.concurrent_connection {
            let rate_spec: Box<dyn RateScheme + Send> = match connection_rate_spec {
                ConcurrentConnectionRateSpec::ArraySpec(spec) => Box::new(spec),
                ConcurrentConnectionRateSpec::ConstantRate(spec) => Box::new(spec),
                ConcurrentConnectionRateSpec::Linear(spec) => Box::new(spec),
                ConcurrentConnectionRateSpec::Elastic(spec) => Box::new(spec),
                ConcurrentConnectionRateSpec::Steps(spec) => Box::new(spec),
            };
            Some(rate_spec)
        } else {
            None
        };

        RequestGenerator::new(
            self.duration,
            req,
            qps,
            self.target,
            connection_rate,
            self.response_assertion,
        )
    }
}

impl Stream for RequestGenerator {
    type Item = (u32, u32);

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.current_count >= self.total {
            trace!("finished generating QPS");
            Poll::Ready(None)
        } else {
            let current_count = self.current_count;
            let qps = self.qps_scheme.next(current_count, None);
            let conn = self.concurrent_connection.next(current_count, None);
            self.current_count += 1;
            Poll::Ready(Some((qps, conn)))
        }
    }
}

enum HttpRequestState {
    Init,
    InProgress,
    ResponseReceived,
    // Done,
}

enum ReturnableConnection {
    PlaceHolder,
}

//todo remove unnecessary fields
#[must_use = "futures do nothing unless polled"]
struct HttpRequestFuture<'a> {
    state: HttpRequestState,
    timer: Option<Instant>,
    job_id: String,
    request: Pin<Box<ResponseFuture>>,
    body: Option<BoxFuture<'a, Result<Bytes, Error>>>,
    status: Option<StatusCode>,
    metrics: &'a Metrics,
    connection: ReturnableConnection,
    assertion: &'a ResponseAssertion,
    request_uri: Uri,
}

impl Future for HttpRequestFuture<'_> {
    type Output = ReturnableConnection;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _e = "ERROR".to_string();
        if let HttpRequestState::Init = self.state {
            self.metrics.upstream_request_count(1);
            self.timer = Some(Instant::now());
            self.state = HttpRequestState::InProgress;
            trace!("HttpRequestFuture [{}] - Begin", &self.job_id);
        }

        if self.body.is_some() {
            let fut = self.body.as_mut().unwrap();
            return match Pin::new(fut).poll(cx) {
                Poll::Ready(body) => {
                    let elapsed = self.timer.unwrap().elapsed().as_millis() as f64;
                    trace!(
                        "HttpRequestFuture [{}] - body - Ready, elapsed={}",
                        &self.job_id,
                        &elapsed
                    );
                    if let Ok(body) = body {
                        if !self.assertion.is_empty() {
                            trace!(
                                "[HttpRequestFuture] [{}] - asserting - {:?}",
                                &self.job_id,
                                &self.assertion
                            );
                            let _ = serde_json::from_slice(body.as_ref())
                                .map_err(|e| {
                                    self.metrics.assertion_parse_failure(&e.to_string());
                                    vec![]
                                })
                                .and_then(|json_resp| {
                                    response_assert::assert(
                                        self.assertion,
                                        &self.request_uri,
                                        None,
                                        &json_resp,
                                    )
                                })
                                .map_err(|errors| {
                                    for err in errors {
                                        info!("assertion error - {:?}", &err);
                                        self.metrics.assertion_failure(err.get_id(), err.into());
                                    }
                                });
                        }
                    };
                    //add metrics here if it's necessary to include time to fetch body
                    Poll::Ready(std::mem::replace(
                        &mut self.connection,
                        ReturnableConnection::PlaceHolder,
                    ))
                }
                Poll::Pending => {
                    trace!("HttpRequestFuture [{}] - body - Pending", &self.job_id);
                    Poll::Pending
                }
            };
        } else if matches!(self.state, HttpRequestState::ResponseReceived) {
            trace!(
                "HttpRequestFuture [{}] - body: None - completing",
                &self.job_id
            );
            return Poll::Ready(std::mem::replace(
                &mut self.connection,
                ReturnableConnection::PlaceHolder,
            ));
        }

        match Pin::new(&mut self.request).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(val) => {
                self.state = HttpRequestState::ResponseReceived;
                match val {
                    Ok(response) => {
                        let status = response.status();
                        self.status = Some(status);
                        let elapsed = self.timer.unwrap().elapsed().as_millis() as f64;
                        trace!(
                            "HttpRequestFuture [{}] - status: {:?}, elapsed: {}",
                            &self.job_id,
                            &self.status,
                            &elapsed
                        );
                        let status_str = status.as_str();
                        self.metrics.upstream_response_time(status_str, elapsed);
                        self.metrics.upstream_request_status_count(1, status_str);
                        //todo use aggregate instead of to_bytes
                        let bytes = hyper::body::to_bytes(response.into_body());
                        self.body = Some(bytes.boxed());
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                    Err(err) => {
                        trace!("HttpRequestFuture [{}] - ERROR: {}", &self.job_id, &err);
                        self.metrics
                            .upstream_request_status_count(1, err.to_string().as_str());
                        Poll::Ready(std::mem::replace(
                            &mut self.connection,
                            ReturnableConnection::PlaceHolder,
                        ))
                    }
                }
            }
        }
    }
}

pub const DEFAULT_DATA_DIR: &str = "/tmp";

pub fn data_dir_path() -> PathBuf {
    env::var("DATA_DIR")
        .map(|env| PathBuf::from("/").join(env))
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_DATA_DIR))
}

#[macro_export]
macro_rules! log_error {
    ($result:expr) => {
        if let Err(e) = $result {
            use log::error;
            error!("{}", e.to_string());
        }
    };
}

async fn get_sender_for_host_port(port: u16, host: &str) -> Option<Sender<MessageFromPrimary>> {
    info!(
        "[get_sender_for_host_port] - connecting to {}:{}",
        &host, port
    );
    let socket = TcpStream::connect((host, port))
        .await
        .map_err(|e| {
            error!(
                "[get_sender_for_host_port] - failed to connect to {}:{}, error: {:?}",
                &host, port, e
            )
        })
        .ok()?;
    let (socket_rx, socket_tx) = socket.into_split();

    let (conn, tx, _): (_, Sender<MessageFromPrimary>, Receiver<()>) =
        remoc::Connect::io(remoc::Cfg::default(), socket_rx, socket_tx)
            .await
            .map_err(|e| error!("{:?}", e))
            .ok()?;
    tokio::spawn(conn);
    Some(tx)
}

/// Sends messages requires for initialization
#[inline(always)]
pub(crate) async fn init_sender(
    request: Request,
    primary_host: String,
    sender: &mut Sender<MessageFromPrimary>,
) -> Result<(), SendError<MessageFromPrimary>> {
    sender
        .send(MessageFromPrimary::Metadata(Metadata { primary_host }))
        .await?;
    sender.send(MessageFromPrimary::Request(request)).await
}

async fn send_end_msg(sender: &mut Sender<MessageFromPrimary>, stop: bool) {
    let msg = if stop {
        MessageFromPrimary::Stop
    } else {
        MessageFromPrimary::Finished
    };
    let result = sender.send(msg).await;
    log_error!(result);
}

pub async fn stop_request_by_job_id(job_id: &String) -> Vec<(String, Option<JobStatus>)> {
    let mut write = JOB_STATUS.write().await;
    write
        .iter_mut()
        .filter(|key_val| key_val.0.starts_with(job_id))
        .map(|key_val| {
            let prev_status = std::mem::replace(key_val.1, JobStatus::Stopped);
            (key_val.0.clone(), Some(prev_status))
        })
        .collect::<Vec<_>>()
}

pub async fn get_status_by_job_id(job_id: &str) -> HashMap<String, JobStatus> {
    return JOB_STATUS
        .read()
        .await
        .iter()
        .filter(|status| status.0.starts_with(job_id))
        .map(|status| (status.0.clone(), *status.1))
        .collect();
}

pub async fn get_status_all() -> BTreeMap<String, JobStatus> {
    JOB_STATUS.read().await.clone()
}

pub async fn cleanup_job<P>(predicate: P)
where
    P: Fn(&JobStatus) -> bool,
{
    let indices = {
        let mut indices = vec![];
        let read_guard = JOB_STATUS.read().await;
        for (id, status) in read_guard.iter() {
            if predicate(status) {
                indices.push(id.clone());
            }
        }
        indices
    };
    {
        let mut write_guard = JOB_STATUS.write().await;
        for id in indices {
            write_guard.remove(&id);
        }
    }
}

#[cfg(test)]
mod test_common {
    use crate::RequestGenerator;
    #[cfg(feature = "cluster")]
    use cluster_mode::RestClusterNode;
    use overload_http::Request;
    #[cfg(feature = "cluster")]
    use rust_cloud_discovery::{Port, ServiceInstance};
    #[cfg(feature = "cluster")]
    use serde_json::json;
    #[cfg(feature = "cluster")]
    use std::collections::HashMap;
    #[cfg(feature = "cluster")]
    use std::str::FromStr;
    use std::sync::Once;
    use tracing_core::Level;
    #[cfg(feature = "cluster")]
    use uuid::Uuid;

    #[allow(dead_code)]
    static INIT: Once = Once::new();

    #[allow(dead_code)]
    pub fn init() {
        INIT.call_once(|| {
            tracing_subscriber::fmt()
                .with_max_level(Level::TRACE)
                .init();
        });
    }

    // #[tokio::test]
    // #[allow(unused_must_use)]
    // async fn test_req_param() {
    //     let generator = RequestGenerator::new(
    //         3,
    //         Box::new(req_list_with_n_req(1)),
    //         Box::new(ConstantRate { count_per_sec: 1 }),
    //         Target {
    //             host: "example.com".into(),
    //             port: 8080,
    //             protocol: Scheme::HTTP,
    //         },
    //         None,
    //         None,
    //     );
    //     request_generator_stream(generator);
    // }

    // pub fn req_list_with_n_req(n: usize) -> RequestList {
    //     let data = (0..n)
    //         .map(|_| {
    //             let r = rand::random::<u8>();
    //             HttpReq {
    //                 id: Uuid::new_v4().to_string(),
    //                 body: None,
    //                 url: format!("https://httpbin.org/anything/{}", r),
    //                 method: http::Method::GET,
    //                 headers: HashMap::new(),
    //             }
    //         })
    //         .collect::<Vec<_>>();
    //     RequestList { data }
    // }

    #[cfg(feature = "cluster")]
    pub fn cluster_node(http: usize, remoc: u32) -> RestClusterNode {
        let port1 = Port::new(
            Some("http-endpoint".to_string()),
            http as u32,
            "tcp".to_string(),
            Some("http".to_string()),
        );
        let port2 = Port::new(
            Some("tcp-remoc".to_string()),
            remoc as u32,
            "tcp".to_string(),
            Some("tcp".to_string()),
        );
        let ports = vec![port1, port2];

        RestClusterNode::new(
            uuid::Uuid::new_v4().to_string(),
            ServiceInstance::new(
                Some(Uuid::new_v4().to_string()),
                Some(String::from_str("test").unwrap()),
                Some(String::from_str("127.0.0.1").unwrap()),
                Some(ports),
                false,
                Some("http://127.0.0.1:3030".to_string()),
                HashMap::new(),
                Some(String::from_str("HTTP").unwrap()),
            ),
        )
    }

    #[cfg(feature = "cluster")]
    pub(crate) fn get_request(host: String, port: u16) -> Request {
        let uuid = Uuid::new_v4().to_string();
        let req = json!(
        {
          "duration": 120,
          "name": uuid,
          "qps": {
            "ConstantRate": {
              "countPerSec": 1
            }
          },
          "req": {
            "RequestList": {
              "data": [
                {
                  "body": null,
                  "method": "GET",
                  "url": "/get"
                }
              ]
            }
          },
          "target": {
            "host": host,
            "port": port,
            "protocol": "HTTP"
          }
        });
        // "###;
        // let result = serde_json::from_str::<Request>(req);
        let result = serde_json::from_value::<Request>(req);
        result.unwrap()
    }

    #[tokio::test]
    async fn test_request_1() {
        let req = r#"
            {
              "duration": 5,
              "name": "demo-test",
              "qps": {
                "ConstantRate": {
                  "countPerSec": 2
                }
              },
              "req": {
                "RequestList": {
                  "data": [
                    {
                      "method": "GET",
                      "url": "example.com"
                    }
                  ]
                }
              },
              "target": {
                "host": "example.com",
                "port": 8080,
                "protocol": "HTTP"
              },
              "histogramBuckets": [35,40,45,48,50, 52]
            }
        "#;
        let result = serde_json::from_str::<Request>(req);
        match result {
            Err(err) => {
                panic!("Error: {}", err);
            }
            Ok(request) => {
                let _: RequestGenerator = request.into();
            }
        }
    }

    #[tokio::test]
    async fn test_request_2() {
        let req = r#"
            {
              "duration": 5,
              "name": "demo-test",
              "qps": {
                "ConstantRate": {
                  "countPerSec": 2
                }
              },
              "req": {
                "RequestList": {
                  "data": [
                    {
                      "method": "GET",
                      "url": "example.com"
                    }
                  ]
                }
              },
              "target": {
                "host": "example.com",
                "port": 8080,
                "protocol": "HTTP"
              },
              "concurrentConnections": {
                "max":100
              },
              "histogramBuckets": [35,40,45,48,50, 52]
            }
        "#;
        let result = serde_json::from_str::<Request>(req);
        match result {
            Err(err) => {
                panic!("Error: {}", err);
            }
            Ok(request) => {
                let _: RequestGenerator = request.into();
            }
        }
    }
}
