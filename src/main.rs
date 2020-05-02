use async_trait::async_trait;
use futures::stream;
use futures::stream::FuturesUnordered;
use hyper::{http, Body, Client, Request, Response, Uri};
use serde::export::fmt::Debug;
use serde::export::Option::Some;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::convert::{TryFrom, TryInto};
use std::future::Future;
use std::time::Duration;
use tokio::stream::StreamExt;
use tokio::time::throttle;
use tracing::{debug, info, instrument};
use tracing_subscriber;
use url::Url;

#[tokio::main]
async fn main() {
    let tracing_builder = tracing_subscriber::fmt()
        .with_env_filter("debug")
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
        qps_spec: ConstantQPSSPec { qps: 4 },
        req_spec: single_req,
        duration: 10,
    };
    debug!("Start processing request {:?}", overload_req);
    execute(overload_req).await;
}

#[instrument(skip(overload_req))]
async fn execute<T, U>(mut overload_req: OverloadRequest<T, U>)
where
    T: ReqSpec + Debug,
    U: QPSSpec + Debug,
{
    info!("Processing request");
    let duration = overload_req.duration;
    let qps_spec = overload_req.get_qps_spec_mut();
    let qps = qps_spec.next();
    let req_spec = overload_req.get_req_spec_mut();
    let mut qps_stream = throttle(
        Duration::from_secs(1),
        futures::stream::repeat(qps).take(duration as usize),
    );
    let client = Client::new();
    while let Some(qps) = qps_stream.next().await {
        info!("QPS => {}", qps);
        client.get(req_spec.next().url.clone().as_str().try_into().unwrap());
        for _ in 0..qps {}
        // generate_request();
        // let f = FuturesUnordered::new();
    }
    // while let Some(qps) = qps_stream.next().await {
    //     let mut request_futures = FuturesUnordered::new();
    //     for _ in 1..(qps+1) {
    //         let resp = client.get(request.request.url.clone().try_into().unwrap());
    //         request_futures.push(resp);
    //     }
    //     let response = request_futures.next().await.unwrap().unwrap();
    //     info!("response {}, headers => {:?}", response.status(), response.headers())
    // }
}

// impl TryFrom<Url> for http::Uri {
//     type Error = ();
//
//     fn try_from(value: Url) -> Result<Self, Self::Error> {
//         value.as_str().try_into()?
//     }
// }

// impl From<Url> for http::Uri{
//     fn from(_: Url) -> Self {
//         unimplemented!()
//     }
// }

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

#[derive(Debug, Serialize, Deserialize)]
enum ReqMethod {
    GET,
    POST,
}

#[derive(Debug, Serialize, Deserialize)]
struct OverloadRequest<R, Q> {
    req_spec: R,
    qps_spec: Q,
    duration: i32,
}

impl<R: ReqSpec, Q: QPSSpec> OverloadRequest<R, Q> {
    fn get_req_spec(&self) -> &R {
        &self.req_spec
    }

    fn get_req_spec_mut(&mut self) -> &mut R {
        &mut self.req_spec
    }

    fn get_qps_spec(&self) -> &Q {
        &self.qps_spec
    }

    fn get_qps_spec_mut(&mut self) -> &mut Q {
        &mut self.qps_spec
    }

    fn duration(&self) -> i32 {
        self.duration
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct HttpReq {
    method: ReqMethod,
    url: Url,
    body: Option<Vec<u8>>,
}

trait ReqSpec {
    fn next(&mut self) -> &HttpReq;
}

#[derive(Debug)]
struct SingleReqSpec {
    req: HttpReq,
}

impl ReqSpec for SingleReqSpec {
    #[inline]
    fn next(&mut self) -> &HttpReq {
        &self.req
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
