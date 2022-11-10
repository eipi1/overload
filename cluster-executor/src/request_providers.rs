use anyhow::Result as AnyResult;
use datagen::generate_data;
use log::{debug, trace};
use overload_http::{HttpReq, RandomDataRequest, RequestFile, RequestList, RequestSpecEnum};
use remoc::rtc::async_trait;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::ConnectOptions;
use std::iter::repeat_with;
use std::str::FromStr;

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

    // /// Generate `n` requests and put into provided vec.
    // /// Returns number of requests generated successfully
    // async fn fill_vec(&mut self, vec: &mut Vec<HttpReq>, n: usize) -> AnyResult<usize>;

    /// number of total requests
    fn size_hint(&self) -> usize;

    /// The provider can be shared between instances in cluster. This will allow sending requests to
    /// secondary/worker instances without request data.
    fn shared(&self) -> bool;

    /// Not a good solution; it creates circular dependency, temporary hack, should find better solution
    #[deprecated]
    fn to_json_str(&self) -> String;
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

        let randomly_selected_data = repeat_with(|| fastrand::usize(0..self.data.len()))
            .take(n)
            .map(|i| self.data.get(i))
            .map(|r| r.cloned())
            .map(Option::unwrap)
            .collect::<Vec<_>>();
        Ok(randomly_selected_data)
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

#[async_trait]
impl RequestProvider for RequestFile {
    async fn get_n(&mut self, n: usize) -> AnyResult<Vec<HttpReq>> {
        if n == 0 {
            panic!("RequestFile: shouldn't request data of 0 size");
        }
        if self.inner.is_none() {
            //open sqlite connection
            let sqlite_file = format!("sqlite://{}", &self.file_name);
            debug!("Opening sqlite file at: {}", &sqlite_file);
            let mut connection = SqliteConnectOptions::from_str(sqlite_file.as_str())?
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
        random_ids.pop(); //remove last comma

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

#[async_trait]
impl RequestProvider for RandomDataRequest {
    async fn get_n(&mut self, n: usize) -> AnyResult<Vec<HttpReq>> {
        if !self.init {
            self.init = true;
            if self.uri_param_schema.is_some() {
                let match_indices = RandomDataRequest::find_param_positions(&self.url);
                self.url_param_pos = Some(match_indices);
            }
        }

        let mut requests = Vec::with_capacity(n);
        for _ in 0..n {
            let mut body = Option::None;
            let mut url = self.url.clone();
            if let Some(schema) = &self.uri_param_schema {
                if let Some(positions) = &self.url_param_pos {
                    let data = generate_data(schema);
                    RandomDataRequest::substitute_param_with_data(&mut url, positions, data)
                }
            }
            if matches!(self.method, http::Method::POST) {
                if let Some(schema) = &self.body_schema {
                    body = Some(Vec::from(generate_data(schema).to_string().as_bytes()));
                }
            }
            let req = HttpReq {
                id: "".to_string(),
                method: self.method.clone(),
                url,
                body,
                headers: self.headers.clone(),
            };
            requests.push(req);
        }
        Ok(requests)
    }

    fn size_hint(&self) -> usize {
        usize::MAX
    }

    fn shared(&self) -> bool {
        true
    }

    fn to_json_str(&self) -> String {
        serde_json::to_string(self).unwrap()
    }
}

#[cfg(test)]
pub(crate) mod test {
    use super::*;
    use crate::rate_spec::RateScheme;
    use crate::RequestGenerator;
    use datagen::data_schema_from_value;
    use http::Method;
    use overload_http::{ConstantRate, Linear, Scheme, Steps, Target};
    use regex::Regex;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::ops::Deref;
    use std::sync::Once;
    use std::time::Duration;
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;
    use tokio_stream::StreamExt;
    use uuid::Uuid;

    static ONCE: Once = Once::new();

    fn setup() {
        ONCE.call_once(|| {
            let _ = tracing_subscriber::fmt()
                .with_env_filter("trace")
                .try_init();
        });
    }

    #[test]
    fn serialize_steps_qps() {
        let json_str = r#"
        {
            "steps": [
              {
                "start": 0,
                "end": 5,
                "rate": 1
              },
              {
                "start": 9,
                "end": 10,
                "rate": 3
              },
              {
                "start": 6,
                "end": 8,
                "rate": 2
              }
            ]
          }
        "#;
        let mut steps: Steps = serde_json::from_str(json_str).unwrap();
        println!("{:?}", steps);
        assert_eq!(steps.next(0, None), 1);
        assert_eq!(steps.next(3, None), 1);
        assert_eq!(steps.next(5, None), 1);
        assert_eq!(steps.next(6, None), 2);
        assert_eq!(steps.next(7, None), 2);
        assert_eq!(steps.next(8, None), 2);
        assert_eq!(steps.next(9, None), 3);
        assert_eq!(steps.next(10, None), 3);
        assert_eq!(steps.next(11, None), 3);
        assert_eq!(steps.next(12, None), 3);
    }

    #[test]
    fn serialize_steps_qps_not_start_from_zero() {
        let json_str = r#"
        {
            "steps": [
              {
                "start": 1,
                "end": 5,
                "rate": 1
              },
              {
                "start": 9,
                "end": 10,
                "rate": 3
              },
              {
                "start": 6,
                "end": 8,
                "rate": 2
              }
            ]
          }
        "#;
        let steps = serde_json::from_str::<Steps>(json_str);
        assert!(steps.is_err());
        assert_eq!(
            steps.err().unwrap().to_string(),
            "Steps should start from 0, but starts from 1 instead."
        );
    }

    #[test]
    fn serialize_steps_qps_start_ge_end() {
        let json_str = r#"
        {
            "steps": [
              {
                "start": 0,
                "end": 5,
                "rate": 1
              },
              {
                "start": 9,
                "end": 9,
                "rate": 3
              },
              {
                "start": 6,
                "end": 8,
                "rate": 2
              }
            ]
          }
        "#;
        let steps = serde_json::from_str::<Steps>(json_str);
        assert!(steps.is_err());
        assert!(steps
            .err()
            .unwrap()
            .to_string()
            .starts_with("start(9) can not be greater than or equal to end(9)"));
    }

    #[tokio::test]
    async fn test_request_generator_empty_req() {
        let mut generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(0)),
            Box::new(ConstantRate { count_per_sec: 3 }),
            Target {
                host: "example.com".into(),
                port: 8080,
                protocol: Scheme::HTTP,
            },
            None,
            None,
        );
        let ret = generator.next().await;
        if let Some(arg) = ret {
            assert_eq!(arg.0, 3);
            assert_eq!(arg.1, 0);
        } else {
            panic!("fails");
        }
        //stream should generate 3 value
        assert!(generator.next().await.is_some());
        assert!(generator.next().await.is_some());
        assert!(generator.next().await.is_none());
    }

    #[tokio::test]
    async fn test_request_generator_req_lt_qps() {
        let mut generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(1)),
            Box::new(ConstantRate { count_per_sec: 3 }),
            Target {
                host: "example.com".into(),
                port: 8080,
                protocol: Scheme::HTTP,
            },
            None,
            None,
        );
        let ret = generator.next().await;
        if let Some(arg) = ret {
            assert_eq!(arg.0, 3);
            assert_eq!(arg.1, 0);
            // if let Some(req) = requests.get(0) {
            //     assert_eq!(req.method, http::Method::GET);
            // }
        } else {
            panic!()
        }
    }

    #[tokio::test]
    async fn test_request_generator_req_eq_qps() {
        let mut generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(4)),
            Box::new(ConstantRate { count_per_sec: 3 }),
            Target {
                host: "example.com".into(),
                port: 8080,
                protocol: Scheme::HTTP,
            },
            None,
            None,
        );
        let ret = generator.next().await;
        if let Some(arg) = ret {
            assert_eq!(arg.0, 3);
            assert_eq!(arg.1, 0);
        } else {
            panic!()
        }
    }

    #[tokio::test]
    async fn test_request_generator_req_gt_max() {
        let mut generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(12)),
            Box::new(ConstantRate { count_per_sec: 15 }),
            Target {
                host: "example.com".into(),
                port: 8080,
                protocol: Scheme::HTTP,
            },
            None,
            None,
        );
        let ret = generator.next().await;
        if let Some(arg) = ret {
            assert_eq!(arg.0, 15);
            assert_eq!(arg.1, 0);
        } else {
            panic!()
        }
    }

    //noinspection Duplicates
    #[tokio::test]
    async fn test_request_generator_concurrent_con_default() {
        let mut generator = RequestGenerator::new(
            3,
            Box::new(req_list_with_n_req(12)),
            Box::new(ConstantRate { count_per_sec: 3 }),
            Target {
                host: "example.com".into(),
                port: 8080,
                protocol: Scheme::HTTP,
            },
            None,
            None,
        );
        let arg = generator.next().await.unwrap();
        assert_eq!(arg.0, 3);
        // assert_eq!(arg.1.unwrap().len(), 3);
        assert_eq!(arg.1, 0);
        tokio::time::pause();
        let arg = generator.next().await.unwrap();
        tokio::time::advance(Duration::from_millis(1001)).await;
        assert_eq!(arg.0, 3);
        // assert_eq!(arg.1.unwrap().len(), 3);
        assert_eq!(arg.1, 0);
        let arg = generator.next().await.unwrap();
        tokio::time::advance(Duration::from_millis(1001)).await;
        assert_eq!(arg.0, 3);
        // assert_eq!(arg.1.unwrap().len(), 3);
        assert_eq!(arg.1, 0);
        let arg = generator.next().await;
        tokio::time::advance(Duration::from_millis(1001)).await;
        assert!(arg.is_none())
    }

    //noinspection Duplicates
    #[tokio::test]
    async fn test_request_generator_concurrent_con_linear() {
        let mut generator = RequestGenerator::new(
            30,
            Box::new(req_list_with_n_req(12)),
            Box::new(ConstantRate { count_per_sec: 3 }),
            Target {
                host: "example.com".into(),
                port: 8080,
                protocol: Scheme::HTTP,
            },
            Some(Box::new(Linear {
                a: 0.5,
                b: 1,
                max: 10,
            })),
            None,
        );
        let arg = generator.next().await.unwrap();
        //assert first element
        assert_eq!(arg.0, 3);
        // assert_eq!(arg.1.unwrap().len(), 3);
        assert_eq!(arg.1, 1);
        tokio::time::pause();

        let _arg = generator.next().await.unwrap();
        tokio::time::advance(Duration::from_millis(1001)).await;

        let arg = generator.next().await.unwrap();
        tokio::time::advance(Duration::from_millis(1001)).await;
        // assert third element
        assert_eq!(arg.0, 3);
        // assert_eq!(arg.1.unwrap().len(), 3);
        assert_eq!(arg.1, 2);

        for _ in 0..26 {
            let arg = generator.next().await;
            tokio::time::advance(Duration::from_millis(1001)).await;
            assert!(arg.is_some());
        }
        let arg = generator.next().await.unwrap();
        tokio::time::advance(Duration::from_millis(1001)).await;
        assert_eq!(arg.0, 3);
        // assert_eq!(arg.1.unwrap().len(), 3);
        assert_eq!(arg.1, 10);
        let arg = generator.next().await;
        tokio::time::advance(Duration::from_millis(1001)).await;
        assert!(arg.is_none());
    }

    //noinspection Duplicates
    // #[tokio::test]
    // async fn test_request_generator_stream() {
    //     time::pause();
    //     let generator = RequestGenerator::new(
    //         3,
    //         Box::new(req_list_with_n_req(1)),
    //         Box::new(ConstantRate { count_per_sec: 3 }),
    //         Target {
    //             host: "example.com".into(),
    //             port: 8080,
    //             protocol: Scheme::HTTP,
    //         },
    //         None,
    //         None,
    //     );
    //     let throttle = request_generator_stream(generator);
    //     let mut throttle = task::spawn(throttle);
    //     let ret = throttle.poll_next();
    //     assert_ready!(ret);
    //     assert_pending!(throttle.poll_next());
    //     time::advance(Duration::from_millis(1001)).await;
    //     assert_ready!(throttle.poll_next());
    // }

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
                file_name: format!(
                    "{}/test-data.sqlite",
                    std::env::var("CARGO_MANIFEST_DIR").unwrap()
                ),
                inner: None,
                size: 0,
            };
            let vec = request.get_n(2).await.unwrap();
            assert_ne!(0, vec.len());
        }
        let _ = tokio::fs::remove_file("test-data.sqlite").await;
    }

    #[test]
    #[should_panic]
    fn random_data_generator_json_test_invalid() {
        setup();
        //bodySchema.properties.age.maximum has invalid int value
        serde_json::from_str::<RandomDataRequest>(r#"{"url":"http://localhost:2080/anything/{param1}/{param2}","method":"GET","bodySchema":{"$id":"https://example.com/person.schema.json","$schema":"https://json-schema.org/draft/2020-12/schema","title":"Person","type":"object","properties":{"firstName":{"type":"string","description":"The person's first name."},"lastName":{"type":"string","description":"The person's last name."},"age":{"description":"Age in years which must be equal to or greater than zero.","type":"integer","minimum":1,"maximum":"100"}}},"uriParamSchema":{"type":"object","properties":{"param1":{"type":"string","description":"The person's first name.","minLength":6,"maxLength":15},"param2":{"description":"Age in years which must be equal to or greater than zero.","type":"integer","minimum":1000000,"maximum":1100000}}}}"#
        ).unwrap();
    }

    #[test]
    fn random_data_generator_json_test() {
        setup();
        //valid, will not panic
        serde_json::from_str::<RandomDataRequest>(r#"{"url":"http://localhost:2080/anything/{param1}/{param2}","method":"GET","bodySchema":{"$id":"https://example.com/person.schema.json","$schema":"https://json-schema.org/draft/2020-12/schema","title":"Person","type":"object","properties":{"firstName":{"type":"string","description":"The person's first name."},"lastName":{"type":"string","description":"The person's last name."},"age":{"description":"Age in years which must be equal to or greater than zero.","type":"integer","minimum":1,"maximum":100}}},"uriParamSchema":{"type":"object","properties":{"param1":{"type":"string","description":"The person's first name.","minLength":6,"maxLength":15},"param2":{"description":"Age in years which must be equal to or greater than zero.","type":"integer","minimum":1000000,"maximum":1100000}}}}"#
        ).unwrap();
    }

    #[test]
    fn find_param_positions_test() {
        let url = String::from("http://localhost:2080/anything/{param1}/{param2}");
        let params = RandomDataRequest::find_param_positions(&url);
        assert_eq!(params.len(), 2);
        for (i, param) in params.iter().enumerate() {
            if i == 0 {
                assert_eq!(param.start, 40);
            } else {
                assert_eq!(param.start, 31);
            }
        }
    }

    #[test]
    fn substitution_param_test() {
        let url = String::from("http://localhost:2080/anything/{param1}/{p2}");
        let url_schema:Value = serde_json::from_str(
            r#"{"type":"object","properties":{"param1":{"type":"string","description":"The person's first name.","minLength":1,"maxLength":10},"p2":{"description":"Age in years which must be equal to or greater than zero.","type":"integer","minimum":1,"maximum":1000}}}"#
        ).unwrap();
        let regex = Regex::new("http://localhost:2080/anything/[a-zA-Z]{1,10}/[0-9]{1,4}").unwrap();
        let schema = data_schema_from_value(&url_schema).unwrap();
        let params = RandomDataRequest::find_param_positions(&url);
        for _ in 0..5 {
            let mut url_ = url.clone();
            let data = generate_data(&schema);
            RandomDataRequest::substitute_param_with_data(&mut url_, &params, data);
            assert!(regex.is_match(&url_));
        }
    }

    #[tokio::test]
    async fn random_data_generator() {
        setup();
        let body_schema = serde_json::from_str(
            r#"{"$id":"https://example.com/person.schema.json","$schema":"https://json-schema.org/draft/2020-12/schema","title":"Person","type":"object","properties":{"firstName":{"type":"string","description":"The person's first name."},"lastName":{"type":"string","description":"The person's last name."},"age":{"description":"Age in years which must be equal to or greater than zero.","type":"integer","minimum":0,"maximum":100}}}"#
        ).unwrap();
        let url_schema = serde_json::from_str(
            r#"{"type":"object","properties":{"param1":{"type":"string","description":"The person's first name.","minLength":1,"maxLength":5},"p2":{"description":"Age in years which must be equal to or greater than zero.","type":"integer","minimum":0,"maximum":100}}}"#
        ).unwrap();
        let mut generator = RandomDataRequest {
            init: false,
            method: Method::POST,
            url: "http://example.com/{param1}/{p2}".to_string(),
            url_param_pos: None,
            headers: Default::default(),
            body_schema: Some(body_schema),
            uri_param_schema: Some(url_schema),
        };
        let requests = generator.get_n(5).await;
        assert!(requests.is_ok());
        for req in requests.unwrap().iter() {
            let result: Value = serde_json::from_slice(req.body.as_ref().unwrap().deref()).unwrap();
            let age = result.get("age").unwrap().as_i64().unwrap();
            more_asserts::assert_le!(age, 100);
            more_asserts::assert_ge!(age, 0);
        }

        let body_schema = serde_json::from_str(
            r#"{"$id":"https://example.com/person.schema.json","$schema":"https://json-schema.org/draft/2020-12/schema","title":"Person","type":"object","properties":{"firstName":{"type":"string","description":"The person's first name."},"lastName":{"type":"string","description":"The person's last name."},"age":{"description":"Age in years which must be equal to or greater than zero.","type":"integer","minimum":0,"maximum":100}}}"#
        ).unwrap();
        let url_schema = serde_json:: from_str(
            r#"{"type":"object","properties":{"param1":{"type":"string","description":"The person's first name.","minLength":6,"maxLength":15},"p2":{"description":"Age in years which must be equal to or greater than zero.","type":"integer","minimum":1000000,"maximum":1100000}}}"#
        ).unwrap();
        let mut request = RandomDataRequest {
            init: false,
            method: Method::POST,
            url: "http://example.com/{param1}/{p2}".to_string(),
            url_param_pos: None,
            headers: Default::default(),
            body_schema: Some(body_schema),
            uri_param_schema: Some(url_schema),
        };
        let requests = request.get_n(5).await;
        assert!(requests.is_ok());
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
