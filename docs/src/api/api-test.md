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
  "duration": 800,
  "name": "test-overload",
  "histogram_buckets": [
    1,
    2,
    5,
    7,
    10,
    15
  ],
  "qps": {
    "Linear": {
      "a": 1,
      "b": 1,
      "max": 100
    }
  },
  "req": {
    "RequestList": {
      "data": [
        {
          "method": "GET",
          "url": "/serde",
          "headers": {
            "Host": "127.0.0.1:2000",
            "Connection": "keep-alive"
          }
        }
      ]
    }
  },
  "target": {
    "host": "172.17.0.1",
    "port": 2000,
    "protocol": "HTTP"
  },
  "concurrentConnection": {
    "Linear": {
      "a": 1,
      "b": 1,
      "max": 100
    }
  },
  "responseAssertion": {
    "luaAssertion": [
      "function(method, url, requestBody, responseBody)",
      "    -- initialize assertion result",
      "    result = {}",
      "    -- verify valid json response",
      "    jsonResp=json.decode(responseBody);",
      "    if jsonResp == nil then",
      "        -- create new assertion result",
      "        local err = {}",
      "        -- used for metrics, should avoid duplicate values in the same assertion",
      "        err.id = 1",
      "        err.success = false",
      "        err.error = 'Failed to parse to json'",
      "        -- add to result",
      "        table.insert(result, err)",
      "    end",
      "    return result",
      "end"
    ]
  }
}
# "###;
# let result = serde_json::from_str::<Request>(req);
# assert!(result.is_ok());
```