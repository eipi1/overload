## ConstantRate

| field         | data type | Description                                              |
|---------------|-----------|----------------------------------------------------------|
| countPerSec   | uint32    | Constant rate to maintain for the `duration` of the test |

### Example

```rust
# extern crate overload_http;
# extern crate serde_json;
# use overload_http::RateSpecEnum;
# let req = r###"
{
    "ConstantRate": {
      "countPerSec": 10
    }
}
# "###;
# let result = serde_json::from_str::<RateSpecEnum>(req);
# assert!(result.is_ok());
```
