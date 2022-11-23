# Getting Started
## Start Test
The following request will send two `GET` request per second(`"countPerSec": 2`) to `httpbin.org/get` for 120
seconds(`"duration": 60`).
```shell
curl --location --request POST '{overload_host}:3030/test' \
--header 'Content-Type: application/json' \
--data-raw '<json_request_body>'
```

Sample JSON request body -

```rust
# extern crate overload_http;
# extern crate serde_json;
# use overload_http::Request;
# let req = r###"
{
  "duration": 120,
  "name": "demo-test",
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
    "host": "httpbin.org",
    "port": 80,
    "protocol": "HTTP"
  }
}
# "###;
# let result = serde_json::from_str::<Request>(req);
# assert!(result.is_ok());
```


It'll respond with a job identifier and status.
```json
{
  "job_id": "demo-test-d2ae5ff0-7bf4-4daf-8784-83b642d7dd6b",
  "status": "Starting"
}
```
We'll need the `job_id` if we want to stop the test later.

## Get job status
```shell
curl --location --request GET '{overload_host}:3030/test/status/'
```
## Stop a job
```shell
curl --location --request GET '{overload_host}:3030/test/stop/demo-test-d2ae5ff0-7bf4-4daf-8784-83b642d7dd6b'
```