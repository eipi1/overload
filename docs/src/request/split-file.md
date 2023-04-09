## SplitRequestFile

Get requests from a CSV file, split the file equally between secondary nodes. File has to be [uploaded](../api/upload-data-file.md) separately before
the test. Upload request returns name/id of the file to be used in the test request.

| field     | Description             | data type |
|-----------|-------------------------|-----------|
| file_name | ID of the uploaded file | UUID      |

#### Example
```rust
# extern crate overload_http;
# extern crate serde_json;
# use overload_http::Request;
# let req = r###"
{
  "duration": 3,
  "req": {
    "SplitRequestFile": {
      "file_name": "4e1d1b32-0f1e-4b31-92dd-66f51c5acf9a"
    }
  },
  "qps": {
    "ConstantRate": {
      "countPerSec": 8
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