use saffron_protocol::{
    InteractionPolicyDto, PlantLifecycleDto, Uuid as WireUuid, VegetationRuntimeQueryFilterDto,
};
use saffron_vegetation::{InteractionPolicy, PlantLifecycle};

use super::*;
use crate::registry::CommandRegistry;
use serde_json::json;

use crate::test_support::{StubRenderer, with_stub};

#[test]
fn snapshot_hex_is_strict_and_roundtrips() {
    let bytes = b"canonical vegetation state";
    let encoded = encode_hex(bytes);
    assert_eq!(decode_hex(&encoded).unwrap(), bytes);
    assert!(decode_hex("AA").is_err());
    assert!(decode_hex("0").is_err());
}

#[test]
fn filter_mapping_is_exhaustive_and_validates_tags() {
    let filter = query_filter(VegetationRuntimeQueryFilterDto {
        families: vec![WireUuid::from(7_u64)],
        required_tags: vec!["42".to_owned()],
        lifecycles: vec![PlantLifecycleDto::Mature],
        interaction_policies: vec![InteractionPolicyDto::Structural],
    })
    .unwrap();
    assert_eq!(filter.families[0].value(), 7);
    assert_eq!(filter.required_tags.iter().next().unwrap().value(), 42);
    assert!(filter.lifecycles.contains(&PlantLifecycle::Mature));
    assert_eq!(filter.interaction_policies, [InteractionPolicy::Structural]);
    assert!(
        query_filter(VegetationRuntimeQueryFilterDto {
            required_tags: vec!["0".to_owned()],
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn status_reports_the_runtime_sessions_typed_unavailability() {
    let mut registry = CommandRegistry::new();
    register_runtime_vegetation_commands(&mut registry);
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |context| {
        let reply = registry.dispatch(
            context,
            &json!({ "cmd": "vegetation-runtime-status", "params": {} }),
        );
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["state"], json!("unavailable"));
        assert_eq!(reply["result"]["reason"], json!("no-project"));
    });
}
