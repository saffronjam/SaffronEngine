//! Canonical typed-document round-trip and strict parse rejection.

use super::*;

#[test]
fn document_round_trip_is_canonical() {
    let document = simple_document();
    let decoded = BiomeGraphDocument::from_json(&document.to_json()).unwrap();
    assert_eq!(decoded, document);
    assert_eq!(decoded.identity(), document.identity());
}

#[test]
fn noncurrent_node_versions_are_rejected() {
    let document = simple_document();
    let mut raw = document.to_json();
    let scatter = raw
        .get_mut("nodes")
        .and_then(Value::as_array_mut)
        .unwrap()
        .iter_mut()
        .find(|node| {
            node.get("guid").and_then(Value::as_str) == Some("00000000000000000000000000000002")
        })
        .unwrap()
        .as_object_mut()
        .unwrap();
    scatter.insert("version".to_owned(), Value::from(0));
    assert!(matches!(
        BiomeGraphDocument::from_json(&raw),
        Err(Error::FormatVersion {
            format: ".sbiome node",
            found: 0,
            expected: BIOME_NODE_VERSION,
        })
    ));
}
#[test]
fn graph_numeric_parameters_reject_fractional_values_at_the_typed_path() {
    let mut value = simple_document().to_json();
    let nodes = value
        .get_mut("nodes")
        .and_then(Value::as_array_mut)
        .unwrap();
    let scatter_guid = guid_text(2);
    let scatter = nodes
        .iter_mut()
        .find(|node| node.get("guid").and_then(Value::as_str) == Some(scatter_guid.as_str()))
        .unwrap();
    scatter
        .get_mut("parameters")
        .and_then(Value::as_object_mut)
        .unwrap()
        .insert("count".to_owned(), Value::from(1.5));

    assert!(matches!(
        BiomeGraphDocument::from_json(&value),
        Err(Error::GraphDocument { path, reason })
            if path.ends_with(".parameters.count") && reason == "expected unsigned integer"
    ));
}
