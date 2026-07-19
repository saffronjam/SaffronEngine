//! Protocol type-inventory tripwires.

use std::collections::HashSet;

use saffron_protocol::{
    AssetSelector, COMPONENT_NAMES, DTO_TYPE_NAMES, EntitySelector, schema_fragments, ts_decls,
};

#[test]
fn every_command_and_component_type_is_registered_once() {
    let declarations = ts_decls();
    let names: HashSet<&str> = declarations.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names.len(),
        declarations.len(),
        "duplicate TypeScript declaration"
    );

    for name in DTO_TYPE_NAMES {
        assert!(
            names.contains(name),
            "missing command DTO declaration {name}"
        );
    }
    for name in COMPONENT_NAMES {
        assert!(
            names.contains(name),
            "missing component DTO declaration {name}"
        );
    }

    let fragments = schema_fragments();
    let fragment_names: HashSet<&str> = fragments.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        fragment_names.len(),
        fragments.len(),
        "duplicate schema fragment"
    );
    for name in DTO_TYPE_NAMES {
        assert!(
            fragment_names.contains(name),
            "missing command DTO schema fragment {name}"
        );
    }
    for name in COMPONENT_NAMES {
        assert!(
            fragment_names.contains(name),
            "missing component DTO schema fragment {name}"
        );
    }
}

#[test]
fn selectors_accept_only_ids_or_names() {
    assert!(serde_json::from_str::<EntitySelector>("42").is_ok());
    assert!(serde_json::from_str::<EntitySelector>(r#""Player""#).is_ok());
    assert!(serde_json::from_str::<EntitySelector>("{}").is_err());

    assert!(serde_json::from_str::<AssetSelector>("42").is_ok());
    assert!(serde_json::from_str::<AssetSelector>(r#""models/player.smodel""#).is_ok());
    assert!(serde_json::from_str::<AssetSelector>("null").is_err());
}
