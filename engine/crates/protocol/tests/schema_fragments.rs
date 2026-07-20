//! The generated schema-fragment oracle.

use saffron_protocol::schema_fragments;
use serde_json::Value;

const OPENRPC: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../schemas/control/openrpc.generated.json"
));

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), sorted(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

#[test]
fn every_rust_fragment_matches_committed_openrpc() {
    let document: Value = serde_json::from_str(OPENRPC).expect("committed openrpc parses");
    let schemas = &document["components"]["schemas"];
    for (name, fragment) in schema_fragments() {
        let committed = &schemas[name];
        assert!(
            !committed.is_null(),
            "{name} missing from committed openrpc"
        );
        assert_eq!(
            sorted(&fragment),
            sorted(committed),
            "fragment for {name} drifted from committed openrpc"
        );
    }
}
