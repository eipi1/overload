# `POST /tests`
Start a group of tests with given specifications.

## Request Body

| Field    | Required | Default | Data type                      | Description                                                         |
|----------|----------|---------|--------------------------------|---------------------------------------------------------------------|
| name     | ❎        | UUID    | String                         | Test name, application will append UUID to ensure unique identifier |
| requests | ✅        |         | [Request](../types/request.md) | Collection of Request to execute                                    |

## Example

```shell
curl --location --request POST 'localhost:3030/tests' \
--header 'Content-Type: application/json' \
--data-raw '<json_request_body>'
```

Sample JSON request body -

```rust
# extern crate overload_http;
# extern crate serde_json;
# use overload_http::MultiRequest;
# let req = r###"
{
  "name": "multi-req",
  "requests": [
    {
      "duration": 600,
      "name": "demo-test",
      "qps": {
        "Linear": {
          "a": 2,
          "b": 1,
          "max": 400
        }
      },
      "req": {
        "RequestList": {
          "data": [
            {
              "method": "GET",
              "url": "/delay/1",
              "headers": {
                "Host": "127.0.0.1:8080",
                "Connection":"keep-alive"
              }
            }
          ]
        }
      },
      "target": {
        "host": "172.17.0.1",
        "port": 8080,
        "protocol": "HTTP"
      },
      "concurrentConnection": {
        "Linear": {
          "a": 2.5,
          "b": 1,
          "max": 500
        }
      }
    },
    {
      "duration": 500,
      "name": "demo-test",
      "qps": {
        "Linear": {
          "a": 1,
          "b": 1,
          "max": 400
        }
      },
      "req": {
        "RequestList": {
          "data": [
            {
              "method": "GET",
              "url": "/get",
              "headers": {
                "Host": "127.0.0.1:8080",
                "Connection":"keep-alive"
              }
            }
          ]
        }
      },
      "target": {
        "host": "172.17.0.1",
        "port": 8080,
        "protocol": "HTTP"
      },
      "concurrentConnection": {
        "Linear": {
          "a": 0.5,
          "b": 1,
          "max": 70
        }
      }
    }
  ]
}
# "###;
# let result = serde_json::from_str::<MultiRequest>(req);
# assert!(result.is_ok());
```
This will run two tests with their own QPS/connection rate, one for 600 seconds, another for 500 seconds.

## Response

| field  | Description        | data type                           |
|--------|--------------------|-------------------------------------|
| job_id | Test identifier    | UUID                                |
| status | status of the test | [JobStatus](../types/job-status.md) |

```json
{
  "job_id": "1b826ded-34d5-434c-b983-a1ba46ffff82",
  "status": "Starting"
}
```
