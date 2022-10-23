//! A distributed load testing utility written in Rust
//!
//! ![Rust](https://github.com/eipi1/overload/actions/workflows/rust.yml/badge.svg)
//! [![Docker hub][dockerhub-badge]][dockerhub-url]
//! [![MIT licensed][mit-badge]][mit-url]
//! [![Github][github-badge]][github-rep]
//!
//! [dockerhub-badge]: https://github.com/eipi1/overload/actions/workflows/dockerhub-push.yml/badge.svg
//!
//! [dockerhub-url]: https://hub.docker.com/r/mdsarowar/overload
//!
//! [mit-badge]: https://img.shields.io/badge/license-MIT-blue.svg
//!
//! [mit-url]: https://github.com/tokio-rs/tokio/blob/master/LICENSE
//!
//! [github-badge]: https://img.shields.io/badge/github-eipi1/overload-brightgreen
//!
//! [github-rep]: https://github.com/eipi1/overload
//!
//! * [Getting Started](#getting-started)
//! * [Environment variables](#environment-variables)
//! * [APIs](#apis)
//! * [Monitoring](#monitoring)
//!
//! # Getting started
//! ## Docker
//! ```shell
//! docker run -p 3030:3030 ghcr.io/eipi1/overload:latest-standalone-snapshot
//! ```
//!
//! ## Kubernetes
//! ```yaml
//! apiVersion: apps/v1
//! kind: Deployment
//! metadata:
//!   labels:
//!     app: overload
//!   name: overload
//!   namespace: default
//! spec:
//!   replicas: 1
//!   selector:
//!     matchLabels:
//!       app: overload
//!   template:
//!     metadata:
//!       labels:
//!         app: overload
//!     spec:
//!       containers:
//!         - image: ghcr.io/eipi1/overload:latest-standalone-snapshot
//!           imagePullPolicy: "Always"
//!           name: overload
//!           ports:
//!             - containerPort: 3030
//!           env:
//!             - name: LOG_LEVEL
//!               value: "info"
//!             - name: DATA_DIR
//!               value: "/tmp"
//! ---
//! apiVersion: v1
//! kind: Service
//! metadata:
//!   name: overload
//! spec:
//!   #  type: LoadBalancer
//!   selector:
//!     app: overload
//!   ports:
//!     - protocol: TCP
//!       port: 3030
//!       targetPort: 3030
//! ```
//!
//! ## Start Test
//! The following request will send two `GET` request per second(`"countPerSec": 2`) to `httpbin.org/get` for 120
//! seconds(`"duration": 60`).
//! ```shell
//! curl --location --request POST 'localhost:3030/test' \
//! --header 'Content-Type: application/json' \
//! --data-raw '<json_request_body>'
//! ```
//!
//! Sample JSON request body -
//!
//! ```rust
//! # use overload::http_util::request::Request;
//! # let req = r###"
//! {
//!   "duration": 120,
//!   "name": "demo-test",
//!   "qps": {
//!     "ConstantRate": {
//!       "countPerSec": 1
//!     }
//!   },
//!   "req": {
//!     "RequestList": {
//!       "data": [
//!         {
//!           "body": null,
//!           "method": "GET",
//!           "url": "/get"
//!         }
//!       ]
//!     }
//!   },
//!   "target": {
//!     "host": "httpbin.org",
//!     "port": 80,
//!     "protocol": "HTTP"
//!   }
//! }
//! # "###;
//! # let result = serde_json::from_str::<Request>(req);
//! # assert!(result.is_ok());
//! ```
//!
//!
//! It'll respond with a job identifier and status.
//! ```json
//! {
//!   "job_id": "demo-test-d2ae5ff0-7bf4-4daf-8784-83b642d7dd6b",
//!   "status": "Starting"
//! }
//! ```
//! We'll need the `job_id` if we want to stop the test later.
//!
//! ## Get job status
//! ```shell
//! curl --location --request GET 'localhost:3030/test/status/'
//! ```
//! ## Stop a job
//! ```shell
//! curl --location --request GET 'localhost:3030/test/stop/demo-test-d2ae5ff0-7bf4-4daf-8784-83b642d7dd6b'
//! ```
//!
//! # Modes
//!
//! Overload support two modes - cluster & standalone. Overload exposes a set of [APIs](#apis)
//! at port `3030` to manage tests.
//!
//! ## Cluster
//!
//! Cluster mode allows the application run in primary/secondary mode, but all the complexity should be transparent to the
//! users. Currently, only supported on Kubernetes.
//!
//! There are a few constraints/limitations is in place -
//!
//! * Shared storage between all nodes for tests using csv to work properly
//! * Minimum required node: 3
//! * Maximum nodes: 20
//! * Requires container image tagged as *{version}-cluster*
//!
//! Repository provides a sample *[deployment.yaml][deployment-yaml]* file. Addition to that the
//! application also needs "get", "list" permission for "pods", "endpoints" for discovery.
//!
//! ## Standalone
//!
//! Runs the application in simple single instance mode. Requires container images tagged as *{version}-standalone.
//!
//! For example to run the latest snapshot
//!
//! ```shell
//! docker run -p 3030:3030 mdsarowar/overload:latest-standalone-snapshot
//! ```
//!
//! # Environment variables
//!
//! | variable           | desc                                                                            | default  |
//! |--------------------|---------------------------------------------------------------------------------|----------|
//! | RUST_LOG           | application log level                                                           | info     |
//! | DATA_DIR           | path to store uploaded CSV, should be shared among all instance in cluster mode | /tmp     |
//! | K8S_ENDPOINT_NAME  | name of the [endpoints][endpoint-api] (cluster mode only)                       | overload |
//! | K8S_NAMESPACE_NAME | kubernetes namespace                                                            | default  |
//!
//! # APIs
//!
//! ## Request
//! Test specification
//!
//! | Field                | Required | Default                       | Data type                                                     | Description                                                                                |
//! |----------------------|----------|-------------------------------|---------------------------------------------------------------|--------------------------------------------------------------------------------------------|
//! | name                 | ❎        | UUID                          | String                                                        | Test name, application will append UUID to ensure unique identifier                        |
//! | duration             | ✅        |                               | uint32                                                        | Test duration                                                                              |
//! | target               | ✅        |                               | [Target](#target)                                             | Target details                                                                             |
//! | req                  | ✅        |                               | [RequestProvider](#requestprovider)                           | Request provider Spec                                                                      |
//! | qps                  | ✅        |                               | [QPSSpec](#qpsspec)                                           | Request per second specification                                                           |
//! | concurrentConnection | ❎        | Elastic                       | [ConcurrentConnectionRateSpec](#concurrentconnectionratespec) | Concurrent number of requests to use to send request                                       |
//! | histogramBuckets     | ❎        | [20, 50, 100, 300, 700, 1100] | [uint16]                                                      | Prometheus histogram buckets. For details https://prometheus.io/docs/practices/histograms/ |
//!
//! ```shell
//! curl --location --request POST 'localhost:3030/test' \
//! --header 'Content-Type: application/json' \
//! --data-raw '<json_request_body>'
//! ```
//!
//! Sample JSON request body -
//!
//! ```rust
//! # use overload::http_util::request::Request;
//! # let req = r###"
//! {
//!   "duration": 120,
//!   "name": "demo-test",
//!   "qps": {
//!     "ConstantRate": {
//!       "countPerSec": 1
//!     }
//!   },
//!   "req": {
//!     "RequestList": {
//!       "data": [
//!         {
//!           "body": null,
//!           "method": "GET",
//!           "url": "/get"
//!         }
//!       ]
//!     }
//!   },
//!   "target": {
//!     "host": "httpbin.org",
//!     "port": 80,
//!     "protocol": "HTTP"
//!   },
//!   "histogramBuckets": [35,40,45,48,50, 52]
//! }
//! # "###;
//! # let result = serde_json::from_str::<Request>(req);
//! # assert!(result.is_ok());
//! ```
//!
//! This will run the test for 120 seconds with a linear increase in request per seconds(RPS).
//!
//! ## Response
//!
//! | field  | Description        | data type               |
//! |--------|--------------------|-------------------------|
//! | job_id | Test ID            | UUID                    |
//! | status | status of the test | [JobStatus](#jobstatus) |
//!
//! ```json
//! {
//!   "job_id": "1b826ded-34d5-434c-b983-a1ba46ffff82",
//!   "status": "Starting"
//! }
//! ```
//!
//! ## Target
//! Specify target details
//!
//! | field    | Required | Description                        | data type |
//! |----------|----------|------------------------------------|-----------|
//! | host     | ✅        | Target domain name or IP           | Domain    |
//! | port     | ✅        | Target port                        | uint16    |
//! | protocol | ✅        | Target protocol, support http only | "HTTP"    |
//!
//! ## RequestProvider
//!
//! Currently, supports the following providers
//!
//! ### RequestList
//!
//! An unordered set of [HttpReq](#httpreq)
//!
//! | field | Description      | data type             |
//! |-------|------------------|-----------------------|
//! | data  | Array of HttpReq | [[HttpReq](#httpreq)] |
//!
//! ### RequestFile
//!
//! Get request data from a file. File have to be [uploaded](#upload-request-data-file) before the test.
//! Generator will pick requests randomly.
//!
//! | field     | Description             | data type |
//! |-----------|-------------------------|-----------|
//! | file_name | ID of the uploaded file | UUID      |
//!
//! #### Example
//!
//! <details>
//!   <summary>Example Request</summary>
//!
//! ```rust
//! # use overload::http_util::request::Request;
//! # let req = r###"
//! {
//!   "duration": 3,
//!   "req": {
//!     "RequestFile": {
//!       "file_name": "4e1d1b32-0f1e-4b31-92dd-66f51c5acf9a"
//!     }
//!   },
//!   "qps": {
//!     "ConstantRate": {
//!       "countPerSec": 8
//!     }
//!   },
//!   "target": {
//!     "host": "httpbin.org",
//!     "port": 80,
//!     "protocol": "HTTP"
//!   }
//! }
//! # "###;
//! # let result = serde_json::from_str::<Request>(req);
//! # assert!(result.is_ok());
//! ```
//!
//! </details>
//!
//! ### RandomDataRequest
//!
//! Generate request with random data based on constraints, can be specified using JSON Schema like syntax.
//!
//! | field          | Required | Description                                                                                                                                       | data type           |
//! |----------------|----------|---------------------------------------------------------------------------------------------------------------------------------------------------|---------------------|
//! | method         | Yes      | HTTP method                                                                                                                                       | Enum ("POST","GET") |
//! | url            | Yes      | Request Url, optionally supports param substitution. Param name should be of format `{[a-z0-9]+}`, e.g. http://httpbin.org/anything/{param1}/{p2} | string              |
//! | headers        | No       | HTTP request header                                                                                                                               | Map<String,String>  |
//! | bodySchema     | No       | Request body spec to be used for random data generation                                                                                           | JSON Schema         |
//! | uriParamSchema | No       | Url param spec to be used for random data generation                                                                                              | JSON Schema         |
//!
//! ### Supported JSON Schema spec
//!
//! | type    | supported |
//! |---------|-----------|
//! | string  | ✅         |
//! | integer | ✅         |
//! | object  | ✅         |
//! | array   | ❎         |
//! | boolean | ❎         |
//! | null    | ❎         |
//!
//! | Constraints | Supported | Note                                                                                                                                                       |
//! |-------------|-----------|------------------------------------------------------------------------------------------------------------------------------------------------------------|
//! | minLength   | ✅         |                                                                                                                                                            |
//! | maxLength   | ✅         |                                                                                                                                                            |
//! | minimum     | ✅         |                                                                                                                                                            |
//! | maximum     | ✅         |                                                                                                                                                            |
//! | constant    | ✅         |                                                                                                                                                            |
//! | pattern     | ✅         | Unicode pattern. Careful with character groups, e.g. `\d` represents digits from english, arabic and other languages. Maximum repeat(`*`,`+`) length is 10 |
//! | format      | ❎         |                                                                                                                                                            |
//!
//! <details>
//! <summary> Example </summary>
//!
//! ```rust
//! # use overload::http_util::request::Request;
//! # let req = r###"
//! {
//!   "duration": 10,
//!   "req": {
//!     "RandomDataRequest": {
//!       "url": "/anything/{param1}/{param2}",
//!       "method": "GET",
//!       "bodySchema": {
//!         "title": "Person",
//!         "type": "object",
//!         "properties": {
//!           "firstName": {
//!             "type": "string",
//!             "description": "The person's first name."
//!           },
//!           "lastName": {
//!             "type": "string",
//!             "description": "The person's last name."
//!           },
//!           "age": {
//!             "description": "Age in years which must be equal to or greater than zero.",
//!             "type": "integer",
//!             "minimum": 1,
//!             "maximum": 100
//!           }
//!         }
//!       },
//!       "uriParamSchema": {
//!         "type": "object",
//!         "properties": {
//!           "param1": {
//!             "type": "string",
//!             "description": "The person's first name.",
//!             "minLength": 6,
//!             "maxLength": 15
//!           },
//!           "param2": {
//!             "description": "Age in years which must be equal to or greater than zero.",
//!             "type": "integer",
//!             "minimum": 1000000,
//!             "maximum": 1100000
//!           }
//!         }
//!       }
//!     }
//!   },
//!   "qps": {
//!     "ConstantRate": {
//!       "countPerSec": 5
//!     }
//!   },
//!   "target": {
//!     "host": "httpbin.org",
//!     "port": 80,
//!     "protocol": "HTTP"
//!   }
//! }
//! # "###;
//! # let result = serde_json::from_str::<Request>(req);
//! # assert!(result.is_ok());
//! ```
//!
//! </details>
//!
//! ### HttpReq
//!
//! | field   | Description         | data type          |
//! |---------|---------------------|--------------------|
//! | method  | HTTP method         | Enum(POST,GET)     |
//! | url     | valid url           | string             |
//! | body    | Body for POST       | string             |
//! | headers | HTTP request header | Map<String,String> |
//!
//! ## QPSSpec
//!
//! Currently, supported configurations are - [ConstantRate](#constantrate), [Linear](#linear), [ArraySpec](#arrayspec),
//! [Steps/Staircase](#stepsstaircase-qps).
//!
//! ## ConcurrentConnectionRateSpec
//!
//! ### Elastic
//! The default configuration, if nothing specified in the request, this will be used.
//!
//! Other supported configurations are - [ConstantRate](#constantrate), [Linear](#linear), [ArraySpec](#arrayspec),
//! [Steps/Staircase](#stepsstaircase-qps).
//!
//! ## Rate configurations
//!
//! ### ConstantRate
//!
//! | field         | data type | Description                                              |
//! |---------------|-----------|----------------------------------------------------------|
//! | countPerSec   | uint32    | Constant rate is maintain for the `duration` of the test |
//!
//! ### Linear
//!
//! Increase rate(QPS/ConnPerSec) linearly; rate for any time is calculated using eq. rate(QPS/ConnectionPS) = ax + b,
//! where x being duration(in seconds) passed since the start of the test.
//!
//! | field | data type | Description       |
//! |-------|-----------|-------------------|
//! | a     | float     | Slope of the line |
//! | b     | uint32    | Intercept         |
//! | max   | uint32    | max QPS           |
//!
//! #### Example
//!
//! ```rust
//! # use overload::http_util::request::RateSpec;
//! # let req = r###"
//! {
//!   "Linear": {
//!     "a": 2,
//!     "b": 1,
//!     "max": 12
//!   }
//! }
//! # "###;
//! # let result = serde_json::from_str::<RateSpec>(req);
//! # assert!(result.is_ok());
//! ```
//!
//! If a test runs for 10 seconds, the generated RPS will be [1, 5, 7, 9, 11, 12, 12, 12, 12, 12]
//!
//! ### ArraySpec
//!
//! Specify QPS directly for a certain period of time, not recommended to use.
//!
//! | field       | data type | Description    |
//! |-------------|-----------|----------------|
//! | countPerSec | [uint32]  | Array of rates |
//!
//! #### Example
//!
//! ```rust
//! # use overload::http_util::request::RateSpec;
//! # let req = r###"
//! {
//!   "ArraySpec": {
//!     "countPerSec": [1, 4, 6, 10, 20, 10]
//!   }
//! }
//! # "###;
//! # let result = serde_json::from_str::<RateSpec>(req);
//! # assert!(result.is_ok());
//! ```
//!
//! If a test runs for 10 seconds, generated RPS will be 1 on the first second, 4 on second, 6 on third seconds and
//! 10, 20, 10 on 4, 5, 6th second. It'll restart from 0 position once reaches the end of the array, so on the 7th seconds,
//! QPS will go down back to 1 and 4, 6, 10 on 8, 9, 10th seconds, respectively.
//!
//! ### Steps/Staircase QPS
//!
//! Specify RPS in steps
//!
//! | field | Description   | data type     |
//! |-------|---------------|---------------|
//! | steps | Array of Step | [Step](#step) |
//!
//! #### Step
//!
//! | field | Description          | data type |
//! |-------|----------------------|-----------|
//! | start | Start of the step    | [uint32]  |
//! | end   | end of the step      | [uint32]  |
//! | qps   | QPS during this step | [uint32]  |
//!
//! #### Example
//!
//! ```rust
//! # use overload::http_util::request::RateSpec;
//! # let req = r###"
//! {
//!   "Steps" : {
//!     "steps": [
//!       {
//!         "start": 0,
//!         "end": 5,
//!         "rate": 1
//!       },
//!       {
//!         "start": 6,
//!         "end": 8,
//!         "rate": 4
//!       },
//!       {
//!         "start": 9,
//!         "end": 10,
//!         "rate": 7
//!       }
//!     ]
//!   }
//! }
//! # "###;
//! # let result = serde_json::from_str::<RateSpec>(req);
//! # assert!(result.is_ok());
//! ```
//!
//! If the test run for 15 seconds, from 0 to 5th seconds, generated QPS is 1, from 6th to 8th seconds,
//! generated QPS is 4, from 9 to 10th and 11th to 15th seconds, generated QPS is 7
//!
//! ## Upload Request Data File
//!
//! Currently, supports CSV file only.
//!
//! Curl sample
//!
//! ```shell
//! curl --location --request POST 'overload.host:3030/test/requests-bin' \
//! --header 'Content-Type: text/csv' \
//! --data-binary '@/path/to/requests.csv'
//! ```
//!
//! ### CSV format
//!
//! ```csv
//! "url","method","body","headers"
//! "http://httpbin.org/anything/11","GET","","{}"
//! "http://httpbin.org/anything/13","GET","","{}"
//! "http://httpbin.org/anything","POST","{\"some\":\"random data\",\"second-key\":\"more data\"}","{\"Authorization\":\"Bearer 123\"}"
//! "http://httpbin.org/bearer","GET","","{\"Authorization\":\"Bearer 123\"}"
//! ```
//!
//! ### Response
//!
//! API returns valid count, i.e. count of requests that has been parsed successfully and a file ID. File ID will be
//! required to for testing.
//!
//! | field       | Description                      | data type |
//! |-------------|----------------------------------|-----------|
//! | valid_count | number of valid requests in file | uint32    |
//! | file        | ID of the file                   | UUID      |
//!
//! ## JobStatus
//!
//! Enum stating the current status of the test
//!
//! | value            | Description                                            |
//! |------------------|--------------------------------------------------------|
//! | Starting         | Job starting or submitted to the queue                 |
//! | InProgress       | Job is running                                         |
//! | Stopped          | Job stopped by User                                    |
//! | Completed        | Job Done                                               |
//! | Failed           | Some error happened, Couldn't finish executing the job |
//! | Error(ErrorCode) | Other error                                            |
//!
//! ## Get Job Status
//!
//! Returns status of all jobs.
//!
//! **Limitation -**
//!
//! * Keep status only for 10 minutes, will be cleaned up after that
//! * No way to get status by job id
//! * Doesn't maintain any kind of sorting
//!
//! ### Request
//!
//! | Spec   | Value        |
//! |--------|--------------|
//! | Path   | /test/status |
//! | Method | GET          |
//!
//! ### Query Params
//!
//! | field  | Description       | data type |
//! |--------|-------------------|-----------|
//! | offset | start of the page | uint32    |
//! | limit  | size of the page  | uint32    |
//!
//! ### Response
//!
//! | field    | Description                                          | data type               |
//! |----------|------------------------------------------------------|-------------------------|
//! | {job_id} | Test job identifier received when test was submitted | string                  |
//! | {status} | current status of the test job                       | [JobStatus](#jobstatus) |
//!
//! ### Example
//!
//! ```http
//! GET /test/status?offset=1&limit =1 HTTP/1.1
//! Host: localhost:3030
//! ```
//!
//! ```json
//! {
//!   "60de342e-b18c-4837-80d2-a2c71c1985f8": "Completed"
//! }
//! ```
//!
//! ## Stop a job
//!
//! ### Request
//!
//! | Spec   | Value                 |
//! |--------|-----------------------|
//! | Path   | /test/stop/{job_id} |
//! | Method | GET                   |
//!
//! ### Request Params
//!
//! | field  | Description                      | data type |
//! |--------|----------------------------------|-----------|
//! | job_id | id of the test job to be stopped | string    |
//!
//!
//! # Response Assertion
//! A set of assertion can be passed to verify response received from API in test. Assertion will fail if response can't
//! be parsed as JSON.
//!
//! For example, the following request will send request to httpbin.org/get and verify response is a json and
//! the value at `$.headers.Host` (JsonPath) is `httpbin.org`.
//!
//! Failed assertion emits an info log and can also be monitored through metrics.
//!
//! ```rust
//! # use overload::http_util::request::Request;
//! # let req = r###"
//! {
//!   "duration": 120,
//!   "name": "demo-test",
//!   "qps": {
//!     "ConstantRate": {
//!       "countPerSec": 1
//!     }
//!   },
//!   "req": {
//!     "RequestList": {
//!       "data": [
//!         {
//!           "body": null,
//!           "method": "GET",
//!           "url": "/get"
//!         }
//!       ]
//!     }
//!   },
//!   "target": {
//!     "host": "httpbin.org",
//!     "port": 80,
//!     "protocol": "HTTP"
//!   },
//!   "responseAssertion": {
//!       "assertions": [
//!         {
//!           "id": 1,
//!           "expectation": {
//!             "Constant": "httpbin.org"
//!           },
//!           "actual": {
//!             "FromJsonResponse": {
//!               "path": "$.headers.Host"
//!             }
//!           }
//!         }
//!       ]
//!     }
//! }
//! # "###;
//! # let result = serde_json::from_str::<Request>(req);
//! # assert!(result.is_ok());
//! ```
//!
//! ## ResponseAssertion
//!
//! | field      | Description                | data type               |
//! |------------|----------------------------|-------------------------|
//! | assertions | List of assertion criteria | [Assertion](#assertion) |
//!
//! ### Assertion
//! | field       | Description                                          | data type                   |
//! |-------------|------------------------------------------------------|-----------------------------|
//! | id          | assertion identifier, used in metrics                | uint32                      |
//! | expectation | Expected value, constant or derived from the request | [Expectation](#expectation) |
//! | actual      | Actual value, constant or derived from the response  | [Actual](#actual)           |
//!
//! #### Expectation
//! Following expectations are supported -
//!
//! ##### Constant
//! Number, String or Json value, expect a constant value for all the request.
//!
//! ```rust
//! # use response_assert::Expectation;
//! # let req = r###"
//! {
//!   "Constant": {
//!     "hello": "world"
//!   }
//! }
//! # "###;
//! # let result = serde_json::from_str::<Expectation>(req);
//! # assert!(result.is_ok());
//! ```
//! ##### RequestPath
//! Derive from request path by provided segment number
//! ```rust
//! # use response_assert::Expectation;
//! # let req = r###"
//! {
//!    "RequestPath": 1
//! }
//! # "###;
//! # let result = serde_json::from_str::<Expectation>(req);
//! # assert!(result.is_ok());
//! ```
//!
//! ##### RequestQueryParam
//! Use the value of provided request query param as expectation
//! ```rust
//! # use response_assert::Expectation;
//! # let req = r###"
//! {
//!   "RequestQueryParam": "hello"
//! }
//! # "###;
//! # let result = serde_json::from_str::<Expectation>(req);
//! # assert!(result.is_ok());
//! ```
//!
//! #### Actual
//! Following actual sources are supported -
//!
//! ##### Constant
//! Number, String or JSON value, expect a constant value for all the request.
//!
//! ```rust
//! # use response_assert::Actual;
//! # let req = r###"
//! {
//!   "Constant": {
//!     "hello": "world"
//!   }
//! }
//! # "###;
//! # let result = serde_json::from_str::<Actual>(req);
//! # assert!(result.is_ok());
//! ```
//!
//! ##### FromJsonResponse
//! Derive expectation from JSON response using JsonPath
//! ```rust
//! # use response_assert::Actual;
//! # let req = r###"
//! {
//!   "FromJsonResponse": {
//!     "path": "$.hello"
//!   }
//! }
//! # "###;
//! # let result = serde_json::from_str::<Actual>(req);
//! # assert!(result.is_ok());
//! ```
//!
//! # Monitoring
//! The application comes with Prometheus support for monitoring. Metrics are exposed at `/metrics` endpoint.
//!
//! <details>
//! <summary>Sample Prometheus scraper config</summary>
//!
//! ```yaml
//! - job_name: overload-k8s
//!   honor_timestamps: true
//!   scrape_interval: 5s
//!   scrape_timeout: 1s
//!   metrics_path: /metrics
//!   scheme: http
//!   kubernetes_sd_configs:
//!   - api_server: <k8s-api-server>
//!     role: endpoints
//!     namespaces:
//!       names:
//!       - default
//!     selectors:
//!     - role: endpoints
//!       field: metadata.name=overload
//! ```
//! </details>
//!
//! ## Histogram
//! Check Prometheus [HISTOGRAMS AND SUMMARIES](https://prometheus.io/docs/practices/histograms/).
//!
//! By default, the application uses (20, 50, 100, 300, 700, 1100) as buckets to calculate response
//! time quantiles. But each service has difference requirements, so the application provides a way to
//! configure buckets in the test request itself.
//!
//! Test endpoint accepts an array field `histogramBuckets`. Users can use this field to configure
//! their own criteria. Currently, the field allows any number of buckets, but it's advisable not to
//! use more than six buckets.
//!
//! ## Grafana Dashboard
//! The application provides [sample Grafana dashboard](overload/docs/monitoring/grafana-dashboard.json) that can be used for monitoring. It has
//! graphs for Request Per Seconds, Response Status count, Average response time and Response
//! time quantiles.
//!
//! ![Grafana Dashboard - RPS](overload/docs/monitoring/grafana-dashboard.png)
//! ![Grafana Dashboard - ResponseTime, Connection pool](overload/docs/monitoring/grafana-dashboard-rt-pool.png)
//!
//!
//! # Build yourself
//!
//! ## Cluster
//!
//! To run in cluster mode, the `cluster` feature needs to be enabled during the build.
//!
//! ```shell
//! cargo build --release --features cluster
//! ```
//!
//! Sample Dockerfile and deployment configurations are provided to build the docker image and deploy it on Kubernetes
//!
//! The application will handle clustering related stuff like consensus/leader election, forwarding API call by itself.
//! Scaling up will add newly created nodes to the cluster or scaling down will remove nodes automatically.
//!
//! ## Standalone
//!
//! Build and run a single instance of the application.
//!
//! ```shell
//! cargo run
//! ```
//!
//! [deployment-yaml]: https://github.com/eipi1/overload/blob/master/deployment.yaml
//!
//! [endpoint-api]: https://kubernetes.io/docs/reference/generated/kubernetes-api/v1.19/#read-endpoints-v1-core

#![allow(clippy::upper_case_acronyms)]
#![allow(deprecated)]
#![warn(unused_lifetimes)]
#![forbid(unsafe_code)]

mod datagen;
pub mod executor;
pub mod generator;
pub mod http_util;
pub mod metrics;

use crate::metrics::MetricsFactory;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::Row;
use std::collections::HashMap;
use std::convert::TryInto;
use std::fmt::Display;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::{env, fmt};

pub const TEST_REQ_TIMEOUT: u8 = 30; // 30 sec hard timeout for requests to test target
pub const DEFAULT_DATA_DIR: &str = "/tmp";
pub const PATH_REQUEST_DATA_FILE_DOWNLOAD: &str = "/cluster/data-file";

lazy_static! {
    pub static ref METRICS_FACTORY: MetricsFactory = MetricsFactory::default();
}

#[deprecated(note = "use data_dir_path")]
pub fn data_dir() -> String {
    env::var("DATA_DIR").unwrap_or_else(|_| DEFAULT_DATA_DIR.to_string())
}

pub fn data_dir_path() -> PathBuf {
    env::var("DATA_DIR")
        .map(|env| PathBuf::from("/").join(env))
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_DATA_DIR))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpReq {
    #[serde(default = "uuid")]
    pub id: String,
    #[serde(with = "http_serde::method")]
    pub method: http::Method,
    //todo as a http::Uri
    //panic in send_request if doesn't start with /
    pub url: String,
    pub body: Option<Vec<u8>>,
    #[serde(default = "HashMap::new")]
    pub headers: HashMap<String, String>,
}

impl HttpReq {
    pub fn new(url: String) -> Self {
        HttpReq {
            id: uuid(),
            method: http::Method::GET,
            url,
            body: None,
            headers: HashMap::new(),
        }
    }
}

impl Display for HttpReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        //todo print body length
        write!(f, "{:?} {}", &self.method, &self.url)
    }
}

impl PartialEq for HttpReq {
    /// The purpose is not to test if two request is exactly equal, rather to check if two
    /// represent the same request.
    /// For a test, each request will be given a uuid. As long as uuid is equal
    /// the request will be treated as same request.
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for HttpReq {}

impl Hash for HttpReq {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<'a> sqlx::FromRow<'a, SqliteRow> for HttpReq {
    fn from_row(row: &'a SqliteRow) -> Result<Self, sqlx::Error> {
        let id: i64 = row.get("rowid");
        let method: String = row.get("method");
        let url: String = row.get("url");
        let body: Option<Vec<u8>> = row.get("body");
        let headers: String = row.get("headers");

        let req = HttpReq {
            id: id.to_string(),
            method: method.as_str().try_into().unwrap(),
            url,
            body,
            headers: serde_json::from_str(headers.as_str()).unwrap_or_default(),
        };
        Ok(req)
    }
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
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
    SqliteOpenFailed,
    PreparationFailed,
    SecondaryClusterNode,
    Others,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    job_id: String,
    status: JobStatus,
}

impl Response {
    pub fn new(job_id: String, status: JobStatus) -> Self {
        Response { job_id, status }
    }

    pub fn get_status(&self) -> JobStatus {
        self.status
    }
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

#[cfg(test)]
pub mod test_utils {
    use crate::http_util::csv_reader_to_sqlite;
    use csv_async::AsyncReaderBuilder;

    pub async fn generate_sqlite_file(file_path: &str) {
        let csv_data = r#"
"url","method","body","headers"
"http://httpbin.org/anything/11","GET","","{}"
"http://httpbin.org/anything/13","GET","","{}"
"http://httpbin.org/anything","POST","{\"some\":\"random data\",\"second-key\":\"more data\"}","{\"Authorization\":\"Bearer 123\"}"
"http://httpbin.org/bearer","GET","","{\"Authorization\":\"Bearer 123\"}"
"#;
        let reader = AsyncReaderBuilder::new()
            .escape(Some(b'\\'))
            .create_deserializer(csv_data.as_bytes());
        let to_sqlite = csv_reader_to_sqlite(reader, file_path.to_string()).await;
        log_error!(to_sqlite);
    }
}
