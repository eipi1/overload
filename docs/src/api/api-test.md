## `POST /test`
Start a test with given specification

### Request Body
{{#include ../types/request.md:3:12}}

### Example

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
  },
  "histogramBuckets": [35,40,45,48,50, 52]
}
# "###;
# let result = serde_json::from_str::<Request>(req);
# assert!(result.is_ok());
```