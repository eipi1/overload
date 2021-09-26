use crate::datagen::DataSchema::Empty;
use anyhow::{Error as AnyError, Result as AnyResult};
use log::error;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Eq, PartialEq, Hash, Clone)]
pub enum Keywords {
    MinLength,
    MaxLength,
    Minimum,
    Maximum,
    Constant,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Constraints {
    MinLength(i64),
    MaxLength(i64),
    Minimum(i64),
    Maximum(i64),
    ConstantInt(i64),
    ConstantStr(String),
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
}

#[derive(Debug, Serialize, Deserialize, Clone)]
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

pub fn data_schema_from_value(schema: &Value) -> AnyResult<DataSchema> {
    log::trace!("generating DataSchema from: {:?}",schema);
    schema
        .get("properties")
        .and_then(|prop| prop.as_object())
        .ok_or("properties not found")
        .map_err(anyhow::Error::msg)
        .and_then(|properties| parse_properties(properties))
        .map(|data_schema| DataSchema::Object(Option::None, data_schema))
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
    use serde_json::{ Value};

    #[test]
    fn test() {
        let data = r#"{
    "$id": "https://example.com/person.schema.json",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "title": "Person",
    "type": "object",
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
        let value = generate_data(&result);
        println!("{:?}", result);
        println!("{:?}", value);
    }
}
