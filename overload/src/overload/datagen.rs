use crate::datagen::DataSchema::Empty;
use anyhow::{Error as AnyError, Result as AnyResult};
use log::{error, trace};
use rand::thread_rng;
use regex_generate::generate_from_hir;
use regex_syntax::hir::Hir;
use regex_syntax::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::convert::TryFrom;

const PATTER_MAX_REPEAT: u32 = 10;

#[derive(Debug, Serialize, Deserialize, Eq, PartialEq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
pub enum Keywords {
    MinLength,
    MaxLength,
    Minimum,
    Maximum,
    Constant,
    Pattern,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum Constraints {
    MinLength(i64),
    MaxLength(i64),
    Minimum(i64),
    Maximum(i64),
    ConstantInt(i64),
    ConstantStr(String),
    Pattern(String, #[serde(skip)] Option<Hir>),
}

#[allow(dead_code)]
impl Constraints {
    fn as_i64(&self) -> Option<i64> {
        match *self {
            Constraints::MinLength(x) => Option::Some(x),
            Constraints::MaxLength(x) => Option::Some(x),
            Constraints::Minimum(x) => Option::Some(x),
            Constraints::Maximum(x) => Option::Some(x),
            Constraints::ConstantInt(x) => Option::Some(x),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match *self {
            Constraints::ConstantStr(ref x) => Option::Some(x),
            _ => None,
        }
    }

    fn as_pattern(&self) -> Option<(&String, &Option<Hir>)> {
        match *self {
            Constraints::Pattern(ref pattern, ref hir) => Option::Some((pattern, hir)),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(try_from = "Value")]
#[serde(into = "Value")]
pub enum DataSchema {
    Empty,
    String(String, HashMap<Keywords, Constraints>),
    Integer(String, HashMap<Keywords, Constraints>),
    Object(Option<String>, Vec<DataSchema>),
}

#[allow(dead_code)]
impl DataSchema {
    fn data(&self) -> Option<&Vec<DataSchema>> {
        match *self {
            DataSchema::Object(_, ref d) => Some(d),
            _ => None,
        }
    }
}

impl Default for DataSchema {
    fn default() -> Self {
        Empty
    }
}

impl TryFrom<Value> for DataSchema {
    type Error = AnyError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        data_schema_from_value(&value)
    }
}

#[allow(clippy::from_over_into)]
impl Into<Value> for DataSchema {
    fn into(self) -> Value {
        data_value_from_schema(self)
    }
}

pub fn data_value_from_schema(schema: DataSchema) -> Value {
    match schema {
        DataSchema::Object(_, schemas) => {
            let map = convert_data_schema_to_map(schemas);
            let mut serialized = HashMap::new();
            serialized.insert("properties", map);
            serde_json::to_value(serialized).unwrap()
        }
        _ => {
            json!("{}")
        }
    }
}

fn convert_data_schema_to_map(schemas: Vec<DataSchema>) -> HashMap<String, Value> {
    let mut properties = HashMap::new();
    for schema in schemas {
        match schema {
            Empty => {}
            DataSchema::String(name, constraints) => {
                let mut v = serde_json::to_value(constraints).unwrap();
                v.as_object_mut().and_then(|map| {
                    map.insert("type".to_string(), serde_json::to_value("string").unwrap())
                });
                properties.insert(name, v);
            }
            DataSchema::Integer(name, constraints) => {
                let mut v = serde_json::to_value(constraints).unwrap();
                v.as_object_mut().and_then(|map| {
                    map.insert("type".to_string(), serde_json::to_value("integer").unwrap())
                });
                properties.insert(name, v);
            }
            DataSchema::Object(name, schemax) => {
                let serialized_obj = convert_data_schema_to_map(schemax);
                let mut map = HashMap::new();
                map.insert("type", json!("object"));
                map.insert("properties", serde_json::to_value(serialized_obj).unwrap());
                properties.insert(name.unwrap(), serde_json::to_value(map).unwrap());
            }
        }
    }
    properties
}

pub fn data_schema_from_value(schema: &Value) -> AnyResult<DataSchema> {
    trace!("generating DataSchema from: {:?}", schema);
    let data_schema = schema
        .get("properties")
        .and_then(|prop| prop.as_object())
        .ok_or("properties not found")
        .map_err(anyhow::Error::msg)
        .and_then(parse_properties)
        .map(|data_schema| DataSchema::Object(Option::None, data_schema));
    trace!("generated DataSchema: {:?}", &data_schema);
    data_schema
}

fn parse_properties(properties: &Map<String, Value>) -> AnyResult<Vec<DataSchema>> {
    let mut data = vec![];
    for (k, v) in properties.iter() {
        let type_ = v
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("type not found for {}", k))
            .map_err(AnyError::msg)?;
        match type_ {
            "object" => data.push(DataSchema::Object(
                Option::Some(k.clone()),
                parse_properties(
                    v.get("properties")
                        .and_then(Value::as_object)
                        .ok_or_else(|| format!("object properties not found for : {}", k))
                        .map_err(AnyError::msg)?,
                )?,
            )),
            "string" => data.push(DataSchema::String(
                k.clone(),
                get_constraints(v)
                    .ok_or_else(|| format!("invalid constraints: {:?}", v))
                    .map_err(AnyError::msg)?,
            )),
            "integer" => data.push(DataSchema::Integer(
                k.clone(),
                get_constraints(v)
                    .ok_or_else(|| format!("invalid constraints: {:?}", v))
                    .map_err(AnyError::msg)?,
            )),
            _ => {}
        }
    }
    Ok(data)
}

fn get_constraints(property: &Value) -> Option<HashMap<Keywords, Constraints>> {
    let mut constraints = HashMap::new();
    for (k, v) in property.as_object()?.iter() {
        match k.as_str() {
            "minimum" => {
                constraints.insert(Keywords::Minimum, Constraints::Minimum(v.as_i64()?));
            }
            "maximum" => {
                constraints.insert(Keywords::Maximum, Constraints::Maximum(v.as_i64()?));
            }
            "minLength" => {
                constraints.insert(Keywords::MinLength, Constraints::MinLength(v.as_i64()?));
            }
            "maxLength" => {
                constraints.insert(Keywords::MaxLength, Constraints::MaxLength(v.as_i64()?));
            }
            "constant" => {
                let constraint = if let Some(val) = v.as_i64() {
                    Constraints::ConstantInt(val)
                } else {
                    Constraints::ConstantStr(String::from(v.as_str()?))
                };
                constraints.insert(Keywords::Constant, constraint);
            }
            "pattern" => {
                let pattern = v.as_str()?;
                let hir = Parser::new().parse(pattern).ok()?;
                constraints.insert(
                    Keywords::Pattern,
                    Constraints::Pattern(String::from(pattern), Some(hir)),
                );
            }
            _ => {}
        };
    }
    Some(constraints)
}

/// Return JSON object with randomly generated data
/// Not returning result, because a valid [DataSchema] should return valid data
pub fn generate_data(schema: &DataSchema) -> Value {
    match schema {
        DataSchema::Object(k, v) => {
            if k.is_some() {
                error!("key should be None for root object");
                Value::Null
            } else {
                Value::Object(generate_object_data(v))
            }
        }
        _ => {
            error!(
                "invalid schema data generator, have to be object, but found: {:?}",
                schema
            );
            Value::Null
        }
    }
}

fn generate_object_data(data: &[DataSchema]) -> Map<String, Value> {
    let mut map = Map::new();
    for datum in data {
        match datum {
            DataSchema::Empty => {}
            DataSchema::String(k, c) => {
                map.insert(k.clone(), Value::String(generate_string_data(c)));
            }
            DataSchema::Integer(k, c) => {
                map.insert(k.clone(), json!(generate_integer_data(c)));
            }
            DataSchema::Object(k, c) => {
                map.insert(
                    k.as_ref().unwrap().clone(),
                    Value::Object(generate_object_data(c)),
                );
            }
        }
    }
    map
}

fn generate_integer_data(constraints: &HashMap<Keywords, Constraints>) -> i64 {
    if let Some(constraint) = constraints.get(&Keywords::Constant) {
        return constraint.as_i64().unwrap_or(i64::MIN);
    }
    let min = constraints
        .get(&Keywords::Minimum)
        .and_then(|constraint| constraint.as_i64())
        .unwrap_or(0);
    let max = constraints
        .get(&Keywords::Maximum)
        .and_then(|constraint| constraint.as_i64())
        .unwrap_or(100);
    fastrand::i64(min..=max)
}

fn generate_string_data(constraints: &HashMap<Keywords, Constraints>) -> String {
    if let Some(constraint) = constraints.get(&Keywords::Constant) {
        return constraint
            .as_str()
            .unwrap_or("Error: invalid constant constraint")
            .into();
    }
    if let Some(constraint) = constraints.get(&Keywords::Pattern) {
        if let Some((pattern, hir)) = constraint.as_pattern() {
            let mut rng = thread_rng();
            let mut buffer: Vec<u8> = vec![];
            return match hir.as_ref() {
                Some(hir) => generate_from_hir(&mut buffer, hir, &mut rng, PATTER_MAX_REPEAT)
                    .ok()
                    .and_then(|_| String::from_utf8(buffer).ok())
                    .unwrap_or(format!("Couldn't generate string for: {}", pattern)),
                None => {
                    log::warn!("Hir not pre-generated for pattern constraint");
                    Parser::new()
                        .parse(pattern)
                        .ok()
                        .and_then(|ir| {
                            generate_from_hir(&mut buffer, &ir, &mut rng, PATTER_MAX_REPEAT).ok()
                        })
                        .and_then(|_| String::from_utf8(buffer).ok())
                        .unwrap_or(format!("Couldn't generate string for: {}", pattern))
                }
            };
        }
    }
    let min = constraints
        .get(&Keywords::MinLength)
        .and_then(|constraint| constraint.as_i64())
        .unwrap_or(1);
    let max = constraints
        .get(&Keywords::MaxLength)
        .and_then(|constraint| constraint.as_i64())
        .unwrap_or(20);
    let size = fastrand::i64(min..=max);
    let data: String = std::iter::repeat_with(fastrand::alphabetic)
        .take(size as usize)
        .collect();
    data
}

#[cfg(test)]
mod test {
    use crate::datagen::{data_schema_from_value, generate_data};
    use serde_json::Value;

    #[test]
    fn test() {
        let data = r#"{
            "properties":
            {
                "firstName":
                {
                    "type": "string",
                    "description": "The person's first name."
                },
                "lastName":
                {
                    "type": "string",
                    "description": "The person's last name."
                },
                "age":
                {
                    "description": "Age in years which must be equal to or greater than zero.",
                    "type": "integer",
                    "minimum": 10,
                    "maximum": 11
                }
            }
        }"#;
        let schema: Value = serde_json::from_str(data).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        let _ = generate_data(result);
    }

    #[test]
    fn test_ser_deser() {
        let data = r#"{
            "properties":
            {
                "firstName":
                {
                    "type": "string",
                    "description": "The person's first name."
                },
                "lastName":
                {
                    "type": "string",
                    "description": "The person's last name."
                },
                "age":
                {
                    "description": "Age in years which must be equal to or greater than zero.",
                    "type": "integer",
                    "minimum": 10,
                    "maximum": 11
                },
                "nested": {
                    "type":"object",
                    "properties": {
                        "nestedProp":{
                            "type": "string",
                            "description": "a nested property description."
                        }
                    }
                }
            }
        }"#;
        let schema: super::DataSchema = serde_json::from_str(data).unwrap();
        println!("deserialized: {}", serde_json::to_string(&schema).unwrap());
        assert_json_diff::assert_json_include!(
            actual: serde_json::from_str::<Value>(data).unwrap(),
            expected: serde_json::from_str::<Value>(serde_json::to_string(&schema).unwrap().as_str())
                .unwrap()
        );
    }

    #[test]
    fn test_integer_constant() {
        let schema = r#"{"properties":{"sample":{"type":"integer","constant":10}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        let value = generate_data(result);
        assert_eq!(value.get("sample").unwrap().as_i64().unwrap(), 10);
    }

    #[test]
    fn test_string_constant() {
        let schema = r#"{"properties":{"sample":{"type":"string","constant":"always produce this string"}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        let value = generate_data(result);
        assert_eq!(
            value.get("sample").unwrap().as_str().unwrap(),
            "always produce this string"
        );
    }

    #[test]
    fn test_string_pattern() {
        let regex = regex::Regex::new(r"^\d{4}-\w+-\d{2}$").unwrap();
        let schema = r#"{"properties":{"sample":{"type":"string","pattern":"^[0-9]{4}-[a-z]{2,}-[0-9]{2}$"}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        let value = generate_data(result);
        assert!(regex.is_match(value.get("sample").unwrap().as_str().unwrap()));
    }

    #[test]
    fn test_object() {
        let schema = r#"{"properties":{"sample":{"constant":10,"type":"integer"},"sampleObject":{"properties":{"nestedSample":{"constant":20,"type":"integer"}},"type":"object"}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        let value = generate_data(result);
        assert_eq!(value.get("sample").unwrap().as_i64().unwrap(), 10);
        println!("{}", serde_json::to_string(&value).unwrap());
        let object = value.get("sampleObject").unwrap();
        assert_eq!(object.get("nestedSample").unwrap().as_i64().unwrap(), 20);
    }
}
