use lazy_static::lazy_static;
use log::trace;
use rand::thread_rng;
use regex::Regex;
use rhai::packages::{Package, StandardPackage};
use rhai::{Dynamic, Engine, AST};
use serde_json::{json, Value};
use std::cmp::max;
use std::collections::HashMap;

lazy_static! {
    static ref PATTERN_FUNCTION: Regex = Regex::new(r#"\{\{(.+?)}}"#).unwrap();
    // static ref PATTERN_FUNCTION_NAME_WITH_REGEX: Regex = Regex::new(r#".+_pattern.*?\("#).unwrap();
}

pub fn parse_templates(
    engine: &Engine,
    json: &Value,
    path: &str,
    template_map: &mut HashMap<String, AST>,
) {
    match json {
        Value::Null => {}
        Value::Bool(_) => {}
        Value::Number(_) => {}
        Value::String(str_val) => {
            if let Some(ast) = find_pattern_and_compile(&PATTERN_FUNCTION, str_val, engine) {
                trace!(
                    "[parse_templates] - input: {}, Rhai AST: {:?}",
                    str_val,
                    ast
                );
                template_map.insert(path.to_string(), ast);
            }
        }
        Value::Array(vals) => {
            for (pos, val) in vals.iter().enumerate() {
                let new_path = format!("{path}/{pos}");
                parse_templates(engine, val, &new_path, template_map);
            }
        }
        Value::Object(vals) => {
            for (key, val) in vals {
                let new_path = format!("{path}/{key}");
                parse_templates(engine, val, &new_path, template_map);
            }
        }
    }
}

fn find_pattern_and_compile(regex: &Regex, target: &str, engine: &Engine) -> Option<AST> {
    let captures = regex.captures(target);
    trace!(
        "[parse_templates] - text: {target}, captures: {:?}",
        &captures
    );
    if let Some(captures) = captures {
        if captures.len() > 1 {
            let expr = captures.get(1).unwrap().as_str();
            let ast = engine
                .compile(expr)
                .unwrap_or_else(|e| engine.compile(format!("`{}`", e)).unwrap());
            return Some(ast);
        }
    }
    None
}

pub fn populate_data(engine: &Engine, json: &mut Value, template_map: &HashMap<String, AST>) {
    for (path, ast) in template_map {
        if let Some(val) = json.pointer_mut(path) {
            let eval_result = engine
                .eval_ast::<Dynamic>(ast)
                .map_err(|e| json!(e.to_string()))
                .and_then(|v| serde_json::to_value(v).map_err(|e| json!(e.to_string())))
                .unwrap_or_else(|e| e);
            *val = eval_result;
        }
    }
}

pub fn build_engine() -> Engine {
    // Create a raw scripting Engine
    let mut engine = Engine::new_raw();

    // Enable the strings interner
    engine.set_max_strings_interned(1024);

    // Register the Standard Package
    let package = StandardPackage::new();

    // Load the package into the [`Engine`]
    package.register_into_engine(&mut engine);
    register_template_functions(&mut engine);
    engine
}

fn register_template_functions(engine: &mut Engine) {
    fn random_int(l: i64, h: i64) -> i64 {
        fastrand::i64(l..h)
    }

    fn random_bool() -> bool {
        fastrand::bool()
    }

    fn random_float(l: f64, h: f64) -> f64 {
        fastrand_contrib::f64_range(l..h)
    }

    fn random_str(min_len: i64, max_len: i64) -> String {
        let min_len = max(1, min_len);
        let max_len = max(min_len, max_len);
        let size = fastrand::i64(min_len..=max_len);
        let data: String = std::iter::repeat_with(fastrand::alphabetic)
            .take(size as usize)
            .collect();
        data
    }

    fn random_str_pattern(regex: &str) -> String {
        regex_generate::Generator::parse(regex, thread_rng())
            .map_err(|e| e.to_string())
            .and_then(|mut g| {
                let mut buffer: Vec<u8> = vec![];
                g.generate(&mut buffer)
                    .map_err(|e| e.to_string())
                    .map(|_| buffer)
            })
            .and_then(|v| String::from_utf8(v).map_err(|e| e.to_string()))
            .unwrap_or_else(|e| e)
    }

    fn to_str<T: ToString>(val: T) -> String {
        val.to_string()
    }

    fn to_int<T: ToString>(int_str: T) -> Dynamic {
        int_str
            .to_string()
            .parse::<i64>()
            .map_err(|e| e.to_string())
            .map(Dynamic::from_int)
            .unwrap_or_else(Dynamic::from)
    }

    engine.register_fn("randomInt", random_int);
    engine.register_fn("randomBool", random_bool);
    engine.register_fn("randomFloat", random_float);
    engine.register_fn("randomStr", random_str);
    engine.register_fn("randomStrWithPattern", random_str_pattern);
    engine.register_fn("toStr", to_str::<i64>);
    engine.register_fn("toStr", to_str::<bool>);
    engine.register_fn("toStr", to_str::<f64>);
    engine.register_fn("toStr", to_str::<String>);
    engine.register_fn("toStr", to_str::<Dynamic>);
    engine.register_fn("toInt", to_int::<String>);
    engine.register_fn("toInt", to_int::<Dynamic>);
}

#[cfg(test)]
mod tests {
    use crate::template::{
        build_engine, find_pattern_and_compile, parse_templates, populate_data, PATTERN_FUNCTION,
    };
    use crate::test::init_logger;
    use log::info;
    use regex::Regex;
    use rhai::{Dynamic, Engine, AST};
    use serde_json::{json, Value};
    use std::collections::HashMap;

    #[test]
    fn test_ast_compile() {
        init_logger();
        let engine = Engine::new();
        let _ast = find_pattern_and_compile(&PATTERN_FUNCTION, "{{random(10, 100)}}", &engine);
        let ast = engine.compile(format!("`{}`", "some error"));
        info!("{:?}", engine.eval_ast::<Dynamic>(&ast.unwrap()));
    }

    #[test]
    fn test_populate_data() {
        init_logger();

        let engine = build_engine();

        let json = test_json_template_1();
        let mut json: Value = serde_json::from_str(json).unwrap();
        let path = "".to_string();
        let mut map = HashMap::new();
        parse_templates(&engine, &json, &path, &mut map);
        info!("{:?}", &map);
        populate_data(&engine, &mut json, &map);
        info!("{:?}", &json);

        let value = json.pointer("/duration").unwrap();
        let value = value.as_i64().unwrap();
        assert!((100..200).contains(&value));

        let value = json.pointer("/name").unwrap();
        let value = value.as_str().unwrap();
        let regex = Regex::new("[a-zA-Z0-9_]{4,10}").unwrap();
        assert!(regex.is_match(value));

        let value = json.pointer("/qps/ConstantRate/countPerSec").unwrap();
        let value = value.as_i64().unwrap();
        assert!((10000..=19999).contains(&value));

        let value = json.pointer("/req/RequestList/data/0/sendHeader").unwrap();
        assert!(value.is_boolean());

        let value = json.pointer("/target/host").unwrap();
        let value = value.as_str().unwrap();
        let regex = Regex::new("[a-zA-Z]{5,10}").unwrap();
        assert!(regex.is_match(value));

        let value = json.pointer("/histogramBuckets/2").unwrap();
        let value = value.as_i64().unwrap();
        assert!((50..60).contains(&value));

        let value = json.pointer("/histogramBuckets/3").unwrap();
        let value = value.as_i64().unwrap();
        assert!((70..90).contains(&value));

        let value = json.pointer("/histogramBuckets/5").unwrap();
        let value = value.as_i64().unwrap();
        assert!((150..180).contains(&value));

        let value = json.pointer("/generation_mode/batch/batchSize").unwrap();
        let value = value.as_f64().unwrap();
        assert!((10.4..20.5).contains(&value));
    }

    fn test_json_template_1() -> &'static str {
        r#"
            {
              "duration": "{{randomInt(100, 200)}}",
              "name": "demo-test-{{randomStrWithPattern(\"[a-zA-Z0-9_]{4,10}\")}}",
              "qps": {
                "ConstantRate": {
                  "countPerSec": "{{toInt(randomStrWithPattern(\"1[0-9]{4}\"))}}"
                }
              },
              "req": {
                "RequestList": {
                  "data": [
                    {
                      "method": "GET",
                      "url": "example.com",
                      "sendHeader": "{{randomBool()}}"
                    }
                  ]
                }
              },
              "target": {
                "host": "{{randomStr(5,10)}}.com",
                "port": 8080,
                "protocol": "HTTP"
              },
              "histogramBuckets": [
                35,
                40,
                "{{randomInt(50, 60)}}",
                "{{randomInt(70, 90)}}",
                120,
                "{{randomInt(150, 180)}}"
              ],
              "generation_mode": {
                "batch": {
                  "batchSize": "{{randomFloat(10.4, 20.5)}}"
                }
              }
            }
        "#
    }

    #[test]
    fn test_eval_2() {
        init_logger();

        let engine = build_engine();

        let json = test_parse_template_2_json();
        let mut json: Value = serde_json::from_str(json).unwrap();
        let path = "".to_string();
        let mut map = HashMap::new();
        parse_templates(&engine, &json, &path, &mut map);
        info!("{:?}", &map);
        populate_data(&engine, &mut json, &map);
        info!("{:?}", &json);

        let value = json.pointer("/qps/ConstantRate/countPerSec").unwrap();
        let value = value.as_i64().unwrap();
        assert!((150..200).contains(&value));

        let value = json.pointer("/duration").unwrap();
        let value = value.as_i64().unwrap();
        assert!((100..200).contains(&value));
    }

    #[test]
    fn test_eval_3() {
        init_logger();
        let mut engine = Engine::new();
        fn random_int(l: i64, h: i64) -> i64 {
            fastrand::i64(l..h)
        }

        fn to_str<T: ToString>(val: T) -> String {
            val.to_string()
        }
        engine.register_fn("toString", to_str::<i64>);
        engine.register_fn("randomInt", random_int);
        let json = test_parse_template_3_json();
        let json: Value = serde_json::from_str(json).unwrap();
        let path = "".to_string();
        let mut map = HashMap::new();
        parse_templates(&engine, &json, &path, &mut map);

        for (path, ast) in &map {
            let i = engine.eval_ast::<Dynamic>(ast).unwrap();
            if path.eq_ignore_ascii_case("/name") {
                info!("{path}: {:?}", &i);
                assert!(i.is_string());
            }
        }
        info!("{:?}", &map);
    }

    #[test]
    fn test_parse_template_3() {
        init_logger();
        let engine = Engine::new();
        let json = crate::template::tests::test_parse_template_3_json();
        let json: Value = serde_json::from_str(json).unwrap();
        let path = "".to_string();
        let mut map = HashMap::new();
        parse_templates(&engine, &json, &path, &mut map);
        info!("{:?}", &map);
        assert_eq!(map.len(), 2);
        assert!(matches!(map.get("/name"), Some(AST { .. })));
        assert!(matches!(
            map.get("/qps/ConstantRate/countPerSec"),
            Some(AST { .. })
        ));
    }

    fn test_parse_template_3_json() -> &'static str {
        r#"
            {
              "duration": 123,
              "name": "{{toString(randomInt(150, 200))}}",
              "qps": {
                "ConstantRate": {
                  "countPerSec": "{{randomInt(150, 200)}}"
                }
              }
            }
        "#
    }

    #[test]
    fn test_parse_template_2() {
        init_logger();
        let engine = Engine::new();
        let json = test_parse_template_2_json();
        let json: Value = serde_json::from_str(json).unwrap();
        let path = "".to_string();
        let mut map = HashMap::new();
        parse_templates(&engine, &json, &path, &mut map);
        info!("{:?}", &map);
        assert_eq!(map.len(), 2);
        assert!(matches!(map.get("/duration"), Some(AST { .. })));
        assert!(matches!(
            map.get("/qps/ConstantRate/countPerSec"),
            Some(AST { .. })
        ));
    }

    fn test_parse_template_2_json() -> &'static str {
        r#"
            {
              "duration": "{{randomInt(100, 200)}}",
              "name": "demo-test",
              "qps": {
                "ConstantRate": {
                  "countPerSec": "{{randomInt(150, 200)}}"
                }
              }
            }
        "#
    }

    #[test]
    fn test_parse_template_1() {
        let engine = Engine::new();
        let json = test_parse_template_1_json();
        let json: Value = serde_json::from_str(json).unwrap();
        let path = "".to_string();
        let mut map = HashMap::new();
        parse_templates(&engine, &json, &path, &mut map);
        assert_eq!(map.len(), 1);
        assert!(matches!(map.get("/duration"), Some(AST { .. })));
    }

    fn test_parse_template_1_json() -> &'static str {
        r#"
            {
              "duration": "{{randomInt(100, 200)}}"
            }
        "#
    }

    #[test]
    fn test_dynamic_to_json() {
        let dynamic = Dynamic::from_int(10);
        let result = serde_json::to_value(dynamic);
        dbg!(&result);
        assert_eq!(result.unwrap(), json!(10));
    }
}
