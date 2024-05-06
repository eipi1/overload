use anyhow::Error as AnyError;
use lazy_static::lazy_static;
use log::trace;
use regex::Regex;
use rhai::{Engine, AST};
use serde_json::Value;
use std::collections::HashMap;

// pub trait TemplateFn {
//     type Return;
//     type Error;
//     fn interpret() -> Result<Self::Return, Self::Error>;
// }

lazy_static! {
    static ref FUNCTION_PATTERN: Regex = Regex::new(r#"\{\{(.+?)}}"#).unwrap();
    static ref NESTED_FUNCTION_PATTERN: Regex = Regex::new(r#"(.+?)\((.*?)\)"#).unwrap();
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
            if let Some(ast) = find_pattern_and_compile(&FUNCTION_PATTERN, str_val, &engine) {
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

// struct ParseErrFn {}
//
// impl TemplateFn for ParseErrFn {
//     type Return = String;
//     type Error = AnyError;
//
//     fn interpret() -> Result<Self::Return, Self::Error> {
//         Ok("Invalid template".to_string())
//     }
// }
//
// struct ToStrFn<T: TemplateFn> {
//     inner: T,
// }
//
// impl<T: TemplateFn> TemplateFn for ToStrFn<T> {
//     type Return = ();
//     type Error = ();
//
//     fn interpret() -> Result<Self::Return, Self::Error> {
//         todo!()
//     }
// }

#[cfg(test)]
mod tests {
    use crate::template::{find_pattern_and_compile, parse_templates, FUNCTION_PATTERN};
    use crate::test::init_logger;
    use log::info;
    use rhai::{Dynamic, Engine, AST};
    use serde_json::Value;
    use std::collections::HashMap;

    #[test]
    fn test_ast_compile() {
        init_logger();
        let engine = Engine::new();
        let ast = find_pattern_and_compile(&FUNCTION_PATTERN, "{{random(10, 100)}}", &engine);
        let ast = engine.compile(format!("`{}`", "some error"));
        info!("{:?}", engine.eval_ast::<Dynamic>(&ast.unwrap()));
    }

    #[test]
    fn test_eval_1() {
        init_logger();
        let mut engine = Engine::new();
        fn random_int(l: i64, h: i64) -> i64 {
            fastrand::i64(l..h)
        }
        engine.register_fn("randomInt", random_int);
        let json = test_parse_template_2_json();
        let json: Value = serde_json::from_str(json).unwrap();
        let mut path = "".to_string();
        let mut map = HashMap::new();
        parse_templates(&engine, &json, &mut path, &mut map);
        for (path, ast) in &map {
            let i = engine.eval_ast::<i64>(ast).unwrap();
            info!("{path}: {i}");
        }
        info!("{:?}", &map);
    }

    #[test]
    fn test_eval_2() {
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
        let mut path = "".to_string();
        let mut map = HashMap::new();
        parse_templates(&engine, &json, &mut path, &mut map);

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
        let mut path = "".to_string();
        let mut map = HashMap::new();
        parse_templates(&engine, &json, &mut path, &mut map);
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
        let mut path = "".to_string();
        let mut map = HashMap::new();
        parse_templates(&engine, &json, &mut path, &mut map);
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
        let mut path = "".to_string();
        let mut map = HashMap::new();
        parse_templates(&engine, &json, &mut path, &mut map);
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
}
