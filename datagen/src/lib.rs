use std::collections::HashMap;
use std::convert::TryFrom;
use std::string::ToString;

use anyhow::{anyhow, Error as AnyError, Result as AnyResult};
use hashbrown::HashMap as HashBrownMap;
use log::{error, trace, warn};
use rand::thread_rng;
use regex_generate::generate_from_hir;
use regex_syntax::hir::Hir;
use regex_syntax::Parser;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{json, Map, Value};

pub use external_data::ExternalData;

use crate::Constraints::DataFile;
use crate::DataSchema::Empty;

mod external_data;

const PATTER_MAX_REPEAT: u32 = 10;
const TYPE_DATA: &str = "type";
const KEY_FOR_KEYLESS_SCHEMAS: &str = "__";

#[derive(Debug, Serialize, Deserialize, Eq, PartialEq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
pub enum Keywords {
    MinLength,
    MaxLength,
    Minimum,
    Maximum,
    Constant,
    Pattern,
    DataFile,
}

impl Keywords {
    pub fn as_str(&self) -> &'static str {
        match self {
            Keywords::MinLength => "minLength",
            Keywords::MaxLength => "maxLength",
            Keywords::Minimum => "minimum",
            Keywords::Maximum => "maximum",
            Keywords::Constant => "constant",
            Keywords::Pattern => "pattern",
            Keywords::DataFile => "dataFile",
        }
    }
}

impl TryFrom<&String> for Keywords {
    type Error = ();
    fn try_from(value: &String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "minLength" => Ok(Keywords::MinLength),
            "maxLength" => Ok(Keywords::MaxLength),
            "minimum" => Ok(Keywords::Minimum),
            "maximum" => Ok(Keywords::Maximum),
            "constant" => Ok(Keywords::Constant),
            "pattern" => Ok(Keywords::Pattern),
            "dataFile" => Ok(Keywords::DataFile),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum Constraints {
    MinLength(i64),
    MaxLength(i64),
    Minimum(i64),
    Maximum(i64),
    ConstantInt(i64),
    ConstantStr(String),
    ConstantValue(Value),
    #[serde(serialize_with = "pattern_serializer")]
    Pattern(String, #[serde(skip_deserializing)] Option<Hir>),
    DataFile(u64),
}

fn pattern_serializer<S>(
    pattern: &str,
    _hir: &Option<Hir>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(pattern)
}

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

    fn as_serde_value(&self) -> Option<&Value> {
        match *self {
            Constraints::ConstantValue(ref x) => Some(x),
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
    ConstantObject(Option<String>, HashMap<Keywords, Constraints>),
    Array(
        Option<String>,
        Vec<DataSchema>,
        HashMap<Keywords, Constraints>,
    ),
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

#[allow(clippy::derivable_impls)]
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
                    map.insert(
                        TYPE_DATA.to_string(),
                        serde_json::to_value("string").unwrap(),
                    )
                });
                properties.insert(name, v);
            }
            DataSchema::Integer(name, constraints) => {
                let mut v = serde_json::to_value(constraints).unwrap();
                v.as_object_mut().and_then(|map| {
                    map.insert(
                        TYPE_DATA.to_string(),
                        serde_json::to_value("integer").unwrap(),
                    )
                });
                properties.insert(name, v);
            }
            DataSchema::Object(name, schema) => {
                let serialized_obj = convert_data_schema_to_map(schema);
                let mut map = HashMap::new();
                map.insert(TYPE_DATA, json!("object"));
                map.insert("properties", serde_json::to_value(serialized_obj).unwrap());
                properties.insert(name.unwrap(), serde_json::to_value(map).unwrap());
            }
            DataSchema::Array(name, schema, mut constraints) => {
                let mut map = HashMap::new();
                map.insert(TYPE_DATA, json!("array"));
                let mut items = convert_data_schema_to_map(schema);
                // map.insert("items", serde_json::to_value(items).unwrap());
                map.insert(
                    "items",
                    items.remove(KEY_FOR_KEYLESS_SCHEMAS).unwrap_or(json!("{}")),
                );
                for (keyword, constraint) in constraints.drain() {
                    map.insert(keyword.as_str(), serde_json::to_value(constraint).unwrap());
                }
                properties.insert(name.unwrap(), serde_json::to_value(map).unwrap());
            }
            DataSchema::ConstantObject(k, constraints) => {
                let mut v = serde_json::to_value(constraints).unwrap();
                v.as_object_mut().and_then(|map| {
                    map.insert(
                        TYPE_DATA.to_string(),
                        serde_json::to_value("object").unwrap(),
                    )
                });
                properties.insert(k.unwrap(), v);
            }
        }
    }
    properties
}

pub fn data_schema_from_value(schema: &Value) -> AnyResult<DataSchema> {
    trace!(
        "generating DataSchema from: {}",
        serde_json::to_string(schema).unwrap()
    );
    let data_schema = schema
        .get("properties")
        .and_then(|prop| prop.as_object())
        .ok_or("properties not found")
        .map_err(anyhow::Error::msg)
        .and_then(parse_properties)
        .map(|data_schema| DataSchema::Object(Option::None, data_schema));
    if let Err(e) = &data_schema {
        error!("[data_schema_from_value] - parsing error: {}", e);
    }
    trace!("generated DataSchema: {:?}", &data_schema);
    data_schema
}

fn parse_properties(properties: &Map<String, Value>) -> AnyResult<Vec<DataSchema>> {
    let mut data = vec![];
    for (k, v) in properties.iter() {
        let type_ = v
            .get(TYPE_DATA)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("type not found for {}", k))
            .map_err(AnyError::msg)?;
        match type_ {
            "object" => {
                if v.get("properties").is_some() {
                    data.push(DataSchema::Object(
                        Option::Some(k.clone()),
                        parse_properties(
                            v.get("properties")
                                .and_then(Value::as_object)
                                .ok_or_else(|| format!("object properties not found for : {}", k))
                                .map_err(AnyError::msg)?,
                        )?,
                    ))
                } else if v.get(Keywords::Constant.as_str()).is_some() {
                    data.push(DataSchema::ConstantObject(
                        Some(k.clone()),
                        HashMap::from([(
                            Keywords::Constant,
                            Constraints::ConstantValue(
                                v.get(Keywords::Constant.as_str()).unwrap().clone(),
                            ),
                        )]),
                    ))
                } else {
                    return Err(AnyError::msg("object should have properties or constant"));
                }
            }
            "string" => data.push(DataSchema::String(k.clone(), get_constraints(v)?)),
            "integer" => data.push(DataSchema::Integer(k.clone(), get_constraints(v)?)),
            "array" => data.push(DataSchema::Array(
                Some(k.clone()),
                {
                    let mut map = Map::with_capacity(1);
                    parse_properties(
                        v.get("items")
                            .map(|v| {
                                map.insert(KEY_FOR_KEYLESS_SCHEMAS.to_string(), v.clone());
                                &map
                            })
                            .ok_or_else(|| format!("array items not found for : {}", k))
                            .map_err(AnyError::msg)?,
                    )?
                },
                get_constraints(v)?,
            )),
            _ => {}
        }
    }
    Ok(data)
}

fn get_constraints(property: &Value) -> AnyResult<HashMap<Keywords, Constraints>> {
    let mut constraints = HashMap::new();
    parse_constraints(property, &mut constraints)
        .ok_or_else(|| AnyError::msg(format!("invalid constraints: {:?}", property.to_string())))?;
    validate_constraints(&constraints, property)?;
    Ok(constraints)
}

fn validate_constraints(
    constraints: &HashMap<Keywords, Constraints>,
    properties: &Value,
) -> AnyResult<()> {
    // validated range
    let min = constraints.get(&Keywords::Minimum);
    let max = constraints.get(&Keywords::Maximum);
    if min.is_some() != max.is_some() {
        return Err(anyhow!(format!(
            "requires both minimum & maximum constraints - {}",
            properties
        )));
    }
    if let Some(min) = min {
        if let Some(max) = max {
            if max.as_i64() < min.as_i64() {
                return Err(anyhow!(format!(
                    "invalid constraints - minimum > maximum - {}",
                    properties
                )));
            }
        }
    }

    // validated length
    let min = constraints.get(&Keywords::MinLength);
    let max = constraints.get(&Keywords::MaxLength);
    if min.is_some() != max.is_some() {
        return Err(anyhow!(format!(
            "requires both minLength & maxLength constraints - {}",
            properties
        )));
    }
    if let Some(min) = min {
        if let Some(max) = max {
            if max.as_i64() < min.as_i64() {
                return Err(anyhow!(format!(
                    "invalid constraints - minLength > maxLength - {}",
                    properties
                )));
            }
        }
    }
    Ok(())
}

fn parse_constraints(
    property: &Value,
    constraints: &mut HashMap<Keywords, Constraints>,
) -> Option<()> {
    for (k, v) in property.as_object()?.iter() {
        let keyword = Keywords::try_from(k);
        if let Ok(keyword) = keyword {
            match keyword {
                Keywords::Minimum => {
                    constraints.insert(keyword, Constraints::Minimum(v.as_i64()?));
                }
                Keywords::Maximum => {
                    constraints.insert(keyword, Constraints::Maximum(v.as_i64()?));
                }
                Keywords::MinLength => {
                    constraints.insert(keyword, Constraints::MinLength(v.as_i64()?));
                }
                Keywords::MaxLength => {
                    constraints.insert(keyword, Constraints::MaxLength(v.as_i64()?));
                }
                Keywords::Constant => {
                    let constraint = if let Some(val) = v.as_i64() {
                        Constraints::ConstantInt(val)
                    } else if v.is_string() {
                        Constraints::ConstantStr(String::from(v.as_str()?))
                    } else {
                        Constraints::ConstantValue(v.clone())
                    };
                    constraints.insert(keyword, constraint);
                }
                Keywords::Pattern => {
                    let pattern = v.as_str()?;
                    let hir = Parser::new().parse(pattern).ok()?;
                    constraints.insert(
                        keyword,
                        Constraints::Pattern(String::from(pattern), Some(hir)),
                    );
                }
                Keywords::DataFile => {
                    constraints.insert(keyword, DataFile(v.as_u64()?));
                }
            };
        }
    }
    Some(())
}

#[cfg(feature = "data-gen-file")]
pub fn generate_data_with_external_data(schema: &DataSchema) -> (Value, Option<Vec<ExternalData>>) {
    match schema {
        DataSchema::Object(k, v) => {
            if k.is_some() {
                error!("key should be None for root object");
                (Value::Null, None)
            } else {
                let (data, ext_data) = generate_object_data(v);
                (Value::Object(data), ext_data)
            }
        }
        _ => {
            error!(
                "invalid schema data generator, have to be object, but found: {:?}",
                schema
            );
            (Value::Null, None)
        }
    }
}

/// Return JSON object with randomly generated data
/// Not returning result, because a valid [DataSchema] should return valid data
pub fn generate_data(schema: &DataSchema) -> (Value, Option<Vec<ExternalData>>) {
    generate_data_with_external_data(schema)
}

fn generate_object_data(data: &[DataSchema]) -> (Map<String, Value>, Option<Vec<ExternalData>>) {
    let mut map = Map::new();
    let mut external_data_desc: HashBrownMap<ExternalData, usize> = HashBrownMap::new();
    for datum in data {
        match datum {
            DataSchema::Empty => {}
            DataSchema::String(k, c) => {
                let (str_data, external) = generate_string_data(c);
                map.insert(k.clone(), Value::String(str_data));
                if external {
                    let ext_data = ExternalData::Object {
                        path: k.clone(),
                        count: 1,
                    };
                    let count = external_data_desc.entry_ref(&ext_data).or_insert(0);
                    *count += 1;
                    // external_data_desc.insert(ExternalData::Object { path: k.clone(), count: 1 });
                }
            }
            DataSchema::Integer(k, c) => {
                map.insert(k.clone(), json!(generate_integer_data(c).0));
            }
            DataSchema::Object(k, c) => {
                let (obj_data, external_data) = generate_object_data(c);
                map.insert(k.as_ref().unwrap().clone(), Value::Object(obj_data));
                if let Some(mut external_data) = external_data {
                    let updated_path = k.as_ref().unwrap().as_str().to_owned() + ".";
                    for external_datum in external_data.iter_mut() {
                        if let ExternalData::Object {
                            ref mut path,
                            count: _,
                        } = external_datum
                        {
                            path.insert_str(0, &updated_path);
                        }
                        external_data_desc.entry_ref(external_datum).or_insert(0);
                    }
                }
            }
            DataSchema::Array(k, schema, constraints) => {
                let (data, mut ext_data) = generate_array_data(schema, constraints);
                let k = k.as_ref().unwrap().clone();
                if let Some(ExternalData::Array(ref mut key, _)) = ext_data {
                    *key = k.clone() + key;
                }
                if let Some(d) = ext_data {
                    // external_data_desc.insert(d);
                    let count = external_data_desc.entry_ref(&d).or_insert(0);
                    *count += 1;
                };
                map.insert(k, Value::Array(data));
            }
            DataSchema::ConstantObject(k, c) => {
                map.insert(
                    k.as_ref().unwrap().clone(),
                    c.get(&Keywords::Constant)
                        .and_then(|c| c.as_serde_value())
                        .cloned()
                        .unwrap_or(json!({})),
                );
            }
        }
    }
    let external_data_desc = if external_data_desc.is_empty() {
        None
    } else {
        Some(external_data_map_to_vec(&mut external_data_desc))
    };
    (map, external_data_desc)
}

#[inline]
fn external_data_map_to_vec(map: &mut HashBrownMap<ExternalData, usize>) -> Vec<ExternalData> {
    map.drain().fold(Vec::new(), |mut b, (mut d, c)| {
        d.set_object_count(c);
        b.push(d);
        b
    })
}

fn generate_array_data(
    schema: &[DataSchema],
    constraints: &HashMap<Keywords, Constraints>,
) -> (Vec<Value>, Option<ExternalData>) {
    let min = constraints
        .get(&Keywords::MinLength)
        .and_then(|constraint| constraint.as_i64())
        .unwrap_or(1);
    let max = constraints
        .get(&Keywords::MaxLength)
        .and_then(|constraint| constraint.as_i64())
        .unwrap_or(20);
    let len = fastrand::i64(min..=max);
    let mut array_values = vec![];
    // let mut ext_data = Vec::new();
    let mut ext_data: HashBrownMap<ExternalData, usize> = HashBrownMap::new();
    for _ in 0..len {
        let (mut data, ext) = generate_object_data(schema);
        array_values.push(data.remove(KEY_FOR_KEYLESS_SCHEMAS).unwrap());
        if let Some(ext) = ext {
            for datum in ext {
                let x = ext_data.entry_ref(&datum).or_insert(0);
                *x += 1;
            }
        }
    }
    let ext_data = if ext_data.is_empty() {
        None
    } else {
        Some(ExternalData::Array(
            "[*]".to_string(),
            external_data_map_to_vec(&mut ext_data),
        ))
    };
    (array_values, ext_data)
}

fn generate_integer_data(constraints: &HashMap<Keywords, Constraints>) -> (i64, bool) {
    if let Some(constraint) = constraints.get(&Keywords::Constant) {
        return (constraint.as_i64().unwrap_or(i64::MIN), false);
    }
    if let Some(constraint) = constraints.get(&Keywords::Pattern) {
        if let Some((pattern, hir)) = constraint.as_pattern() {
            let rand_int = random_string(pattern, hir);
            return (
                rand_int.as_str().parse::<i64>().unwrap_or_else(|_| {
                    warn!("invalid pattern {} for integer", pattern);
                    0
                }),
                false,
            );
        }
        return (constraint.as_i64().unwrap_or(i64::MIN), false);
    }
    // let data = try_to_get_data_from_file(constraints, tx, rx)
    //     .and_then(|val| val.parse::<i64>().ok());
    // if let Some(data) = data {
    //     return (-1, true);
    // }
    let min = constraints
        .get(&Keywords::Minimum)
        .and_then(|constraint| constraint.as_i64())
        .unwrap_or(0);
    let max = constraints
        .get(&Keywords::Maximum)
        .and_then(|constraint| constraint.as_i64())
        .unwrap_or(100);
    (fastrand::i64(min..=max), false)
}

fn random_string(pattern: &String, hir: &Option<Hir>) -> String {
    let mut rng = thread_rng();
    let mut buffer: Vec<u8> = vec![];

    match hir.as_ref() {
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
    }
}

fn generate_string_data(constraints: &HashMap<Keywords, Constraints>) -> (String, bool) {
    if let Some(constraint) = constraints.get(&Keywords::Constant) {
        return (
            constraint
                .as_str()
                .unwrap_or("Error: invalid constant constraint")
                .into(),
            false,
        );
    }
    if let Some(constraint) = constraints.get(&Keywords::Pattern) {
        if let Some((pattern, hir)) = constraint.as_pattern() {
            let rand_str = random_string(pattern, hir);
            return (rand_str, false);
        }
    }

    if let Some(DataFile(column)) = constraints.get(&Keywords::DataFile) {
        return (column.to_string(), true);
    }

    // if let Some(value) = try_to_get_data_from_file(constraints, tx, rx).await {
    //     return (value, true);
    // }
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
    (data, false)
}

// fn try_to_get_data_from_file(
//     constraints: &HashMap<Keywords, Constraints>,
//     tx: &ReqSender,
//     rx: &mut DataReceiver,
// ) -> Option<String> {
//     if let Some(DataFile(column)) = constraints.get(&Keywords::DataFile) {
//         if let Some(tx) = tx {
//             if tx.send(1).await.is_ok() {
//                 let val = timeout(DURATION_DATA_FILE_TIMEOUT, rx.as_mut().unwrap().recv())
//                     .await
//                     .ok()
//                     .flatten()
//                     .and_then(|mut str_from_file| str_from_file.pop())
//                     .and_then(|mut str_from_file| {
//                         let column = *column as usize;
//                         if column < str_from_file.len() {
//                             Some(str_from_file.swap_remove(column))
//                         } else {
//                             None
//                         }
//                     });
//                 if let Some(val) = val {
//                     return Some(val);
//                 } else {
//                     error!("unable to get data from file");
//                 }
//             }
//         }
//     }
//     None
// }

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::sync::Once;

    use log::info;
    use regex::Regex;
    use serde_json::{json, Value};
    use tokio::sync::mpsc::{Receiver, Sender};

    use crate::external_data::ExternalData;
    use crate::{
        data_schema_from_value, generate_data, generate_data_with_external_data, Constraints,
        DataSchema, Keywords, KEY_FOR_KEYLESS_SCHEMAS,
    };

    static INIT: Once = Once::new();

    pub fn init_logger() {
        INIT.call_once(|| {
            env_logger::init_from_env(
                env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "trace"),
            );
        });
    }

    fn validate_json_path<'a>(
        path: &str,
        json: &'a Value,
        expected: Option<&Value>,
    ) -> Vec<&'a Value> {
        let mut selector = jsonpath_lib::selector(json);
        let result = selector(path);
        assert!(result.is_ok());
        let result = result.unwrap();
        if let Some(expected) = expected {
            assert_json_diff::assert_json_eq!(result.first().unwrap(), expected);
        }
        result
    }

    #[test]
    fn json_schema_to_data_schema_and_back() {
        let schemas = vec![
            const_obj_data_gen_schema as fn() -> &'static str,
            const_obj_data_gen_with_other_obj_property_schema,
            integer_pattern_data_gen_schema,
            array_sample_json_3,
            array_sample_json_2,
            array_sample_json_1,
            with_all_feature_sample_schema,
        ];
        for schema in schemas {
            let json_schema = schema();
            let original_json_schema: Value = serde_json::from_str(json_schema).unwrap();
            let data_schema = data_schema_from_value(&original_json_schema).unwrap();
            let converted_json_schema = serde_json::to_value(data_schema).unwrap();
            assert_json_diff::assert_json_eq!(converted_json_schema, original_json_schema);
        }
    }

    #[test]
    fn integer_pattern_data_gen() {
        let json_schema = integer_pattern_data_gen_schema();
        let json_schema: Value = serde_json::from_str(json_schema).unwrap();
        let data_schema = data_schema_from_value(&json_schema).unwrap();
        let data = generate_data(&data_schema).0;
        let val = validate_json_path("$.nested.objKey1", &data, None);
        info!("[integer_pattern_data_gen] - val: {:?}", val);
        assert_ne!(val.first().unwrap().as_i64().unwrap(), 0)
    }

    fn integer_pattern_data_gen_schema() -> &'static str {
        r#"
        {
          "properties": {
            "key1": {
              "type": "string",
              "constant": "value1"
            },
            "nested": {
              "type": "object",
              "properties": {
                "objKey1": {
                  "type": "integer",
                  "pattern": "^1[0-9]{4}$"
                },
                "objKey2": {
                  "type": "string",
                  "pattern": "^a[0-9]{4}z$"
                }
              }
            }
          }
        }
        "#
    }

    #[test]
    fn const_obj_data_gen() {
        let json_schema = const_obj_data_gen_schema();
        let json_schema: Value = serde_json::from_str(json_schema).unwrap();
        let data_schema = data_schema_from_value(&json_schema).unwrap();
        let data = generate_data(&data_schema).0;
        validate_json_path(
            "$.constObject.properties.comment",
            &data,
            Some(&json!("this will be parsed as json instead of schema")),
        );
    }

    #[test]
    fn const_obj_data_gen_with_other_obj_property() {
        let json_schema = const_obj_data_gen_with_other_obj_property_schema();
        let json_schema: Value = serde_json::from_str(json_schema).unwrap();
        let data_schema = data_schema_from_value(&json_schema).unwrap();
        let data = generate_data(&data_schema).0;
        validate_json_path("$.key1", &data, Some(&json!("value1")));
        validate_json_path("$.nested.objKey1", &data, None);
        validate_json_path("$.constObject.keyInt", &data, Some(&json!(1234)));
    }

    fn const_obj_data_gen_with_other_obj_property_schema() -> &'static str {
        r#"
        {
          "properties": {
            "key1": {
              "type": "string",
              "constant": "value1"
            },
            "nested": {
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
            },
            "constObject": {
              "type": "object",
              "constant": {
                "key1": "val1",
                "properties": {
                  "comment": "this will be parsed as json instead of schema"
                },
                "keyInt": 1234
              }
            }
          }
        }
        "#
    }

    fn with_all_feature_sample_schema() -> &'static str {
        r#"
        {
          "properties": {
            "key1": {
              "type": "string",
              "constant": "value1"
            },
            "nested": {
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
            },
            "constObject": {
              "type": "object",
              "constant": {
                "key1": "val1",
                "properties": {
                  "comment": "this will be parsed as json instead of schema"
                },
                "keyInt": 1234
              }
            }
          }
        }
        "#
    }

    fn const_obj_data_gen_schema() -> &'static str {
        r#"
        {
          "properties": {
            "constObject": {
              "type": "object",
              "constant": {
                "key1": "val1",
                "properties": {
                  "comment": "this will be parsed as json instead of schema"
                },
                "keyInt": 1234
              }
            }
          }
        }
        "#
    }

    #[test]
    fn array_json_to_schema() {
        let data = array_sample_json_1();
        let schema: Value = serde_json::from_str(data).unwrap();
        let schema: DataSchema = TryFrom::try_from(schema).unwrap();
        match schema {
            DataSchema::Object(_, values) => {
                assert_eq!(values.len(), 1);
                let array = values.first().unwrap();
                match array {
                    DataSchema::Array(k, values, c) => {
                        assert_eq!(k, &Some("keys".to_string()));
                        assert_eq!(values.len(), 1);
                        let elements = values.first().unwrap();
                        matches!(elements, DataSchema::Integer(_, _));
                        assert_eq!(
                            c.get(&Keywords::MinLength).unwrap(),
                            &Constraints::MinLength(2)
                        );
                        assert_eq!(
                            c.get(&Keywords::MaxLength).unwrap(),
                            &Constraints::MaxLength(10)
                        );
                    }
                    _ => panic!("expected array, got: {:?}", array),
                }
            }
            _ => panic!("expected object, got: {:?}", schema),
        }
    }

    #[test]
    fn array_schema_to_value() {
        let data = array_sample_json_1();
        let original_schema: Value = serde_json::from_str(data).unwrap();
        let schema: DataSchema = TryFrom::try_from(original_schema.clone()).unwrap();
        let value: Value = schema.into();
        assert_json_diff::assert_json_eq!(value, original_schema);
    }

    #[test]
    fn array_schema_to_value_string_pattern() {
        init_logger();
        let data = array_sample_json_3();
        let original_schema: Value = serde_json::from_str(data).unwrap();
        let schema: DataSchema = TryFrom::try_from(original_schema.clone()).unwrap();
        let value: Value = schema.into();
        info!(
            "schema to value: {}",
            serde_json::to_string(&value).unwrap()
        );
        assert_json_diff::assert_json_eq!(value, original_schema);
    }

    #[test]
    fn array_schema_data_gen_integer() {
        let data = array_sample_json_1();
        let schema: DataSchema = serde_json::from_str(data).unwrap();
        let value = generate_data(&schema).0;
        matches!(value, Value::Object(_));
        let array = value.get("keys").unwrap();
        matches!(array, &Value::Array(_));
        let array = array.as_array().unwrap();
        (2..=10usize).contains(&array.len());
        let arr_val_range = 1..=100i64;
        for elm in array {
            arr_val_range.contains(&elm.as_i64().unwrap());
        }
    }

    #[test]
    fn array_schema_data_gen_object() {
        let data = array_sample_json_2();
        let schema: DataSchema = serde_json::from_str(data).unwrap();
        let value = generate_data(&schema).0;
        matches!(value, Value::Object(_));
        let array = value.get("keys").unwrap();
        matches!(array, &Value::Array(_));
        let array = array.as_array().unwrap();
        (2..=10usize).contains(&array.len());

        let key1_val_range = 1..=100i64;
        for elm in array {
            key1_val_range.contains(&elm.get("key1").unwrap().as_i64().unwrap());
            let key2 = i64::from_str(elm.get("key2").unwrap().as_str().unwrap()).unwrap();
            assert!(key2 > 1);
            assert!(key2 < (1999999 - 1))
        }
    }

    #[test]
    fn array_schema_data_gen_string_pattern() {
        let data = array_sample_json_3();
        let schema: DataSchema = serde_json::from_str(data).unwrap();
        let value = generate_data(&schema).0;
        matches!(value, Value::Object(_));
        let array = value.get("objArrayKeys").unwrap();
        matches!(array, &Value::Array(_));

        let key1_val_range = 10..=1000i64;
        let regex = Regex::new("^a[0-9]{4}z$").unwrap();
        for elm in array.as_array().unwrap() {
            key1_val_range.contains(&elm.get("objKey1").unwrap().as_i64().unwrap());
            let key2 = elm.get("objKey2").unwrap().as_str().unwrap();
            assert!(regex.is_match(key2));
        }

        let array = value.get("scalarArray").unwrap();
        matches!(array, &Value::Array(_));
        let array = array.as_array().unwrap();
        assert_eq!(array.len(), 20);
        for elm in array {
            let key = elm.as_str().unwrap();
            assert!(regex.is_match(key));
        }
    }

    fn array_sample_json_3() -> &'static str {
        r#"
        {
          "properties": {
            "firstName": {
              "type": "string"
            },
            "lastName": {
              "type": "string",
                "pattern": "^a[0-9]{4}z$"
            },
            "age": {
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
        }
        "#
    }

    fn array_sample_json_2() -> &'static str {
        r#"
        {
          "properties":{
            "keys": {
              "type": "array",
              "items": {
                "type": "object",
                "properties": {
                  "key1": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100
                  },
                  "key2": {
                    "type": "string",
                    "pattern":"^1[0-9]{6}$"
                  }
                }
              },
              "maxLength": 10,
              "minLength": 2
            }
          }
        }
        "#
    }

    fn array_sample_json_1() -> &'static str {
        r#"
        {
          "properties":{
            "keys": {
              "type": "array",
              "items": {
                "type": "integer",
                "minimum": 1,
                "maximum": 100
              },
              "maxLength": 10,
              "minLength": 2
            }
          }
        }
        "#
    }

    #[test]
    fn failure_test_min_max_range() {
        let data = r#"
        {
          "type": "object",
          "properties": {
            "keys": {
              "type": "array",
              "maxLength": 3,
              "minLength": 2,
              "items": {
                "type": "object",
                "properties": {
                  "key1": {
                    "type": "integer",
                    "minimum": 300000,
                    "maximum": 200000
                  },
                  "key2": {
                    "type": "string",
                    "pattern": "^a[0-9]{4}z$"
                  }
                }
              }
            }
          }
        }
        "#;
        let schema = serde_json::from_str::<DataSchema>(data);
        assert!(schema.is_err());
        assert!(schema
            .err()
            .unwrap()
            .to_string()
            .contains("minimum > maximum"));
    }

    #[test]
    fn failure_test_min_max_length() {
        let data = r#"
        {
          "type": "object",
          "properties": {
            "keys": {
              "type": "array",
              "maxLength": 1,
              "minLength": 2,
              "items": {
                "type": "object",
                "properties": {
                  "key1": {
                    "type": "integer",
                    "minimum": 100000,
                    "maximum": 200000
                  },
                  "key2": {
                    "type": "string",
                    "pattern": "^a[0-9]{4}z$"
                  }
                }
              }
            }
          }
        }
        "#;
        let schema = serde_json::from_str::<DataSchema>(data);
        assert!(schema.is_err());
        assert!(schema
            .err()
            .unwrap()
            .to_string()
            .contains("minLength > maxLength"));
    }

    #[test]
    fn failure_test_min_missing_range() {
        let data = r#"
        {
          "type": "object",
          "properties": {
            "keys": {
              "type": "array",
              "maxLength": 3,
              "minLength": 2,
              "items": {
                "type": "object",
                "properties": {
                  "key1": {
                    "type": "integer",
                    "maximum": 200000
                  },
                  "key2": {
                    "type": "string",
                    "pattern": "^a[0-9]{4}z$"
                  }
                }
              }
            }
          }
        }
        "#;
        let schema = serde_json::from_str::<DataSchema>(data);
        assert!(schema.is_err());
        assert!(schema
            .err()
            .unwrap()
            .to_string()
            .contains("requires both minimum & maximum"));
    }

    #[test]
    fn failure_test_max_missing_range() {
        let data = r#"
        {
          "type": "object",
          "properties": {
            "keys": {
              "type": "array",
              "maxLength": 3,
              "minLength": 2,
              "items": {
                "type": "object",
                "properties": {
                  "key1": {
                    "type": "integer",
                    "minimum": 200000
                  },
                  "key2": {
                    "type": "string",
                    "pattern": "^a[0-9]{4}z$"
                  }
                }
              }
            }
          }
        }
        "#;
        let schema = serde_json::from_str::<DataSchema>(data);
        assert!(schema.is_err());
        assert!(schema
            .err()
            .unwrap()
            .to_string()
            .contains("requires both minimum & maximum"));
    }

    #[test]
    fn failure_test_max_missing_length() {
        let data = r#"
        {
          "type": "object",
          "properties": {
            "keys": {
              "type": "array",
              "minLength": 2,
              "items": {
                "type": "object",
                "properties": {
                  "key1": {
                    "type": "integer",
                    "maximum": 200000,
                    "minimum": 200000
                  },
                  "key2": {
                    "type": "string",
                    "pattern": "^a[0-9]{4}z$"
                  }
                }
              }
            }
          }
        }
        "#;
        let schema = serde_json::from_str::<DataSchema>(data);
        assert!(schema.is_err());
        assert!(schema
            .err()
            .unwrap()
            .to_string()
            .contains("requires both minLength & maxLength"));
    }

    #[test]
    fn failure_test_min_missing_length() {
        let data = r#"
        {
          "type": "object",
          "properties": {
            "keys": {
              "type": "array",
              "maxLength": 2,
              "items": {
                "type": "object",
                "properties": {
                  "key1": {
                    "type": "integer",
                    "maximum": 200000,
                    "minimum": 200000
                  },
                  "key2": {
                    "type": "string",
                    "pattern": "^a[0-9]{4}z$"
                  }
                }
              }
            }
          }
        }
        "#;
        let schema = serde_json::from_str::<DataSchema>(data);
        assert!(schema.is_err());
        assert!(schema
            .err()
            .unwrap()
            .to_string()
            .contains("requires both minLength & maxLength"));
    }

    #[test]
    fn test() {
        let data = r#"{
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
                    "dataFile": 1
                  },
                  "objKey2": {
                    "type": "string",
                    "pattern": "^a[0-9]{4}z$"
                  },
                  "objKey3": {
                    "type": "string",
                    "dataFile": 2
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
        }"#;
        let schema: Value = serde_json::from_str(data).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        let g = generate_data(result).0;
        println!("{g}")
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
        assert_json_diff::assert_json_include!(
            actual: serde_json::from_str::<Value>(data).unwrap(),
            expected: serde_json::from_str::<Value>(serde_json::to_string(&schema).unwrap().as_str())
                .unwrap()
        );
    }

    #[test]
    fn test_integer_constant() {
        init_logger();
        let schema = r#"{"properties":{"sample":{"type":"integer","constant":10}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        info!("schema: {:?}", &result);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        let value = generate_data(result).0;
        info!("{:?}", &value.to_string());
        assert_eq!(value.get("sample").unwrap().as_i64().unwrap(), 10);
    }

    // #[test]
    // fn test_integer_data_file() {
    //     let schema = r#"{"properties":{"sample":{"type":"integer","dataFile":1}}}"#;
    //     let schema: Value = serde_json::from_str(schema).unwrap();
    //     let result = data_schema_from_value(&schema);
    //     assert!(result.is_ok());
    //     let result = result.as_ref().unwrap();
    //     let (mut rx, rtx) = get_rx_tx().await;
    //     let value = generate_data_with_external_data(result, &rtx, &mut rx)
    //         .await
    //         .0;
    //     assert_eq!(value.get("sample").unwrap().as_i64().unwrap(), 5);
    // }

    #[test]
    fn test_string_constant() {
        let schema = r#"{"properties":{"sample":{"type":"string","constant":"always produce this string"}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        let value = generate_data(result).0;
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
        let value = generate_data(result).0;
        assert!(regex.is_match(value.get("sample").unwrap().as_str().unwrap()));
    }

    // #[test]
    // fn test_string_data_file() {
    //     let schema = r#"{"properties":{"sample":{"type":"string","dataFile":1}}}"#;
    //     let schema: Value = serde_json::from_str(schema).unwrap();
    //     let result = data_schema_from_value(&schema);
    //     assert!(result.is_ok());
    //     let result = result.as_ref().unwrap();
    //     let (mut rx, rtx) = get_rx_tx().await;
    //     // let value = generate_data(result).await;
    //     let value = generate_data_with_external_data(result, &rtx, &mut rx)
    //         .await
    //         .0;
    //     assert_eq!(value.get("sample").unwrap().as_str().unwrap(), "5");
    // }

    type ReqSender = Option<Sender<usize>>;
    type DataReceiver = Option<Receiver<Vec<Vec<String>>>>;

    #[allow(dead_code)]
    async fn get_rx_tx() -> (DataReceiver, ReqSender) {
        let (tx, rx): (_, Receiver<Vec<Vec<String>>>) = tokio::sync::mpsc::channel(10);
        let (rtx, mut rrx): (Sender<usize>, _) = tokio::sync::mpsc::channel(10);
        tokio::spawn(async move {
            while let Some(n) = rrx.recv().await {
                let v = vec!["1".to_string(), "5".to_string()];
                let mut vv = Vec::with_capacity(n);
                for _ in 0..n {
                    vv.push(v.clone());
                }
                tx.send(vv).await.unwrap();
            }
        });
        (Some(rx), Some(rtx))
    }

    #[test]
    fn test_object() {
        let schema = r#"{"properties":{"sample":{"constant":10,"type":"integer"},"sampleObject":{"properties":{"nestedSample":{"constant":20,"type":"integer"}},"type":"object"}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        let value = generate_data(result).0;
        assert_eq!(value.get("sample").unwrap().as_i64().unwrap(), 10);
        let object = value.get("sampleObject").unwrap();
        assert_eq!(object.get("nestedSample").unwrap().as_i64().unwrap(), 20);
    }

    #[test]
    fn test_external_data_description_depth_1() {
        let schema = r#"{"properties":{"sample":{"type":"string","dataFile":1}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        // let (mut rx, rtx) = get_rx_tx().await;
        // let value = generate_data(result).await;
        let value = generate_data_with_external_data(result);
        let value = value.1.unwrap();
        println!("{:?}", value);
        let expect = ExternalData::Object {
            path: "sample".to_string(),
            count: 1,
        };
        assert_eq!(value.first().unwrap(), &expect);
    }

    #[test]
    fn test_external_data_description_depth_2() {
        let schema = r#"{"properties":{"sample":{"type":"object","properties":{"sample":{"type":"string","dataFile":1}}}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        // let (mut rx, rtx) = get_rx_tx().await;
        // let value = generate_data(result).await;
        let value = generate_data_with_external_data(result);
        let value = value.1.unwrap();
        println!("{:?}", value);
        let expect = ExternalData::Object {
            path: "sample.sample".to_string(),
            count: 1,
        };
        assert_eq!(value.first().unwrap(), &expect);
    }

    #[test]
    fn test_external_data_description_depth_3() {
        let schema = r#"{"properties":{"sample":{"type":"object","properties":{"sample":{"type":"object","properties":{"sample":{"type":"string","dataFile":1}}}}}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        // let (mut rx, rtx) = get_rx_tx().await;
        // let value = generate_data(result).await;
        let value = generate_data_with_external_data(result);
        let value = value.1.unwrap();
        println!("{:?}", value);
        let expect = ExternalData::Object {
            path: "sample.sample.sample".to_string(),
            count: 1,
        };
        assert_eq!(value.first().unwrap(), &expect);
    }

    #[test]
    fn test_multi_external_data_description_depth_1() {
        let schema = r#"{"properties":{"sample":{"type":"string","dataFile":1}, "sample2":{"type":"string","dataFile":2}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        let value = generate_data_with_external_data(result);
        let value = value.1.unwrap();
        println!("{:?}", value);
        let expect_1 = ExternalData::Object {
            path: "sample".to_string(),
            count: 1,
        };
        let expect_2 = ExternalData::Object {
            path: "sample2".to_string(),
            count: 1,
        };
        assert!(value.contains(&expect_1));
        assert!(value.contains(&expect_2));
    }

    #[test]
    fn test_arr_external_data_description_depth_1() {
        init_logger();
        let schema = r#"{"properties":{"keys":{"type":"array","items":{"type": "string", "dataFile":1},"maxLength":10,"minLength":2}}}"#;
        let schema: Value = serde_json::from_str(schema).unwrap();
        let result = data_schema_from_value(&schema);
        assert!(result.is_ok());
        let result = result.as_ref().unwrap();
        let (value, ext_data) = generate_data_with_external_data(result);
        let ext_data = ext_data.unwrap();
        let expect = ExternalData::Array(
            "keys[*]".to_string(),
            vec![ExternalData::Object {
                path: String::from(KEY_FOR_KEYLESS_SCHEMAS),
                count: 0,
            }],
        );
        assert_eq!(ext_data.len(), 1);
        let ext_data = ext_data.first().unwrap();
        assert_eq!(ext_data, &expect);
        info!(
            "generated json data: {}, external data: {:?}",
            value, ext_data
        );
        if let ExternalData::Object { path: _, count } = ext_data {
            assert_eq!(value.get("keys").unwrap().as_array().unwrap().len(), *count);
        }
    }
}
