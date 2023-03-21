# Lua Assertion
[Lua](https://www.lua.org/) is a powerful, efficient, lightweight scripting language. Overload comes with lua embedded.
Users can write complex logic to verify request/response and expose results through metrics. The embedded engine provides
functions to parse JSON.

Lua assertion requires following conventions -
* Supported lua version: 5.3
* Lua chunks must be a function
* The function accepts 4 arguments in following order
  * HTTP method
  * URL
  * Request Body (String)
  * Response Body (String)
* The function must return assertion failure as an array of assertion results

Note that, uncaught lua exceptions will be reported in metrics with assertion_id = 2147483647. The details of the error 
can be found in application info log.

## Writing simple assertion

### Writing Lua function
The following chunks will verify the request method.
```lua
function(method, url, requestBody, responseBody)
    -- initialize assertion result
    result = {}
    -- verify http method
    if method ~= 'POST' then 
        -- create new assertion result
        local err = {}
        -- used for metrics, should avoid duplicate values in the same assertion
        err.id = 1
        err.success = false
        err.error = 'Invalid method'
        -- add to result
        table.insert(result, err)
    end
    return result
end
```
#### Assertion Result
Overload transforms the array returned from lua function to an array. Each array element needs to be a table with following
keys -

| field   | Description                                        | data type |
|---------|----------------------------------------------------|-----------|
| id      | Assertion identifier, used in metrics              | uint32    |
| success | Was the the assertion successful                   | boolean   |
| error   | Requires for failed cases - a detail error message | String    |

### Test request with the lua function
Lua is included in the test request as an array of strings.
```rust
# extern crate overload_http;
# extern crate serde_json;
# use overload_http::Request;
# let req = r###"
{
  "duration": 10,
  "name": "test-assertion",
  "qps": {
    "ConstantRate": {
      "countPerSec": 3
    }
  },
  "req": {
    "RequestList": {
      "data": [
        {
          "method": "POST",
          "url": "/anything",
          "headers": {
            "Host": "httpbin.org",
            "Connection":"keep-alive"
          },
          "body": "{\"keys\":[\"0001\",\"0002\",\"0004\"]}"
        }
      ]
    }
  },
  "target": {
    "host": "httpbin.org",
    "port": 80,
    "protocol": "HTTP"
  },
  "concurrentConnection": {
    "ConstantRate": {
      "countPerSec": 3
    }
  },
  "responseAssertion": {
    "luaAssertion": [
      "function(method, url, requestBody, responseBody)",
      "    -- initialize assertion result",
      "    result = {}",
      "    -- verify http method",
      "    if method ~= 'POST' then",
      "        -- create new assertion result",
      "        local err = {}",
      "        -- used for metrics, should avoid duplicate values in the same assertion",
      "        err.id = 1",
      "        err.success = false",
      "        err.error = 'Invalid method'",
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