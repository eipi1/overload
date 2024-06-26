#![warn(unused_lifetimes)]
#![forbid(unsafe_code)]

use crate::AssertionError::{FailedAssert, InvalidActual, InvalidExpectation, LuaAssertionFailure};
use anyhow::anyhow;
use http::{Method, Uri};
use jsonpath_lib::Compiled;
use log::trace;
use lua_helper::{call_lua_func_from_registry, LuaAssertionResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::num::NonZeroU8;
use std::ops::Deref;
use strum::IntoStaticStr;
use tokio::sync::mpsc::UnboundedSender as TkMpscSender;
use tokio::sync::oneshot::Sender as TkOneShotSender;

type SegmentNumber = NonZeroU8;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Expectation {
    RequestPath(SegmentNumber),
    RequestQueryParam(String),
    RequestBody(JsonPath),
    Constant(Value),
    // JsonSchema(JsonSchema),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Actual {
    FromJsonResponse(JsonPath),
    Constant(Value),
    // FullResponse,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Assertion {
    pub id: u32,
    pub(crate) expectation: Expectation,
    pub(crate) actual: Actual,
}

#[derive(Deserialize, Serialize)]
#[serde(try_from = "JsonSchemaShadowType")]
#[allow(dead_code)]
struct JsonSchema {
    pub(crate) schema: Value,
    #[serde(skip)]
    pub(crate) compiled: jsonschema::JSONSchema,
}

impl TryFrom<Value> for JsonSchema {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let compiled_schema = jsonschema::JSONSchema::compile(&value)
            .map_err(|e| anyhow!(format!("Error parsing json schema: {}", e)))?;
        Ok(Self {
            schema: value,
            compiled: compiled_schema,
        })
    }
}

#[derive(Deserialize, Serialize)]
struct JsonSchemaShadowType {
    pub(crate) schema: Value,
}

impl TryFrom<JsonSchemaShadowType> for JsonSchema {
    type Error = anyhow::Error;

    fn try_from(value: JsonSchemaShadowType) -> Result<Self, Self::Error> {
        let x = value.schema;
        TryFrom::try_from(x)
    }
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(try_from = "JsonPathShadowType")]
pub struct JsonPath {
    pub(crate) path: String,
    #[serde(skip)]
    pub(crate) inner: Compiled,
}

impl TryFrom<&str> for JsonPath {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let compiled = Compiled::compile(value)
            .map_err(|e| anyhow!(format!("Error creating json path: {}", e)))?;
        Ok(JsonPath {
            path: value.into(),
            inner: compiled,
        })
    }
}

impl Debug for JsonPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonPath")
            .field("path", &self.path)
            .finish()
    }
}

impl Deref for JsonPath {
    type Target = Compiled;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(Deserialize, Serialize)]
struct JsonPathShadowType {
    pub(crate) path: String,
}

impl TryFrom<JsonPathShadowType> for JsonPath {
    type Error = anyhow::Error;

    fn try_from(value: JsonPathShadowType) -> Result<Self, Self::Error> {
        let x = &*value.path;
        TryFrom::try_from(x)
    }
}

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ResponseAssertion {
    pub simple_assertions: Option<Vec<Assertion>>,
    pub lua_assertion: Option<Vec<String>>,
}

#[derive(Debug, IntoStaticStr)]
pub enum AssertionError {
    InvalidExpectation(u32, String),
    FailedAssert(u32, String),
    InvalidActual(u32, String),
    LuaAssertionFailure(u32, String),
}

impl AssertionError {
    pub fn get_id(&self) -> u32 {
        match self {
            &InvalidExpectation(id, ..)
            | &FailedAssert(id, ..)
            | &InvalidActual(id, ..)
            | &LuaAssertionFailure(id, ..) => id,
        }
    }
}

impl TryFrom<LuaAssertionResult> for AssertionError {
    type Error = ();

    fn try_from(value: LuaAssertionResult) -> Result<Self, Self::Error> {
        Ok(LuaAssertionFailure(
            value.assertion_id as u32,
            value.assertion_result.err().ok_or(())?,
        ))
    }
}

pub fn simple_assert(
    assertions: &ResponseAssertion,
    request_url: &Uri,
    request_body: Option<&Value>,
    response: &Value,
) -> Result<(), Vec<AssertionError>> {
    trace!(
        "Asserting - assertion count: {}, uri: {}",
        assertions
            .simple_assertions
            .as_ref()
            .map_or(0, |v| { v.len() }),
        request_url.to_string()
    );
    let mut errors = vec![];
    let mut paths: Option<Vec<&str>> = None;
    let mut params = None;
    let mut all_matched = true;
    for assertion in assertions
        .simple_assertions
        .as_ref()
        .unwrap_or(&vec![])
        .iter()
    {
        let id = assertion.id;
        //temporary container to avoid 'returns a value referencing data owned by the current function'
        let mut tmp = vec![];
        let expectation = match &assertion.expectation {
            Expectation::RequestPath(segment) => {
                if paths.is_none() {
                    let split = request_url.path().split('/').collect::<Vec<_>>();
                    paths = Some(split);
                }
                let split = paths.as_ref().unwrap();
                let pos = segment.get() as usize;
                if split.len() < pos + 1 {
                    errors.push(InvalidExpectation(
                        id,
                        format!(
                            "Assertion: {:?} - {:?}: SegmentNumber too big.",
                            &assertion, &assertion.expectation
                        ),
                    ));
                    None
                } else {
                    let value = Value::String(split.get(pos).unwrap().to_string());
                    tmp.push(value);
                    Some(vec![tmp.last().unwrap()])
                }
            }
            Expectation::RequestQueryParam(param_name) => {
                if params.is_none() {
                    match request_url.query() {
                        None => {
                            params = Some(HashMap::new());
                        }
                        Some(queries) => {
                            let query_map = queries
                                .split('&')
                                .map(|query| query.split_at(query.find('=').unwrap()))
                                .map(|(s1, s2)| (s1, &s2[1..]))
                                .collect::<HashMap<_, _>>();
                            params = Some(query_map);
                        }
                    }
                }
                params
                    .as_ref()
                    .unwrap()
                    .get(param_name.as_str())
                    .map(|t| {
                        let value = Value::String(t.to_string());
                        tmp.push(value);
                        vec![tmp.last().unwrap()]
                    })
                    .or_else(|| {
                        errors.push(InvalidExpectation(
                            id,
                            format!(
                                "Assertion: {:?} - {:?}: query param not found.",
                                &assertion, assertion.expectation
                            ),
                        ));
                        None
                    })
            }
            Expectation::Constant(val) => Some(vec![val]),
            Expectation::RequestBody(path) => {
                if let Some(body) = request_body {
                    path.select(body)
                        .map_err(|e| {
                            errors.push(InvalidExpectation(
                                id,
                                format!(
                                    "Assertion: {:?} - {:?}: failed to extract value: {}.",
                                    &assertion, assertion.expectation, e
                                ),
                            ));
                        })
                        .ok()
                } else {
                    errors.push(InvalidExpectation(
                        id,
                        format!("{:?} - no request body", &assertion),
                    ));
                    None
                }
            }
        };
        let mut matched = false;
        match expectation {
            None => {}
            Some(expect) => {
                if !expect.is_empty() {
                    match &assertion.actual {
                        Actual::FromJsonResponse(path) => match path.select(response) {
                            Ok(actual) => {
                                matched = actual == expect;
                                if !matched {
                                    errors.push(FailedAssert(
                                        id,
                                        format!(
                                            "{:?} - expected: {:?}, actual: {:?}",
                                            &assertion, expect, actual
                                        ),
                                    ));
                                }
                            }
                            Err(e) => errors.push(InvalidActual(
                                id,
                                format!("{:?} - {:?}: error: {}", &assertion, &assertion.actual, e),
                            )),
                        },
                        Actual::Constant(val) => {
                            matched = expect == vec![val];
                            if !matched {
                                errors.push(FailedAssert(
                                    id,
                                    format!(
                                        "{:?} - expected: {:?}, actual: {:?}",
                                        &assertion, expect, val
                                    ),
                                ));
                            }
                        }
                    }
                } else {
                    errors.push(InvalidExpectation(
                        id,
                        format!(
                            "{:?} - expectation not found or empty - {:?}",
                            &assertion, expect
                        ),
                    ));
                }
            }
        };
        if !matched {
            all_matched = false;
        }
    }
    if all_matched {
        Ok(())
    } else {
        Err(errors)
    }
}

type ResponseBody = String;
type UrlStr = String;
type RequestBody = Option<String>;
type RequestHeader = HashMap<String, String>;
pub type LuaAssertionResultSender = TkOneShotSender<Result<(), Vec<AssertionError>>>;
pub type LuaExecSender = TkMpscSender<(
    ResponseBody,
    Method,
    UrlStr,
    RequestBody,
    RequestHeader,
    LuaAssertionResultSender,
)>;

pub async fn init_lua_executor(response_assert: &ResponseAssertion) -> Option<LuaExecSender> {
    let chunks = response_assert.lua_assertion.as_ref()?;
    if !chunks.is_empty() {
        let chunks = chunks.join("\n");
        let lua = lua_helper::init_lua();
        let func_key = lua_helper::load_lua_func_with_registry(&chunks, &lua)?;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<(
            String,
            Method,
            UrlStr,
            RequestBody,
            RequestHeader,
            LuaAssertionResultSender,
        )>();
        tokio::spawn(async move {
            loop {
                let msg = receiver.recv().await;
                if let Some((resp_body, method, url, req_body, _req_header, sender)) = msg {
                    let lua_result = call_lua_func_from_registry(
                        &lua,
                        &func_key,
                        method.as_str(),
                        url,
                        req_body,
                        resp_body,
                    )
                    .map_err(|mut errors| {
                        errors
                            .drain(..)
                            .map(AssertionError::try_from)
                            .filter_map(|x| x.ok())
                            .collect::<Vec<_>>()
                    });
                    trace!("init_lua_executor - sending back lua result");
                    let _ = sender.send(lua_result);
                } else {
                    break;
                }
            }
        });
        Some(sender)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::{simple_assert, Actual, Assertion, Expectation, ResponseAssertion, SegmentNumber};
    use http::Uri;
    use serde_json::Value;

    #[test]
    fn test_de_serialize() {
        let constant_str = Actual::Constant("$.phoneNumbers[:1].type".into());
        println!("{}", serde_json::to_string(&constant_str).unwrap());
        println!(
            "{:?}",
            serde_json::from_str::<Actual>(&serde_json::to_string(&constant_str).unwrap()).unwrap()
        );

        let actual = Actual::FromJsonResponse("$.phoneNumbers[:1].type".try_into().unwrap());
        println!("{}", serde_json::to_string(&actual).unwrap());
        println!(
            "{:?}",
            serde_json::from_str::<Actual>(&serde_json::to_string(&actual).unwrap()).unwrap()
        );

        let assertion = Assertion {
            id: 123,
            expectation: Expectation::Constant("iPhone".into()),
            actual,
        };

        serde_json::from_str::<Assertion>(&serde_json::to_string(&assertion).unwrap()).unwrap();
        let vec1 = vec![assertion];
        let validator = ResponseAssertion {
            simple_assertions: Some(vec1),
            lua_assertion: None,
        };
        let json = serde_json::to_string(&validator).unwrap();
        println!("{:?}", &json);
        let validator: ResponseAssertion = serde_json::from_str(&json).unwrap();
        // let expectation = ExpectationSource::Constant("iPhone".into());
        assert!(matches!(
            &validator
                .simple_assertions
                .as_ref()
                .unwrap()
                .first()
                .unwrap()
                .expectation,
            Expectation::Constant(_)
        ));
        match &validator
            .simple_assertions
            .as_ref()
            .unwrap()
            .first()
            .unwrap()
            .expectation
        {
            Expectation::Constant(x) => {
                assert_eq!(x.as_str().unwrap(), "iPhone")
            }
            _ => {
                panic!()
            }
        }

        assert!(matches!(
            &validator
                .simple_assertions
                .as_ref()
                .unwrap()
                .first()
                .unwrap()
                .actual,
            Actual::FromJsonResponse(_)
        ));
    }

    #[test]
    fn test_assertion() {
        let mut assertions = vec![];
        let assertion = Assertion {
            id: 0,
            expectation: Expectation::RequestPath(SegmentNumber::new(2_u8).unwrap()),
            actual: Actual::FromJsonResponse("$.phoneNumbers[0].type".try_into().unwrap()),
        };
        assertions.push(assertion);
        let assertion = Assertion {
            id: 0,
            expectation: Expectation::Constant(Value::String("Nara".to_string())),
            actual: Actual::FromJsonResponse("$.address.city".try_into().unwrap()),
        };
        assertions.push(assertion);

        let assertion = Assertion {
            id: 0,
            expectation: Expectation::RequestBody("$.address.city".try_into().unwrap()),
            actual: Actual::FromJsonResponse("$.address.city".try_into().unwrap()),
        };
        assertions.push(assertion);

        let assertion = Assertion {
            id: 0,
            expectation: Expectation::RequestQueryParam("age".to_string()),
            actual: Actual::Constant(Value::String("16".to_string())),
        };
        assertions.push(assertion);

        let assertions = ResponseAssertion {
            simple_assertions: Some(assertions),
            ..Default::default()
        };

        let uri = Uri::try_from("http://localhost:3030/hello/iPhone?eanye=dxwr&age=16".to_string())
            .unwrap();

        let response = json_response();
        let selector = jsonpath_lib::Compiled::compile("$").unwrap();
        println!("selector: {:?}", selector.select(&response));

        let assert_result = simple_assert(&assertions, &uri, Some(&response), &response);
        println!("{:?}", &assert_result);
        assert!(&assert_result.is_ok());
    }

    fn json_response() -> Value {
        let resp = r#"{
    "firstName": "John",
    "lastName": "doe",
    "age": 26,
    "address": {
        "streetAddress": "naist street",
        "city": "Nara",
        "postalCode": "630-0192"
    },
    "phoneNumbers": [
        {
            "type": "iPhone",
            "number": "0123-4567-8888"
        },
        {
            "type": "home",
            "number": "0123-4567-8910"
        }
    ]
}"#;
        serde_json::from_str(resp).unwrap()
    }
}
