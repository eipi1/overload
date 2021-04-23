#![allow(clippy::upper_case_acronyms)]

use std::cmp::min;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use pin_project::pin_project;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::HttpReq;
use futures_util::stream::Stream;
use tokio_stream::StreamExt;

const MAX_REQ_RET_SIZE: usize = 10;

/// Based on the QPS policy, generate requests to be sent to the application that's being tested.
#[pin_project]
#[must_use = "futures do nothing unless polled"]
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
    // http_util requests
    requests: Vec<HttpReq>,
    qps_scheme: Box<dyn QPSScheme + Send>,
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

//todo use failure rate for adaptive qps control
//https://micrometer.io/docs/concepts#rate-aggregation

impl RequestGenerator {
    pub fn new(
        duration: u32,
        requests: Vec<HttpReq>,
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
        }
    }
}

impl Stream for RequestGenerator {
    // impl<Q> Stream for RequestGenerator<Q> {
    type Item = (u32, Vec<HttpReq>);

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let me = self.project();
        if me.current_count >= me.total {
            Poll::Ready(None)
        } else {
            let qps = me.qps_scheme.next(*me.current_count, None);
            *me.current_count += 1;
            let len = me.requests.len();
            let req_size = min(qps as usize, min(len, MAX_REQ_RET_SIZE));
            let mut rng = rand::thread_rng();
            let reqs = (0..req_size)
                .map(|_| {
                    let idx = rng.gen_range(0..len);
                    me.requests.get(idx)
                })
                .filter(|req| req.is_some())
                .map(|req| req.unwrap().clone())
                .collect::<Vec<_>>();
            Poll::Ready(Some((qps, reqs)))
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
