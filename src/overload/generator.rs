#![allow(clippy::upper_case_acronyms)]

use crate::HttpReq;
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use futures_util::stream::Stream;


use rand::Rng;
use serde::{Deserialize, Serialize};
use std::cmp::min;

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio_stream::StreamExt;


const MAX_REQ_RET_SIZE: usize = 10;

pub enum ProviderOrFuture {
    Provider(Box<dyn RequestSpec + Send>),
    Future(BoxFuture<'static, (Box<dyn RequestSpec + Send>,Option<Vec<HttpReq>>)>),
    Dummy,
}

/// Based on the QPS policy, generate requests to be sent to the application that's being tested.
// #[pin_project]
#[must_use = "futures do nothing unless polled"]
// pub struct RequestGenerator {
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
    // http requests
    requests: Box<dyn RequestSpec + Send>,
    // requests: Box<dyn RequestSpec + Send +'a>,
    // requests: RequestSpecStruct,
    // requests: RequestSpecStruct,
    qps_scheme: Box<dyn QPSScheme + Send>,
    // get_req_future: Option<BoxFuture<'a,Option<Vec<HttpReq>>>>,
    provider_or_future: ProviderOrFuture,
}

impl RequestGenerator {
    pub fn time_scale(&self) -> u8 {
        self.time_scale
    }
}

pub trait QPSScheme {
    fn next(&self, nth: u32, last_qps: Option<u32>) -> u32;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConstantQPS {
    pub qps: u32,
}

impl QPSScheme for ConstantQPS {
    #[inline]
    fn next(&self, _nth: u32, _last_qps: Option<u32>) -> u32 {
        self.qps
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ArrayQPS {
    qps: Vec<u32>,
}

impl ArrayQPS {
    pub fn new(qps: Vec<u32>) -> Self {
        Self { qps }
    }
}

impl QPSScheme for ArrayQPS {
    #[inline]
    fn next(&self, nth: u32, _last_qps: Option<u32>) -> u32 {
        let len = self.qps.len();
        if len != 0 {
            let idx = nth as usize % len;
            let val = self.qps.get(idx).unwrap();
            *val
        } else {
            0
        }
    }
}

#[async_trait]
#[typetag::serde]
pub trait RequestSpec {
    async fn get_mut_n(&mut self, n: usize) -> Option<Vec<HttpReq>> {
        unimplemented!()
    }
    /// Return 1 request, be it chosen randomly or in any other way, entirely depends on implementations
    async fn get_one(&self) -> Option<HttpReq>;
    // async fn get_one(&self) -> Option<HttpReq> {
    //     self.get_n(1).await
    //         .and_then(|mut v| v.pop())
    // }
    /// Return `n` requests, be it chosen randomly or in any other way, entirely depends on implementations
    async fn get_n(&self, n: usize) -> Option<Vec<HttpReq>>;
    /// number of requests
    fn size_hint(&self) -> usize;
}

pub struct RequestSpecStruct {}
impl RequestSpecStruct {
    /// Return 1 request, be it chosen randomly or in any other way, entirely depends on implementations
    pub async fn get_one(&self) -> Option<HttpReq> {
        unimplemented!()
    }
    // async fn get_one(&self) -> Option<HttpReq> {
    //     self.get_n(1).await
    //         .and_then(|mut v| v.pop())
    // }
    /// Return `n` requests, be it chosen randomly or in any other way, entirely depends on implementations
    async fn get_n(&self, n: usize) -> Option<Vec<HttpReq>> {
        unimplemented!()
    }
    /// number of requests
    fn size_hint(&self) -> usize {
        unimplemented!()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RequestList {
    data: Vec<HttpReq>,
}

#[async_trait]
#[typetag::serde]
impl RequestSpec for RequestList {
    async fn get_one(&self) -> Option<HttpReq> {
        let i = rand::thread_rng().gen_range(0..self.data.len());
        self.data.get(i).cloned()
    }

    async fn get_n(&self, n: usize) -> Option<Vec<HttpReq>> {
        todo!()
    }

    fn size_hint(&self) -> usize {
        self.data.len()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RequestFile {
    file_name: String,
}

#[async_trait]
#[typetag::serde]
impl RequestSpec for RequestFile {
    async fn get_one(&self) -> Option<HttpReq> {
        todo!()
    }

    async fn get_n(&self, n: usize) -> Option<Vec<HttpReq>> {
        todo!()
    }

    fn size_hint(&self) -> usize {
        todo!()
    }
}

//todo use failure rate for adaptive qps control
//https://micrometer.io/docs/concepts#rate-aggregation

impl RequestGenerator {
    pub fn new(
        duration: u32,
        requests: Box<dyn RequestSpec + Send>,
        // requests: RequestSpecStruct,
        qps_scheme: Box<dyn QPSScheme + Send>,
    ) -> Self {
        let time_scale: u8 = 1;
        let total = duration * time_scale as u32;
        RequestGenerator {
            duration,
            time_scale,
            total,
            requests,
            qps_scheme,
            current_count: 0,
            // get_req_future: None,
            provider_or_future: ProviderOrFuture::Dummy,
        }
    }
}

// impl<'a> Stream for RequestGenerator<'a> {
impl Stream for RequestGenerator {
    type Item = (u32, Vec<HttpReq>);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.current_count >= self.total {
            Poll::Ready(None)
        } else {
            let provider_or_future =
                std::mem::replace(&mut self.provider_or_future, ProviderOrFuture::Dummy);
            match provider_or_future {
                ProviderOrFuture::Provider(mut provider) => {
                    let qps = self.qps_scheme.next(self.current_count, None);
                    let len = self.requests.size_hint();
                    let req_size = min(qps as usize, min(len, MAX_REQ_RET_SIZE));
                    self.current_count += 1;
                    let provider_and_fut = async move {
                        let requests = provider.get_mut_n(req_size).await;
                        (provider, requests)
                    };
                    self.provider_or_future = ProviderOrFuture::Future(Box::pin(provider_and_fut));
                    self.poll_next(cx)
                }
                ProviderOrFuture::Future(mut future) => {
                    match future.as_mut().poll(cx) {
                        Poll::Ready((provider,result)) => {
                            self.provider_or_future = ProviderOrFuture::Provider(provider);
                            let result = result.unwrap_or_default();
                            Poll::Ready(Some((0,result)))
                        }
                        Poll::Pending => Poll::Pending
                    }
                }
                ProviderOrFuture::Dummy => panic!("Something went wrong :(")
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Option::from(self.total as usize))
    }
}

#[allow(clippy::let_and_return)]
pub fn request_generator_stream(
    generator: RequestGenerator,
) -> impl Stream<Item = (u32, Vec<HttpReq>)> {
    // ) -> impl Stream<Item = (u32, Vec<HttpReq>)> + '_ {
    //todo impl throttled stream function according to time_scale
    let scale = generator.time_scale;
    let throttle = generator.throttle(Duration::from_millis(1000 / scale as u64));
    // tokio::pin!(throttle); //Why causes error => the parameter type `Q` may not live long enough.
    throttle
}

/// Increase QPS linearly as per equation
/// `y=ax+b` until hit the max cap
#[derive(Debug, Serialize, Deserialize)]
pub struct Linear {
    a: f32,
    b: u32,
    max: u32,
}

impl QPSScheme for Linear {
    fn next(&self, nth: u32, _last_qps: Option<u32>) -> u32 {
        min((self.a * nth as f32).ceil() as u32 + self.b, self.max)
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use tokio::time;
    use tokio_stream::StreamExt;
    use tokio_test::{assert_pending, assert_ready, task};

    use crate::generator::{
        request_generator_stream, ConstantQPS, RequestGenerator, MAX_REQ_RET_SIZE,
    };
    use crate::HttpReq;
    use std::collections::HashMap;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_request_generator_empty_req() {
        let mut generator = RequestGenerator::new(3, Vec::new(), Box::new(ConstantQPS { qps: 3 }));
        let ret = generator.next().await;
        if let Some(arg) = ret {
            assert_eq!(arg.0, 3);
            assert_eq!(arg.1.len(), 0);
        } else {
            panic!("fails");
        }
        //stream should generate 3 value
        assert_eq!(generator.next().await.is_some(), true);
        assert_eq!(generator.next().await.is_some(), true);
        assert_eq!(generator.next().await.is_some(), false);
    }

    #[tokio::test]
    async fn test_request_generator_req_lt_qps() {
        let mut vec = Vec::new();
        vec.push(test_http_req());
        let mut generator = RequestGenerator::new(3, vec, Box::new(ConstantQPS { qps: 3 }));
        let ret = generator.next().await;
        if let Some(arg) = ret {
            assert_eq!(arg.0, 3);
            assert_eq!(arg.1.len(), 1);
            if let Some(req) = arg.1.get(0) {
                assert_eq!(req.method, http::Method::GET);
            }
        } else {
            panic!()
        }
    }

    #[tokio::test]
    async fn test_request_generator_req_eq_qps() {
        let mut vec = Vec::new();
        vec.push(test_http_req());
        vec.push(test_http_req());
        vec.push(test_http_req());
        let mut generator = RequestGenerator::new(3, vec, Box::new(ConstantQPS { qps: 3 }));
        let ret = generator.next().await;
        if let Some(arg) = ret {
            assert_eq!(arg.0, 3);
            assert_eq!(arg.1.len(), 3);
        } else {
            panic!()
        }
    }

    #[tokio::test]
    async fn test_request_generator_req_gt_max() {
        let vec = (0..12).map(|_| test_http_req()).collect();
        let mut generator = RequestGenerator::new(3, vec, Box::new(ConstantQPS { qps: 15 }));
        let ret = generator.next().await;
        if let Some(arg) = ret {
            assert_eq!(arg.0, 15);
            assert_eq!(arg.1.len(), MAX_REQ_RET_SIZE);
        } else {
            panic!()
        }
    }

    //noinspection Duplicates
    #[tokio::test]
    async fn test_request_generator_stream() {
        time::pause();
        let mut vec = Vec::new();
        vec.push(test_http_req());
        let generator = RequestGenerator::new(3, vec, Box::new(ConstantQPS { qps: 3 }));
        let throttle = request_generator_stream(generator);
        // let throttle = generator.throttle(Duration::from_secs(1));
        let mut throttle = task::spawn(throttle);
        // tokio::pin!(throttle);
        let ret = throttle.poll_next();
        assert_ready!(ret);
        assert_pending!(throttle.poll_next());
        time::advance(Duration::from_millis(1001)).await;
        assert_ready!(throttle.poll_next());
    }

    fn test_http_req() -> HttpReq {
        HttpReq {
            id: Uuid::new_v4().to_string(),
            method: http::Method::GET,
            url: "http://example.com".to_string(),
            body: None,
            headers: HashMap::new(),
        }
    }
}
