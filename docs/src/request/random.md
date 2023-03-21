## RandomDataRequest

Generate request with random data based on constraints specified using JSON Schema like syntax.

| field          | Required | Description                                                                                                                                       | data type           |
|----------------|----------|---------------------------------------------------------------------------------------------------------------------------------------------------|---------------------|
| method         | Yes      | HTTP method                                                                                                                                       | Enum ("POST","GET") |
| url            | Yes      | Request Url, optionally supports param substitution. Param name should be of format `{[a-z0-9]+}`, e.g. http://httpbin.org/anything/{param1}/{p2} | string              |
| headers        | No       | HTTP request header                                                                                                                               | Map<String,String>  |
| bodySchema     | No       | Request body spec to be used for random data generation                                                                                           | JSON Schema         |
| uriParamSchema | No       | Url param spec to be used for random data generation                                                                                              | JSON Schema         |

### Supported JSON Schema spec
| type    | supported |
|---------|-----------|
| string  | ✅         |
| integer | ✅         |
| object  | ✅         |
| array   | ✅         |
| boolean | ❎         |
| null    | ❎         |

### Supported constraints
| Constraints | Supported | Note                                                                                                                                                       |
|-------------|-----------|------------------------------------------------------------------------------------------------------------------------------------------------------------|
| minLength   | ✅         |                                                                                                                                                            |
| maxLength   | ✅         |                                                                                                                                                            |
| minimum     | ✅         |                                                                                                                                                            |
| maximum     | ✅         |                                                                                                                                                            |
| constant    | ✅         |                                                                                                                                                            |
| pattern     | ✅         | Unicode pattern. Careful with character groups, e.g. `\d` represents digits from english, arabic and other languages. Maximum repeat(`*`,`+`) length is 10 |
| format      | ❎         |                                                                                                                                                            |

```rust
# extern crate overload_http;
# extern crate serde_json;
# use overload_http::Request;
# let req = r###"
{
  "duration": 10,
  "req": {
    "RandomDataRequest": {
      "url": "/anything/{param1}/{param2}",
      "method": "POST",
      "bodySchema": {
        "title": "Person",
        "type": "object",
        "properties": {
          "firstName": {
            "type": "string",
            "description": "The person's first name."
          },
          "lastName": {
            "type": "string",
            "description": "The person's last name."
          },
          "age": {
            "description": "Age in years which must be equal to or greater than zero.",
            "type": "integer",
            "minimum": 10,
            "maximum": 20
          },
          "objArrayKeys": {
            "type": "array",
            "maxLength": 20,
            "minLength": 10,
            "items": {
              "type": "object",
              "properties": {
                "objKey1": {
                  "type": "integer",
                  "minimum": 10,
                  "maximum": 1000
                },
                "objKey2": {
                  "type": "string",
                  "pattern": "^a[0-9]{4}z$"
                }
              }
            }
          },
          "scalarArray": {
            "type": "array",
            "maxLength": 20,
            "minLength": 20,
            "items": {
              "type": "string",
              "pattern": "^a[0-9]{4}z$"
            }
          }
        }
      },
      "uriParamSchema": {
        "type": "object",
        "properties": {
          "param1": {
            "type": "string",
            "description": "The person's first name.",
            "minLength": 6,
            "maxLength": 15
          },
          "param2": {
            "description": "Age in years which must be equal to or greater than zero.",
            "type": "integer",
            "minimum": 1000000,
            "maximum": 1100000
          }
        }
      }
    }
  },
  "qps": {
    "ConstantRate": {
      "countPerSec": 5
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

This request will generate POST request to urls like "http://httpbin.org/anything/uejaiwd/1044053", "http://httpbin.org/anything/ryjhwq/1078153"
and so on with a randomly generated JSON body.