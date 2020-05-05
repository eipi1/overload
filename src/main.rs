#![allow(unused_imports)]

use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream, StreamExt};
use futures::stream::FuturesUnordered;
use hyper::client::HttpConnector;
use hyper::{http, Body, Client, Method, Request, Response, Uri};
use serde::export::fmt::Debug;
use serde::export::Option::Some;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell, RefMut};
use std::convert::{TryFrom, TryInto};
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::throttle;
use tracing::{debug, info, instrument};
use tracing_subscriber;
use url::Url;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .try_init()
        .unwrap();
    let single_req = SingleReqSpec {
        req: HttpReq {
            method: ReqMethod::GET,
            url: Url::parse("http://localhost:31380/productpage").unwrap(),
            body: None,
        },
    };

    let overload_req = OverloadRequest {
        qps_spec: RefCell::new(ConstantQPSSPec { qps: 4 }),
        req_spec: RefCell::new(single_req),
        duration: 10,
    };
    debug!("Start processing request {:?}", overload_req);
    execute(overload_req).await;
}

#[instrument(skip(overload_req))]
async fn execute<T, U>(overload_req: OverloadRequest<T, U>)
where
    T: ReqSpec + Debug,
    U: QPSSpec + Debug,
{
    info!("Processing request");
    let duration = overload_req.duration();
    let mut qps_spec = overload_req.get_qps_spec_mut();
    let mut req_spec = overload_req.get_req_spec_mut();
    let qps = qps_spec.next();
    let mut qps_stream = throttle(
        Duration::from_secs(1),
        futures::stream::repeat(qps).take(duration as usize),
    );
    let client = Arc::new(Client::new());
    while let Some(qps) = qps_stream.next().await {
        tokio::spawn(send_requests(req_spec.next(), client.clone(), qps));
    }
}

async fn send_requests(req: HttpReq, client: Arc<Client<HttpConnector, Body>>, count: i32) {
    info!("sending {} requests", count);
    let body = if let Some(body) = req.body {
        Bytes::from(body)
    } else {
        Bytes::new()
    };
    let mut request_futures = FuturesUnordered::new();
    for _ in 1..=count {
        let request = Request::builder()
            .uri(req.url.as_str())
            .method(req.method.clone())
            .body(body.clone().into())
            .unwrap();
        let request = client.request(request);
        request_futures.push(request);
    }
    request_futures.next().await;
}

trait QPSSpec {
    fn next(&mut self) -> i32;
    fn next_few(&mut self, count: i32) -> Vec<i32>;
}

#[derive(Debug, Serialize, Deserialize)]
struct ConstantQPSSPec {
    qps: i32,
}

impl QPSSpec for ConstantQPSSPec {
    #[inline(always)]
    fn next(&mut self) -> i32 {
        self.qps
    }

    #[inline]
    fn next_few(&mut self, count: i32) -> Vec<i32> {
        vec![self.qps, count]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum ReqMethod {
    GET,
    POST,
}

impl Into<Method> for ReqMethod {
    fn into(self) -> Method {
        match self {
            ReqMethod::POST => Method::POST,
            _ => Method::GET,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct OverloadRequest<R, Q> {
    req_spec: RefCell<R>,
    qps_spec: RefCell<Q>,
    duration: i32,
}

impl<R: ReqSpec, Q: QPSSpec> OverloadRequest<R, Q> {
    fn get_req_spec_mut(&self) -> RefMut<'_, R> {
        self.req_spec.borrow_mut()
    }

    fn get_qps_spec_mut(&self) -> RefMut<'_, Q> {
        self.qps_spec.borrow_mut()
    }

    fn duration(&self) -> i32 {
        self.duration
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HttpReq {
    method: ReqMethod,
    url: Url,
    body: Option<Vec<u8>>,
}

trait ReqSpec {
    fn next(&mut self) -> HttpReq;
}

#[derive(Debug)]
struct SingleReqSpec {
    req: HttpReq,
}

impl ReqSpec for SingleReqSpec {
    #[inline]
    fn next(&mut self) -> HttpReq {
        self.req.clone()
    }
}

// #[async_trait]
// trait Overload<T,U> {
//     async fn execute(&self, spec: OverloadRequest<T, U>);
// }
//
// struct LocalOverload;
//
// #[async_trait]
// impl<T: ReqSpec+Send, U: QPSSpec + Send> Overload<T, U> for LocalOverload {
//     async fn execute(&self, spec: OverloadRequest<T, U>) {
//
//     }
// }
