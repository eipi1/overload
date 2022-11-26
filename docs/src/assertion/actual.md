# Actual
Following actual sources are supported -

## Constant
Number, String or JSON value, expect a constant value for all the request.

```rust
# extern crate response_assert;
# extern crate serde_json;
# use response_assert::Actual;
# let req = r###"
{
  "Constant": {
    "hello": "world"
  }
}
# "###;
# let result = serde_json::from_str::<Actual>(req);
# assert!(result.is_ok());
```

## FromJsonResponse
Derive expectation from JSON response using JsonPath
```rust
# extern crate response_assert;
# extern crate serde_json;
# use response_assert::Actual;
# let req = r###"
{
  "FromJsonResponse": {
    "path": "$.hello"
  }
}
# "###;
# let result = serde_json::from_str::<Actual>(req);
# assert!(result.is_ok());
```