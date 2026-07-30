//! The JSON↔Lua bridge for the component read/write surface: total conversions
//! between `serde_json::Value` and `mlua::Value`.
//!
//! Both halves are total over the component DTO shapes: nothing aborts, and an unrepresentable
//! input degrades to `nil` or `null`. Arrays cross as 1-based tables, a uuid stays its decimal
//! string, and an unsigned integer past `i64::MAX` falls back to `f64` so a u64 uuid that slipped
//! through as a number does not wrap negative. Writing back, a `sa.Vec3` userdata becomes the
//! `{x, y, z}` object the per-component serde reads.

use mlua::{Lua, Value as LuaValue};
use serde_json::{Map, Value as JsonValue};

use crate::value::SaVec3;

/// Converts a JSON value to a Lua value (the `get_component` read half).
///
/// Total over the component DTO shapes — see the module docs. `lua` is needed to
/// allocate tables; a conversion never fails (it returns `nil` for anything it cannot
/// represent, but every JSON value is representable), so the `mlua::Result` only
/// carries an allocation failure from `mlua` itself.
pub fn json_to_lua(lua: &Lua, value: &JsonValue) -> mlua::Result<LuaValue> {
    match value {
        JsonValue::Null => Ok(LuaValue::Nil),
        JsonValue::Bool(b) => Ok(LuaValue::Boolean(*b)),
        JsonValue::String(s) => Ok(LuaValue::String(lua.create_string(s)?)),
        JsonValue::Number(n) => Ok(number_to_lua(n)),
        JsonValue::Array(items) => {
            let table = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                table.set(i + 1, json_to_lua(lua, item)?)?;
            }
            Ok(LuaValue::Table(table))
        }
        JsonValue::Object(fields) => {
            let table = lua.create_table()?;
            for (key, item) in fields {
                table.set(key.as_str(), json_to_lua(lua, item)?)?;
            }
            Ok(LuaValue::Table(table))
        }
    }
}

/// Converts a JSON number to a Lua value with an integer/float split.
///
/// A float stays a Lua number; an unsigned integer past `i64::MAX` falls back to a
/// Lua number (so a u64 that arrived as a number does not wrap negative when narrowed
/// to an `i64`); every other integer becomes a Lua integer.
fn number_to_lua(n: &serde_json::Number) -> LuaValue {
    if let Some(i) = n.as_i64() {
        return LuaValue::Integer(i);
    }
    if let Some(u) = n.as_u64() {
        return LuaValue::Number(u as f64);
    }
    match n.as_f64() {
        Some(f) => LuaValue::Number(f),
        None => LuaValue::Nil,
    }
}

/// Converts a Lua value to a JSON value (the `set_component` write half).
///
/// Total over the script-facing shapes — see the module docs. An `sa.Vec3` userdata
/// becomes a `{x,y,z}` object; any other userdata (no numeric `x`/`y`/`z`) drops to
/// `null` rather than being guessed at. A table with a positive array length is an
/// array; otherwise its string keys become an object (non-string keys are dropped).
pub fn lua_to_json(value: &LuaValue) -> JsonValue {
    match value {
        LuaValue::Nil => JsonValue::Null,
        LuaValue::Boolean(b) => JsonValue::Bool(*b),
        LuaValue::Integer(i) => JsonValue::Number((*i).into()),
        LuaValue::Number(n) => serde_json::Number::from_f64(*n)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        LuaValue::String(s) => match s.to_str() {
            Ok(text) => JsonValue::String(text.to_owned()),
            Err(_) => JsonValue::Null,
        },
        LuaValue::UserData(ud) => userdata_to_json(ud),
        LuaValue::Table(table) => table_to_json(table),
        _ => JsonValue::Null,
    }
}

/// An `sa.Vec3` userdata → `{x,y,z}` object (the shape the per-component serde reads);
/// any other userdata → `null`.
fn userdata_to_json(ud: &mlua::AnyUserData) -> JsonValue {
    match ud.borrow::<SaVec3>() {
        Ok(v) => {
            let mut object = Map::new();
            object.insert("x".to_owned(), float_to_json(f64::from(v.0.x)));
            object.insert("y".to_owned(), float_to_json(f64::from(v.0.y)));
            object.insert("z".to_owned(), float_to_json(f64::from(v.0.z)));
            JsonValue::Object(object)
        }
        Err(_) => JsonValue::Null,
    }
}

/// A Lua table → an array (positive sequence length) or an object (string keys).
fn table_to_json(table: &mlua::Table) -> JsonValue {
    let len = table.raw_len();
    if len > 0 {
        let mut array = Vec::with_capacity(len);
        for i in 1..=len {
            let item: LuaValue = table.get(i).unwrap_or(LuaValue::Nil);
            array.push(lua_to_json(&item));
        }
        return JsonValue::Array(array);
    }
    let mut object = Map::new();
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let Ok((key, item)) = pair else { continue };
        if let LuaValue::String(key) = key
            && let Ok(key) = key.to_str()
        {
            object.insert(key.to_owned(), lua_to_json(&item));
        }
    }
    JsonValue::Object(object)
}

/// A finite `f64` → a JSON number, NaN/inf → `null` (JSON has no representation for a
/// non-finite double).
fn float_to_json(f: f64) -> JsonValue {
    serde_json::Number::from_f64(f)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::vec3;
    use serde_json::json;

    fn lua() -> Lua {
        Lua::new()
    }

    #[test]
    fn json_object_becomes_a_string_keyed_table() {
        let lua = lua();
        let value = json!({ "a": 1, "b": "hi", "c": true });
        let LuaValue::Table(table) = json_to_lua(&lua, &value).expect("convert") else {
            panic!("object should convert to a table");
        };
        assert_eq!(table.get::<i64>("a").expect("a"), 1);
        assert_eq!(table.get::<String>("b").expect("b"), "hi");
        assert!(table.get::<bool>("c").expect("c"));
    }

    #[test]
    fn json_array_becomes_a_one_based_table() {
        let lua = lua();
        let value = json!([10, 20, 30]);
        let LuaValue::Table(table) = json_to_lua(&lua, &value).expect("convert") else {
            panic!("array should convert to a table");
        };
        assert_eq!(table.raw_len(), 3);
        assert_eq!(table.get::<i64>(1).expect("[1]"), 10);
        assert_eq!(table.get::<i64>(3).expect("[3]"), 30);
    }

    #[test]
    fn json_null_becomes_nil() {
        let lua = lua();
        assert!(matches!(
            json_to_lua(&lua, &JsonValue::Null).expect("convert"),
            LuaValue::Nil
        ));
    }

    #[test]
    fn large_unsigned_falls_back_to_a_number() {
        let lua = lua();
        // A u64 past i64::MAX (the decimal-string uuid range) must not wrap negative
        // when it slips through as a JSON number — it falls back to a Lua number.
        let big = u64::MAX;
        let value = json!(big);
        let lua_value = json_to_lua(&lua, &value).expect("convert");
        match lua_value {
            LuaValue::Number(n) => assert!(n > 0.0, "should be a positive number"),
            other => panic!("expected a number fallback, got {other:?}"),
        }
    }

    #[test]
    fn uuid_string_stays_a_string() {
        let lua = lua();
        // The serde emits a uuid as a decimal string; it round-trips as a string.
        let value = json!({ "id": "12345678901234567890" });
        let LuaValue::Table(table) = json_to_lua(&lua, &value).expect("convert") else {
            panic!("object should convert to a table");
        };
        assert_eq!(
            table.get::<String>("id").expect("id"),
            "12345678901234567890"
        );
    }

    #[test]
    fn lua_string_keyed_table_becomes_an_object() {
        let lua = lua();
        let table = lua.create_table().expect("table");
        table.set("a", 1).expect("set a");
        table.set("b", "hi").expect("set b");
        let json = lua_to_json(&LuaValue::Table(table));
        assert_eq!(json, json!({ "a": 1, "b": "hi" }));
    }

    #[test]
    fn lua_sequence_becomes_an_array() {
        let lua = lua();
        let table = lua.create_table().expect("table");
        for i in 1..=3 {
            table.set(i, i * 10).expect("set");
        }
        let json = lua_to_json(&LuaValue::Table(table));
        assert_eq!(json, json!([10, 20, 30]));
    }

    #[test]
    fn sa_vec3_userdata_becomes_an_xyz_object() {
        let lua = lua();
        let ud = lua.create_userdata(vec3(1.0, 2.0, 3.0)).expect("userdata");
        let json = lua_to_json(&LuaValue::UserData(ud));
        assert_eq!(json, json!({ "x": 1.0, "y": 2.0, "z": 3.0 }));
    }

    #[test]
    fn lua_nil_and_unknown_become_null() {
        assert_eq!(lua_to_json(&LuaValue::Nil), JsonValue::Null);
    }

    #[test]
    fn round_trip_object_through_both_halves() {
        let lua = lua();
        let original = json!({ "name": "x", "count": 3, "on": true, "nested": { "k": "v" } });
        let lua_value = json_to_lua(&lua, &original).expect("to lua");
        let back = lua_to_json(&lua_value);
        assert_eq!(
            back, original,
            "object should round-trip through both halves"
        );
    }
}
