#![allow(clippy::upper_case_acronyms)]

use crate::http_util::request::RequestSpecEnum;
use crate::HttpReq;
use anyhow::Result as AnyResult;
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use futures_util::stream::Stream;
use log::trace;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, SqliteConnection};
use std::cmp::min;
use std::iter::repeat_with;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio_stream::StreamExt;

const MAX_REQ_RET_SIZE: usize = 10;

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
    // replace ProviderOrFuture::Provider with enum, no need to use conversion
    // or check if Any can be used like sqlx AnyConnection
    req_provider: String,
    provider_or_future: ProviderOrFuture,
    qps_scheme: Box<dyn QPSScheme + Send>,
}

impl RequestGenerator {
    pub fn time_scale(&self) -> u8 {
        self.time_scale
    }

    pub fn shared_provider(&self) -> bool {
        self.shared_provider
    }

    pub fn request_provider(&self) -> &String {
        &self.req_provider
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
pub trait RequestProvider {
    /// Ask for `n` requests, be it chosen randomly or in any other way, entirely depends on implementations.
    /// For randomized picking, it's not guaranteed to return exactly n requests, for example -
    /// when randomizer return duplicate request id(e.g. sqlite ROWID)
    ///
    /// #Panics
    /// Implementations should panic if n=0, to notify invoker it's an unnecessary call and
    /// should be handled properly
    async fn get_n(&mut self, n: usize) -> AnyResult<Vec<HttpReq>>;

    /// number of total requests
    fn size_hint(&self) -> usize;

    /// The provider can be shared between instances in cluster. This will allow sending requests to
    /// secondary/worker instances without request data.
    fn shared(&self) -> bool;

    /// Not a good solution; it creates circular dependency, temporary hack, should find better solution
    fn to_json_str(&self) -> String;
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RequestList {
    pub(crate) data: Vec<HttpReq>,
}

#[async_trait]
impl RequestProvider for RequestList {
    async fn get_n(&mut self, n: usize) -> AnyResult<Vec<HttpReq>> {
        if n == 0 {
            panic!("RequestList: shouldn't request data of 0 size");
        }

        if self.data.is_empty() {
            return Err(anyhow::anyhow!("No Data Found"));
        }

        let random_data = repeat_with(|| fastrand::usize(0..self.data.len()))
            .take(n)
            .map(|i| self.data.get(i))
            .map(|r| r.cloned())
            .map(Option::unwrap)
            .collect::<Vec<_>>();
        Ok(random_data)
    }

    fn size_hint(&self) -> usize {
        self.data.len()
    }

    fn shared(&self) -> bool {
        false
    }

    fn to_json_str(&self) -> String {
        let spec_enum = RequestSpecEnum::RequestList(RequestList {
            data: self.data.clone(),
        });
        serde_json::to_string(&spec_enum).unwrap()
    }
}

impl From<Vec<HttpReq>> for RequestList {
    fn from(data: Vec<HttpReq>) -> Self {
        RequestList { data }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RequestFile {
    pub(crate) file_name: String,
    #[serde(skip)]
    inner: Option<SqliteConnection>,
    #[serde(skip)]
    #[serde(default = "default_request_file_size")]
    size: usize,
}

impl RequestFile {
    pub(crate) fn new(file_name: String) -> RequestFile {
        RequestFile {
            file_name,
            inner: None,
            size: default_request_file_size(),
        }
    }
}

impl Clone for RequestFile {
    fn clone(&self) -> Self {
        RequestFile::new(self.file_name.clone())
    }
}

#[async_trait]
impl RequestProvider for RequestFile {
    async fn get_n(&mut self, n: usize) -> AnyResult<Vec<HttpReq>> {
        if n == 0 {
            panic!("RequestFile: shouldn't request data of 0 size");
        }
        if self.inner.is_none() {
            //open sqlite connection
            let mut connection =
                SqliteConnectOptions::from_str(format!("sqlite://{}", &self.file_name).as_str())?
                    .read_only(true)
                    .connect()
                    .await?;
            let size: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM http_req")
                .fetch_one(&mut connection)
                .await?;
            self.size = size.0 as usize;
            self.inner = Some(connection);
            trace!("found rows: {}", &self.size);
        }
        let mut random_ids = repeat_with(|| fastrand::usize(1..=self.size))
            .take(n)
            .fold(String::new(), |acc, r| acc + &r.to_string() + ",");
        random_ids.pop();

        let random_data: Vec<HttpReq> = sqlx::query_as(
            format!(
                "SELECT ROWID, * FROM http_req WHERE ROWID IN ({})",
                random_ids
            )
            .as_str(),
        )
        .fetch_all(self.inner.as_mut().unwrap())
        .await?;
        trace!("requested size: {}, returning: {}", n, random_data.len());
        Ok(random_data)
    }

    fn size_hint(&self) -> usize {
        self.size
    }

    fn shared(&self) -> bool {
        true
    }

    fn to_json_str(&self) -> String {
        let file_name = self
            .file_name
            .rsplit('/')
            .next()
            .and_then(|s| s.strip_suffix(".sqlite"))
            .expect("RequestFile to enum conversion failed. File name should end with .sqlite")
            .to_string();
        let spec_enum = RequestSpecEnum::RequestFile(RequestFile::new(file_name));
        serde_json::to_string(&spec_enum).unwrap()
    }
}

fn default_request_file_size() -> usize {
    999
}

//todo use failure rate for adaptive qps control
//https://micrometer.io/docs/concepts#rate-aggregation

impl RequestGenerator {
    pub fn new(
        duration: u32,
        requests: ReqProvider,
        qps_scheme: Box<dyn QPSScheme + Send>,
    ) -> Self {
        let time_scale: u8 = 1;
        let total = duration * time_scale as u32;
        RequestGenerator {
            duration,
            time_scale,
            total,
            current_qps: 0,
            shared_provider: requests.shared(),
            req_provider: requests.to_json_str(),
            provider_or_future: ProviderOrFuture::Provider(requests),
            qps_scheme,
            current_count: 0,
        }
    }
}

impl Stream for RequestGenerator {
    type Item = (u32, AnyResult<Vec<HttpReq>>);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.current_count >= self.total {
            trace!("finished generating QPS");
            Poll::Ready(None)
        } else {
            let provider_or_future =
                std::mem::replace(&mut self.provider_or_future, ProviderOrFuture::Dummy);
            match provider_or_future {
                ProviderOrFuture::Provider(mut provider) => {
                    let qps = self.qps_scheme.next(self.current_count, None);
                    let len = provider.size_hint();
                    let req_size = min(qps as usize, min(len, MAX_REQ_RET_SIZE));
                    trace!("request size: {}, qps: {}, len: {}", req_size, qps, len);
                    let provider_and_fut = async move {
                        let requests = if req_size > 0 {
                            provider.get_n(req_size).await
                        } else {
                            Ok(vec![])
                        };
                        (provider, requests)
                    };
                    self.provider_or_future = ProviderOrFuture::Future(Box::pin(provider_and_fut));
                    self.current_qps = qps;
                    self.poll_next(cx)
                }
                ProviderOrFuture::Future(mut future) => match future.as_mut().poll(cx) {
                    Poll::Ready((provider, result)) => {
                        let qps = self.current_qps;
                        self.current_qps = 0;
                        self.current_count += 1;
                        self.provider_or_future = ProviderOrFuture::Provider(provider);
                        Poll::Ready(Some((qps, result)))
                    }
                    Poll::Pending => {
                        self.provider_or_future = ProviderOrFuture::Future(future);
                        Poll::Pending
                    }
                },
                ProviderOrFuture::Dummy => panic!("Something went wrong :("),
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.total as usize))
    }
}

#[allow(clippy::let_and_return)]
pub fn request_generator_stream(
    generator: RequestGenerator,
) -> impl Stream<Item = (u32, AnyResult<Vec<HttpReq>>)> {
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
pub(crate) mod test {
    use crate::generator::{
        request_generator_stream, ConstantQPS, RequestFile, RequestGenerator, RequestList,
        RequestProvider, MAX_REQ_RET_SIZE,
    };
    use crate::HttpReq;
    use std::collections::HashMap;
    use std::sync::Once;
    use std::time::Duration;
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;
    use tokio::time;
    use tokio_stream::StreamExt;
    use tokio_test::{assert_pending, assert_ready, task};
    use uuid::Uuid;

    static ONCE: Once = Once::new();

    fn setup() {
        ONCE.call_once(|| {
            tracing_subscriber::fmt()
                .with_env_filter("trace")
                .try_init()
                .unwrap();
        });
    }

    #[tokio::test]
    async fn test_request_generator_empty_req() {
        let mut generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(0)),
            Box::new(ConstantQPS { qps: 3 }),
        );
        let ret = generator.next().await;
        if let Some(arg) = ret {
            assert_eq!(arg.0, 3);
            assert_eq!(arg.1.unwrap().len(), 0);
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
        let mut generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(1)),
            Box::new(ConstantQPS { qps: 3 }),
        );
        let ret = generator.next().await;
        if let Some(arg) = ret {
            let requests = arg.1.unwrap();
            assert_eq!(arg.0, 3);
            assert_eq!(requests.len(), 1);
            if let Some(req) = requests.get(0) {
                assert_eq!(req.method, http::Method::GET);
            }
        } else {
            panic!()
        }
    }

    #[tokio::test]
    async fn test_request_generator_req_eq_qps() {
        let mut generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(4)),
            Box::new(ConstantQPS { qps: 3 }),
        );
        let ret = generator.next().await;
        if let Some(arg) = ret {
            assert_eq!(arg.0, 3);
            assert_eq!(arg.1.unwrap().len(), 3);
        } else {
            panic!()
        }
    }

    #[tokio::test]
    async fn test_request_generator_req_gt_max() {
        let mut generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(12)),
            Box::new(ConstantQPS { qps: 15 }),
        );
        let ret = generator.next().await;
        if let Some(arg) = ret {
            assert_eq!(arg.0, 15);
            assert_eq!(arg.1.unwrap().len(), MAX_REQ_RET_SIZE);
        } else {
            panic!()
        }
    }

    //noinspection Duplicates
    #[tokio::test]
    async fn test_request_generator_stream() {
        time::pause();
        let generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(1)),
            Box::new(ConstantQPS { qps: 3 }),
        );
        let throttle = request_generator_stream(generator);
        let mut throttle = task::spawn(throttle);
        let ret = throttle.poll_next();
        assert_ready!(ret);
        assert_pending!(throttle.poll_next());
        time::advance(Duration::from_millis(1001)).await;
        assert_ready!(throttle.poll_next());
    }

    pub fn req_list_with_n_req(n: usize) -> RequestList {
        let data = (0..n)
            .map(|_| {
                let r = rand::random::<u8>();
                HttpReq {
                    id: Uuid::new_v4().to_string(),
                    body: None,
                    url: format!("https://httpbin.org/anything/{}", r),
                    method: http::Method::GET,
                    headers: HashMap::new(),
                }
            })
            .collect::<Vec<_>>();
        RequestList { data }
    }

    #[tokio::test]
    async fn request_list_get_n() {
        let mut request_list = req_list_with_n_req(5);
        let result = request_list.get_n(2).await.unwrap();
        assert_eq!(2, result.len());
    }

    #[tokio::test]
    #[should_panic(expected = "shouldn't request data of 0 size")]
    async fn request_list_get_n_panic_on_0() {
        let mut request_list = req_list_with_n_req(5);
        let _result = request_list.get_n(0).await;
    }

    #[tokio::test]
    #[should_panic(expected = "shouldn't request data of 0 size")]
    async fn request_list_get_n_error() {
        let mut request_list = req_list_with_n_req(0);
        let result = request_list.get_n(0).await;
        assert!(result.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn request_file_get_n() {
        setup();
        if create_sqlite_file().await.is_ok() {
            let mut request = RequestFile {
                file_name: format!("{}/test-data.sqlite", std::env::var("PWD").unwrap()),
                inner: None,
                size: 0,
            };
            let vec = request.get_n(2).await.unwrap();
            assert_ne!(0, vec.len());
        }
    }

    async fn create_sqlite_file() -> anyhow::Result<()> {
        let sqlite_base64 =
            "U1FMaXRlIGZvcm1hdCAzABAAAgIAQCAgAAAAAgAAAAIAAAAAAAAAAAAAAAEAAAAEAAAAAAAAAAAA\
AAABAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACAC5PfA0AAAABD0sAD0sAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgTIBBxcdHQGCN3RhYmxlaHR0\
cF9yZXFodHRwX3JlcQJDUkVBVEUgVEFCTEUgaHR0cF9yZXEgKAogICAgICAgICAgICB1cmwgVEVY\
VCBOT1QgTlVMTCwKICAgICAgICAgICAgbWV0aG9kIFRFWFQgTk9UIE5VTEwsCiAgICAgICAgICAg\
IGJvZHkgQkxPQiwKICAgICAgICAgICAgaGVhZGVycyBURVhUCiAgICAgICAgICAgKQ0AAAAEDvgA\
D9YPrA85DvgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
AAAAAAA/BAU/EwBJaHR0cDovL2h0dHBiaW4ub3JnL2JlYXJlckdFVHsiQXV0aG9yaXphdGlvbiI6\
IkJlYXJlciAxMjMifXEDBUMVaklodHRwOi8vaHR0cGJpbi5vcmcvYW55dGhpbmdQT1NUeyJzb21l\
IjoicmFuZG9tIGRhdGEiLCJzZWNvbmQta2V5IjoibW9yZSBkYXRhIn17IkF1dGhvcml6YXRpb24i\
OiJCZWFyZXIgMTIzIn0oAgVJEwARaHR0cDovL2h0dHBiaW4ub3JnL2FueXRoaW5nLzEzR0VUe30o\
AQVJEwARaHR0cDovL2h0dHBiaW4ub3JnL2FueXRoaW5nLzExR0VUe30=";
        let mut file = File::create("test-data.sqlite").await?;
        file.write_all(base64::decode(sqlite_base64).unwrap().as_slice())
            .await?;
        Ok(())
    }
}
