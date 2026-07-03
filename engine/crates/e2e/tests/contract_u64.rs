//! The decimal-string-u64 contract probe against the live engine: drive id-returning commands and
//! assert their raw reply bytes carry every id as a quoted decimal string.
//!
//! The negative probe (a bare-number id is *caught*) lives as a unit test in `src/lib.rs`; this
//! file proves the positive direction live.

use saffron_e2e::{TestEngine, assert_raw_u64};
use serde_json::json;

/// `add-entity cube` returns an `EntityRef` carrying the new entity's `id`; its raw reply bytes must
/// encode that id as a quoted decimal string. `list-entities` (an array of ids, with `parentId`
/// after a reparent) exercises the multi-id path the same way.
#[test]
fn live_id_returning_commands_emit_decimal_string_ids() {
    let mut engine = TestEngine::boot(&[("SAFFRON_SCRATCH_PROJECT", "1")]).expect("boot engine");

    // `add-entity cube` needs a loaded project (the auto empty project above) and returns the id.
    let added = engine
        .call_raw_text("add-entity", json!({ "args": ["cube"] }))
        .expect("add cube");
    let errors = assert_raw_u64(&added, "add-entity");
    assert!(
        errors.is_empty(),
        "add-entity id must be a quoted decimal string:\n  raw: {added}\n  errors: {errors:?}"
    );
    // Guard against a vacuous pass: the reply must actually carry a quoted id token.
    assert!(
        added.contains("\"id\":\""),
        "add-entity reply should carry a quoted id: {added}"
    );

    // `list-entities` returns every entity id (and a `parentId` for parented ones); all decimal
    // strings.
    let listed = engine
        .call_raw_text("list-entities", json!({}))
        .expect("list entities");
    let errors = assert_raw_u64(&listed, "list-entities");
    assert!(
        errors.is_empty(),
        "list-entities ids must be quoted decimal strings:\n  raw: {listed}\n  errors: {errors:?}"
    );

    engine.shutdown();
}
