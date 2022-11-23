# Expectation
Specify what do we expect. Following expectations are supported -

## Constant
Number, String or Json value, expect a constant value for all the request.

```rust
# extern crate response_assert;
# extern crate serde_json;
# use response_assert::Expectation;
# let req = r###"
{
  "Constant": {
    "hello": "world"
  }
}
# "###;
# let result = serde_json::from_str::<Expectation>(req);
# assert!(result.is_ok());
```
## RequestPath
Derive from request path using provided segment number
```rust
# extern crate response_assert;
# extern crate serde_json;
# use response_assert::Expectation;
# let req = r###"
{
   "RequestPath": 1
}
# "###;
# let result = serde_json::from_str::<Expectation>(req);
# assert!(result.is_ok());
```

## RequestQueryParam
Use the value of the request query param as expectation
```rust
# extern crate response_assert;
# extern crate serde_json;
# use response_assert::Expectation;
# let req = r###"
{
  "RequestQueryParam": "hello"
}
# "###;
# let result = serde_json::from_str::<Expectation>(req);
# assert!(result.is_ok());
```