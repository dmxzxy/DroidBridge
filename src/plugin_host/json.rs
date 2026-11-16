//! serde_json 与 Lua 值互转。
//!
//! 规则：有连续整数键 1..n 的表编码为数组；空表编码为 `{}`（对象）。

use mlua::{Lua, Result, Table, Value};
use serde_json::{Map, Number, Value as J};

/// Lua 侧表示 JSON null 的哨兵字符串（`__host.null`）
pub const NULL_SENTINEL: &str = "\u{1}db.null";

/// JSON → Lua
pub fn to_lua(lua: &Lua, v: &J) -> Result<Value> {
    Ok(match v {
        J::Null => Value::Nil,
        J::Bool(b) => Value::Boolean(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Number(n.as_f64().unwrap_or(0.0)),
        },
        J::String(s) => Value::String(lua.create_string(s)?),
        J::Array(a) => {
            let t = lua.create_table_with_capacity(a.len(), 0)?;
            for (i, x) in a.iter().enumerate() {
                t.set(i + 1, to_lua(lua, x)?)?;
            }
            Value::Table(t)
        }
        J::Object(o) => {
            let t = lua.create_table_with_capacity(0, o.len())?;
            for (k, x) in o {
                t.set(k.as_str(), to_lua(lua, x)?)?;
            }
            Value::Table(t)
        }
    })
}

/// Lua → JSON
pub fn to_json(v: &Value) -> J {
    match v {
        Value::Nil => J::Null,
        Value::Boolean(b) => J::Bool(*b),
        Value::Integer(i) => J::Number(Number::from(*i)),
        Value::Number(n) => Number::from_f64(*n).map(J::Number).unwrap_or(J::Null),
        Value::String(s) => {
            let text = s.to_string_lossy();
            if text == NULL_SENTINEL {
                J::Null
            } else {
                J::String(text.to_string())
            }
        }
        Value::Table(t) => table_to_json(t),
        Value::Error(e) => J::String(format!("{e}")),
        _ => J::Null,
    }
}

fn table_to_json(t: &Table) -> J {
    let len = t.raw_len();
    let mut pairs = Vec::new();
    for pair in t.clone().pairs::<Value, Value>() {
        match pair {
            Ok((k, v)) => pairs.push((k, v)),
            Err(_) => return J::Null,
        }
    }
    // 连续整数键 1..len 且没有其它键 → 数组
    if len > 0 && pairs.len() == len {
        let mut arr = Vec::with_capacity(len);
        for i in 1..=len {
            match t.raw_get::<Value>(i) {
                Ok(v) => arr.push(to_json(&v)),
                Err(_) => return J::Null,
            }
        }
        return J::Array(arr);
    }
    // 空表（或没有任何键）→ 对象
    let mut obj = Map::new();
    for (k, v) in pairs {
        let key = match k {
            Value::String(s) => s.to_string_lossy().to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Boolean(b) => b.to_string(),
            _ => continue,
        };
        obj.insert(key, to_json(&v));
    }
    J::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;
    use serde_json::json;

    #[test]
    fn json_roundtrip() {
        let lua = Lua::new();
        let t = lua
            .load(r#"return {a = 1, b = {1, 2, 3}, c = "x", d = true, e = {x = {y = 2}}}"#)
            .eval::<Value>()
            .unwrap();
        assert_eq!(
            to_json(&t),
            json!({"a": 1, "b": [1, 2, 3], "c": "x", "d": true, "e": {"x": {"y": 2}}})
        );
        // 空表 → {}
        let empty = lua.load("return {}").eval::<Value>().unwrap();
        assert_eq!(to_json(&empty), json!({}));
        // JSON → Lua → JSON
        let v = json!({"n": 1.5, "arr": [1, 2, "s"], "o": {}});
        let lv = to_lua(&lua, &v).unwrap();
        assert_eq!(to_json(&lv), v);
    }

    #[test]
    fn json_lua_array_rules() {
        let lua = Lua::new();
        // 稀疏表 → 对象
        let sparse = lua.load("local t = {}; t[1] = 'a'; t[3] = 'c'; return t").eval::<Value>().unwrap();
        assert_eq!(to_json(&sparse), json!({"1": "a", "3": "c"}));
    }
}
