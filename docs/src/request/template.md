## JsonTemplateRequest
Generate request based on a template.
### Version
SNAPSHOT
### Example
```rust
# extern crate overload_http;
# extern crate serde_json;
# use overload_http::{Request, RequestSpecEnum};
# let req = r###"
{
  "duration": 10,
  "name": "json-template-post-2.json",
  "qps": {
    "ConstantRate": {
      "countPerSec": 3
    }
  },
  "req": {
    "JsonTemplateRequest": {
      "method": "POST",
      "url": "{{patternStr(\"/anything/[a-z]{4}\")}}",
      "headers": {
        "Host": "httpbin.org",
        "Connection": "keep-alive"
      },
      "body": [
        {
          "id": "{{randomInt(1,10)}}",
          "type": "donut",
          "name": "Cake",
          "ppu": "{{randomFloat(0.2,1.0)}}",
          "available": "{{randomBool()}}",
          "batters": {
            "batter": [
              {
                "id": "{{toStr(randomInt(100,200))}}",
                "type": "Regular"
              },
              {
                "id": "{{toStr(randomInt(100,200))}}",
                "type": "Chocolate"
              }
            ]
          },
          "topping": [
            {
              "id": "5001",
              "type": "None"
            },
            {
              "id": "5002",
              "type": "Glazed"
            },
            {
              "id": "5005",
              "type": "Sugar"
            },
            {
              "id": "5007",
              "type": "Powdered Sugar"
            },
            {
              "id": "5006",
              "type": "Chocolate with Sprinkles"
            },
            {
              "id": "5003",
              "type": "Chocolate"
            },
            {
              "id": "5004",
              "type": "Maple"
            }
          ]
        },
        {
          "id": "0002",
          "type": "donut",
          "name": "{{patternStr(\"[a-z]{5}-[0-9]{3}\")}}",
          "ppu": 0.6,
          "available": "{{randomBool()}}",
          "batters": {
            "batter": [
              {
                "id": "1001",
                "type": "Regular"
              }
            ]
          },
          "topping": [
            {
              "id": "5001",
              "type": "None"
            },
            {
              "id": "5002",
              "type": "Glazed"
            }
          ]
        }
      ]
    }
  },
  "target": {
    "host": "httpbin.org",
    "port": 80,
    "protocol": "HTTP"
  },
  "generationMode": {
    "batch": {
      "batchSize": 3
    }
  },
  "concurrentConnection": {
    "ConstantRate": {
      "countPerSec": 3
    }
  }
}
# "###;
# let result = serde_json::from_str::<Request>(req);
# assert!(result.is_ok());
# assert!(matches!(result.unwrap().req, RequestSpecEnum::JsonTemplateRequest(_)));
```
The above request will generate traffic to httpbin.org/anything/{randomPath} with request body like the following
```json
[
  {
    "id": 6,
    "type": "donut",
    "name": "Cake",
    "ppu": 0.5362728858152506,
    "available": false,
    "batters": {
      "batter": [
        {
          "id": "161",
          "type": "Regular"
        },
        {
          "id": "123",
          "type": "Chocolate"
        }
      ]
    },
    "topping": [
      {
        "id": "5001",
        "type": "None"
      },
      {
        "id": "5002",
        "type": "Glazed"
      },
      {
        "id": "5005",
        "type": "Sugar"
      },
      {
        "id": "5007",
        "type": "Powdered Sugar"
      },
      {
        "id": "5006",
        "type": "Chocolate with Sprinkles"
      },
      {
        "id": "5003",
        "type": "Chocolate"
      },
      {
        "id": "5004",
        "type": "Maple"
      }
    ]
  },
  {
    "id": "0002",
    "type": "donut",
    "name": "mafiq-895",
    "ppu": 0.6,
    "available": false,
    "batters": {
      "batter": [
        {
          "id": "1001",
          "type": "Regular"
        }
      ]
    },
    "topping": [
      {
        "id": "5001",
        "type": "None"
      },
      {
        "id": "5002",
        "type": "Glazed"
      }
    ]
  }
]
```

| field          | Required | Description                                          | data type           | Validation                                           |
|----------------|----------|------------------------------------------------------|---------------------|------------------------------------------------------|
| method         | Yes      | HTTP method                                          | Enum ("POST","GET") |                                                      |
| url            | Yes      | non-empty                                            | string or template  | string or string produced by template start with `/` |
| headers        | No       | HTTP request header                                  | Map<String,String>  |                                                      |
| body           | No       | Request body with template                           | JSON                |                                                      |

### Supported Template Functions
| Function                  | Parameter           | Description                                                  |
|---------------------------|---------------------|--------------------------------------------------------------|
| randomInt(min,max)        | (int, int)          | Generate random integer with min<=value<max                  |
| randomBool()              |                     | Generate true or false                                       |
| randomFloat(min, max)     | (float, float)      | Generate random float with min<=value<max                    |
| randomStr(minLen, maxLen) | (int, int)          | Generate random alphabetical string with minLen<=len<=maxLen |
| patternStr(pattern)       | (regex)             | Generate random string with the provided regex               |
| toStr(value)              | (int/boolean/float) | Convert values to string                                     |
| toInt(value)              | (int)               | Converts string to integer                                   |

#### Notes
* Template function can be chained - "{{toStr(randomBool())}}" is valid and will generate "true" or "false"
* Request body needs to be valid JSON, so all template including int/boolean fields needs to be string. Datatype will be determined based on the function used.
  * randomInt(..) will produce json integer value, randomBool() will produce boolean
  * Use toStr/toInt to convert for type conversion if necessary
* Pattern
  * Unicode pattern. Careful with character groups, e.g. `\d` represents digits from english, arabic and other languages.
  * Maximum repeat(`*`,`+`) length is 10.
  * For integer, non-numeric pattern (eg. `^a[0-9]{4}z$`) will produce 0
