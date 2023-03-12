use log::{debug, error};
use mlua::{FromLua, Function, Lua, RegistryKey, Table, Value};

static LUA_DEF_JSON_PARSER: &str = include_str!("json.lua");

#[derive(Debug)]
pub struct LuaAssertionResult {
    pub assertion_id: i32,
    pub assertion_result: Result<(), String>,
}

impl LuaAssertionResult {
    fn lua_error(error: String) -> Self {
        Self {
            assertion_id: i32::MAX,
            assertion_result: Err(error),
        }
    }
}

impl FromLua<'_> for LuaAssertionResult {
    fn from_lua(lua_value: Value, _lua: &Lua) -> mlua::Result<Self> {
        match lua_value {
            Value::Table(table) => {
                let id = table.raw_get::<_, i32>("id")?;
                let mut result = Ok(());
                if !table.raw_get::<_, bool>("success")? {
                    result = Err(table.raw_get::<_, String>("error")?);
                }
                Ok(Self {
                    assertion_id: id,
                    assertion_result: result,
                })
            }
            _ => Err(mlua::Error::FromLuaConversionError {
                from: lua_value.type_name(),
                to: "LuaAssertionResult",
                message: Some("expected table".to_string()),
            }),
        }
    }
}

pub fn init_lua() -> Lua {
    let lua = Lua::new();
    let f: Table = lua.load(LUA_DEF_JSON_PARSER).eval().unwrap();
    lua.globals().set("json", f).unwrap();
    lua
}

pub fn load_lua_func<'lua>(lua_chunk: &str, lua: &'lua Lua) -> Option<Function<'lua>> {
    debug!("loading lua module from");
    let result = lua
        .load(lua_chunk)
        .eval::<Function>()
        .map_err(|e| error!("Failed to load lua function - {}", e))
        .ok();
    result
}

pub fn load_lua_func_with_registry(lua_chunk: &str, lua: &Lua) -> Option<RegistryKey> {
    debug!("loading lua module from");
    let result = lua
        .load(lua_chunk)
        .eval::<Function>()
        .map_err(|e| error!("Failed to load lua function:{}, error: {}", lua_chunk, e))
        .ok()
        .and_then(|func| {
            lua.create_registry_value(func)
                .map_err(|e| error!("Failed to create registry value - {}", e))
                .ok()
        });
    result
}

pub fn verify_lua_chunk(chunk: &str) -> Result<(), String> {
    let lua = Lua::new();
    let result = lua.load(chunk).eval::<Function>();
    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn call_lua_func_from_registry(
    lua: &Lua,
    func_key: &RegistryKey,
    method: &str,
    url: String,
    req_body: Option<String>,
    resp_body: String,
) -> Result<(), Vec<LuaAssertionResult>> {
    match lua.registry_value::<Function>(func_key) {
        Ok(func) => func
            .call::<_, Vec<LuaAssertionResult>>((method, url, req_body, resp_body))
            .map_err(|e| {
                error!("Error calling lua function: {}", e);
                Vec::new()
            })
            .and_then(|r| if r.is_empty() { Ok(()) } else { Err(r) }),
        Err(e) => {
            let e = e.to_string();
            log::error!(
                "Failed to get lua function using registry key, error: {}",
                &e
            );
            Err(vec![LuaAssertionResult::lua_error(e)])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::info;
    use mlua::{Function, Lua};
    use serde_json::json;
    use std::sync::Once;

    #[allow(dead_code)]
    static INIT: Once = Once::new();

    #[allow(dead_code)]
    pub fn init_logger() {
        INIT.call_once(|| {
            env_logger::init_from_env(
                env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
            );
        });
    }

    #[test]
    fn lua_init() {
        let lua = init_lua();
        let test_json: Function = lua
            .load(
                r#"
                function(s)
                    jsonVal=json.decode(s)
                    return jsonVal.hello .. " from lua"
                end
        "#,
            )
            .eval()
            .unwrap();

        assert_eq!(
            test_json
                .call::<_, String>("{\"hello\":\"world\"}".to_string())
                .unwrap(),
            "world from lua"
        );
    }

    #[test]
    fn call_lua_func() {
        env_logger::init_from_env(
            env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
        );
        let _json = json!({ "id": "1001", "type": "Regular" });
        let lua_txt = r#"
        function(status, response)
          jsonVal=json.decode(response)
          rVal={}
          rVal.id=jsonVal.id
          rVal.success=false
          rVal.error="Test error"
          err2 = {}
          err2.id=123
          err2.success=true
          return {rVal, err2}
        end
        "#;
        let lua = init_lua();
        let _func = load_lua_func(lua_txt, &lua).unwrap();
        // let result =
        //     super::call_lua_func::<Vec<LuaAssertionResult>, _>(func, (200, json.to_string()))
        //         .unwrap();
        // info!("{:?}", result);
        // let result1 = result.get(0).unwrap();
        // assert_eq!(result1.assertion_id, 1001);
        // assert!(result1.assertion_result.is_err());
        // let result1 = result.get(1).unwrap();
        // assert_eq!(result1.assertion_id, 123);
        // assert!(result1.assertion_result.is_ok());
    }

    #[test]
    fn lua_validate() {
        let lua_txt = r#"
        function(status, response)
          jsonVal=json.decode(response)
          rVal={}
          rVal.id=jsonVal.id
          rVal.success=false
          rVal.error="Test error"
          err2 = {}
          err2.id=123
          err2.success=true
          return {rVal, err2}
        end
        "#;
        let lua = Lua::new();
        let result = lua.load(lua_txt).eval::<Function>();
        assert!(result.is_ok())
    }

    #[test]
    fn lua_table() {
        let lua = init_lua();
        let test_json: Function = lua
            .load(
                r#"
                function(s)
                    jsonVal=json.decode(s)
                    return jsonVal
                end
        "#,
            )
            .eval()
            .unwrap();

        let json = sample_json();
        let lua_table = test_json.call::<_, Table>(json.to_string()).unwrap();
        let v = lua_table
            .get::<_, Table>(1)
            .unwrap()
            .get::<_, String>("id")
            .unwrap();
        assert_eq!(v, "0001".to_string())
    }

    fn sample_json() -> serde_json::Value {
        json!(
        [
            {
                "id": "0001",
                "type": "donut",
                "name": "Cake",
                "ppu": 0.55,
                "batters":
                    {
                        "batter":
                            [
                                { "id": "1001", "type": "Regular" },
                                { "id": "1002", "type": "Chocolate" },
                                { "id": "1003", "type": "Blueberry" },
                                { "id": "1004", "type": "Devil's Food" }
                            ]
                    },
                "topping":
                    [
                        { "id": "5001", "type": "None" },
                        { "id": "5002", "type": "Glazed" },
                        { "id": "5005", "type": "Sugar" },
                        { "id": "5007", "type": "Powdered Sugar" },
                        { "id": "5006", "type": "Chocolate with Sprinkles" },
                        { "id": "5003", "type": "Chocolate" },
                        { "id": "5004", "type": "Maple" }
                    ]
            },
            {
                "id": "0002",
                "type": "donut",
                "name": "Raised",
                "ppu": 0.55,
                "batters":
                    {
                        "batter":
                            [
                                { "id": "1001", "type": "Regular" }
                            ]
                    },
                "topping":
                    [
                        { "id": "5001", "type": "None" },
                        { "id": "5002", "type": "Glazed" },
                        { "id": "5005", "type": "Sugar" },
                        { "id": "5003", "type": "Chocolate" },
                        { "id": "5004", "type": "Maple" }
                    ]
            },
            {
                "id": "0003",
                "type": "donut",
                "name": "Old Fashioned",
                "ppu": 0.55,
                "batters":
                    {
                        "batter":
                            [
                                { "id": "1001", "type": "Regular" },
                                { "id": "1002", "type": "Chocolate" }
                            ]
                    },
                "topping":
                    [
                        { "id": "5001", "type": "None" },
                        { "id": "5002", "type": "Glazed" },
                        { "id": "5003", "type": "Chocolate" },
                        { "id": "5004", "type": "Maple" }
                    ]
            }
        ])
    }

    #[test]
    fn test_lua() {
        let lua_txt = r#"
        function(x)
          return 2*x
        end
        "#;
        let lua = Lua::new();
        let lua_twice: Function = lua.load(lua_txt).eval().unwrap();
        assert_eq!(lua_twice.call::<_, u32>(5).unwrap(), 10);
    }

    #[test]
    fn test_call_lua_func_from_registry() {
        let lua = init_lua();
        let lua_txt = r#"
        function(method, url, reqBody, respBody)
          result ={}
          if method~="POST" then
            local err={}
            err.id=1
            err.success=false
            err.error="Invalid method"
            table.insert(result,err)
          end
          jsonReq=json.decode(reqBody)
          jsonResp=json.decode(respBody)
          for k,v in pairs(jsonReq.keys) do
            if jsonResp[k].id ~= v then
                local err = {}
                err.id=2
                err.success=false
                err.error="invalid value "..jsonResp[k].id.." at "..k
                table.insert(result,err)
            end
          end
          for k,v in pairs(result) do
             print(k)
             print(v)
          end
          return result
        end
        "#;
        let func_key = load_lua_func_with_registry(lua_txt, &lua).unwrap();
        let req = json!(
            {
                "keys":[
                  "0001",
                  "0002",
                  "0004"
                ]
            }
        )
        .to_string();
        let result = super::call_lua_func_from_registry(
            &lua,
            &func_key,
            "GET",
            "/some/path".to_string(),
            Some(req),
            sample_json().to_string(),
        );
        info!("{:?}", result);
        assert!(result.is_err());
        let result = result.unwrap_err();
        assert_eq!(result[0].assertion_id, 1);
        assert_eq!(result[1].assertion_id, 2);
    }
}
