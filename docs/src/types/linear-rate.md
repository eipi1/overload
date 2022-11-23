## Linear

| field | data type | Description       |
|-------|-----------|-------------------|
| a     | float     | Slope of the line |
| b     | uint32    | Intercept         |
| max   | uint32    | max QPS           |

### Example

```rust
# extern crate overload_http;
# extern crate serde_json;
# use overload_http::RateSpecEnum;
# let req = r###"
{
  "Linear": {
    "a": 2,
    "b": 1,
    "max": 12
  }
}
# "###;
# let result = serde_json::from_str::<RateSpecEnum>(req);
# assert!(result.is_ok());
```
