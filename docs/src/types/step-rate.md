### Steps/Staircase Rate

| field | Description   | data type     |
|-------|---------------|---------------|
| steps | Array of Step | [Step](#step) |

#### Step

| field | Description          | data type |
|-------|----------------------|-----------|
| start | Start of the step    | \[uint32\]  |
| end   | end of the step      | \[uint32\]  |
| qps   | QPS during this step | \[uint32\]  |

#### Example

```rust
# extern crate overload_http;
# extern crate serde_json;
# use overload_http::RateSpecEnum;
# let req = r###"
{
  "Steps" : {
    "steps": [
      {
        "start": 0,
        "end": 5,
        "rate": 1
      },
      {
        "start": 6,
        "end": 8,
        "rate": 4
      },
      {
        "start": 9,
        "end": 10,
        "rate": 7
      }
    ]
  }
}
# "###;
# let result = serde_json::from_str::<RateSpecEnum>(req);
# assert!(result.is_ok());
```
