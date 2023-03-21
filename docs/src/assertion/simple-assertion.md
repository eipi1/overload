# \[Deprecated\] Simple Assertion
A set of assertion can be passed to verify response received from API in test. Assertion will fail if response can't
be parsed as JSON.

For example, the following request will send request to httpbin.org/get and verify response is a json and
the value at `$.headers.Host` (JsonPath) is `httpbin.org`.

Failed assertion emits an info log and can also be monitored through metrics.

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
  "responseAssertion": {
      "simpleAssertion": [
        {
          "id": 1,
          "expectation": {
            "Constant": "httpbin.org"
          },
          "actual": {
            "FromJsonResponse": {
              "path": "$.headers.Host"
            }
          }
        }
      ]
    }
}
# "###;
# let result = serde_json::from_str::<Request>(req);
# assert!(result.is_ok());
```

## Simple Assertion

| field       | Description                                          | data type                     |
|-------------|------------------------------------------------------|-------------------------------|
| id          | assertion identifier, used in metrics                | uint32                        |
| expectation | Expected value, constant or derived from the request | [Expectation](expectation.md) |
| actual      | Actual value, constant or derived from the response  | [Actual](actual.md)           |
