use saffron_geometry::glam::DVec3;
use saffron_spatial::{
    ResidencyFacet, ResidencyMask, SourceLevel, SpatialSource, SpatialSourceId, WorldPosition,
};
use serde_json::json;

use crate::registry::{CommandRegistry, EngineContext, register_builtin_commands};
use crate::test_support::{StubRenderer, with_stub};

fn registry() -> CommandRegistry {
    let mut reg = CommandRegistry::new();
    register_builtin_commands(&mut reg);
    reg
}

#[test]
fn spatial_cell_reports_exact_negative_face_ownership() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let reply = reg.dispatch(
            ctx,
            &json!({
                "cmd": "spatial-cell",
                "params": { "ticks": { "x": "-1", "y": "-262144", "z": "262144" }, "level": 1 }
            }),
        );
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["position"]["cell"]["x"], json!("-1"));
        assert_eq!(reply["result"]["position"]["cell"]["y"], json!("-1"));
        assert_eq!(reply["result"]["position"]["cell"]["z"], json!("1"));
        assert_eq!(reply["result"]["position"]["local"]["x"], json!(262_143));
        assert_eq!(reply["result"]["selectedCell"]["x"], json!("-1"));
        assert_eq!(reply["result"]["selectedCell"]["y"], json!("-1"));
        assert_eq!(reply["result"]["selectedCell"]["z"], json!("0"));
        assert_eq!(
            reply["result"]["selectedCell"]["canonicalHex"]
                .as_str()
                .unwrap()
                .len(),
            50
        );
    });
}

#[test]
fn spatial_provider_and_user_channel_diagnostics_are_read_only() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let providers = reg.dispatch(ctx, &json!({ "cmd": "spatial-providers" }));
        assert_eq!(providers["result"]["providers"], json!([]));

        let missing_user_id = reg.dispatch(
            ctx,
            &json!({
                "cmd": "spatial-sample",
                "params": {
                    "provider": "1",
                    "channel": "user",
                    "position": { "x": 0, "y": 0, "z": 0 }
                }
            }),
        );
        assert_eq!(missing_user_id["ok"], json!(false));
        assert_eq!(
            missing_user_id["error"]["message"],
            json!("userChannel is required when channel is 'user'")
        );
    });
}

#[test]
fn spatial_residency_reports_sources_and_facet_counts() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        ctx.spatial
            .update_source(SpatialSource {
                id: SpatialSourceId(19),
                revision: 7,
                position: WorldPosition::origin(),
                velocity_mps: DVec3::new(2.0, 0.0, 0.0),
                prediction_seconds: 0.5,
                levels: vec![SourceLevel {
                    level: 0,
                    load_radius_cells: 0,
                    cleanup_radius_cells: 1,
                }],
                facets: ResidencyMask::one(ResidencyFacet::Render).with(ResidencyFacet::Editing),
                priority: 42,
            })
            .unwrap();
        let reply = reg.dispatch(ctx, &json!({ "cmd": "spatial-residency" }));
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["sources"][0]["id"], json!("19"));
        assert_eq!(
            reply["result"]["sources"][0]["facets"],
            json!(["render", "editing"])
        );
        assert_eq!(
            reply["result"]["cells"][0]["referenceCounts"]["render"],
            json!(1)
        );
        assert_eq!(
            reply["result"]["cells"][0]["referenceCounts"]["editing"],
            json!(1)
        );
        assert_eq!(reply["result"]["cells"][0]["priority"], json!(42));
    });
}

/// `create-entity` then `destroy-entity` round-trips, and the returned `EntityRef.id` is
/// a decimal string (the frozen wire encoding).
#[test]
fn create_then_destroy_entity_round_trip() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let created = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-entity", "params": { "name": "Crate" } }),
        );
        assert_eq!(created["ok"], json!(true));
        let id = created["result"]["id"].as_str().expect("id is a string");
        assert_eq!(created["result"]["name"], json!("Crate"));
        assert!(id.parse::<u64>().is_ok(), "id is a decimal string");

        let destroyed = reg.dispatch(
            ctx,
            &json!({ "cmd": "destroy-entity", "params": { "entity": id } }),
        );
        assert_eq!(destroyed["ok"], json!(true));
        assert_eq!(destroyed["result"]["destroyed"], json!(id));
    });
}

/// `resolve_entity` finds by UUID (a numeric string), by name, and errors with the dumped
/// selector when absent — surfaced through `select`.
#[test]
fn resolve_entity_by_uuid_name_and_missing() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let created = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-entity", "params": { "name": "Target" } }),
        );
        let id = created["result"]["id"].as_str().unwrap().to_owned();

        let by_uuid = reg.dispatch(ctx, &json!({ "cmd": "select", "params": { "entity": id } }));
        assert_eq!(by_uuid["ok"], json!(true));
        assert_eq!(by_uuid["result"]["name"], json!("Target"));

        let by_name = reg.dispatch(
            ctx,
            &json!({ "cmd": "select", "params": { "entity": "Target" } }),
        );
        assert_eq!(by_name["ok"], json!(true));
        assert_eq!(by_name["result"]["id"], json!(id));

        // Absent: the error dumps the selector byte-for-byte.
        let missing = reg.dispatch(
            ctx,
            &json!({ "cmd": "select", "params": { "entity": "ghost" } }),
        );
        assert_eq!(missing["ok"], json!(false));
        assert_eq!(
            missing["error"]["message"],
            json!("entity not found: \"ghost\"")
        );
    });
}

/// `add-component` / `set-component-field` dispatch through the registry by name; an
/// unknown component name is a typed error.
#[test]
fn component_commands_dispatch_through_registry() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let created = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-entity", "params": { "name": "E" } }),
        );
        let id = created["result"]["id"].as_str().unwrap().to_owned();

        let added = reg.dispatch(
            ctx,
            &json!({ "cmd": "add-component", "params": { "entity": id, "component": "Camera" } }),
        );
        assert_eq!(added["ok"], json!(true));
        assert_eq!(added["result"]["added"], json!("Camera"));

        let again = reg.dispatch(
            ctx,
            &json!({ "cmd": "add-component", "params": { "entity": id, "component": "Camera" } }),
        );
        assert_eq!(again["ok"], json!(false));
        assert_eq!(
            again["error"]["message"],
            json!("entity already has 'Camera'")
        );

        // An unknown component name is a typed error.
        let unknown = reg.dispatch(
            ctx,
            &json!({ "cmd": "add-component", "params": { "entity": id, "component": "Nope" } }),
        );
        assert_eq!(unknown["ok"], json!(false));
        assert_eq!(
            unknown["error"]["message"],
            json!("unknown component 'Nope'")
        );

        // set-component-field merges one field on the Name component (the string value
        // passes through, not parsed as a number).
        let set = reg.dispatch(
            ctx,
            &json!({
                "cmd": "set-component-field",
                "params": { "entity": id, "component": "Name", "field": "name", "value": "Renamed" }
            }),
        );
        assert_eq!(set["ok"], json!(true));
        assert_eq!(set["result"]["set"], json!("Name"));
        assert_eq!(set["result"]["field"], json!("name"));

        let inspect = reg.dispatch(
            ctx,
            &json!({ "cmd": "inspect", "params": { "entity": id } }),
        );
        assert_eq!(
            inspect["result"]["components"]["Name"]["name"],
            json!("Renamed")
        );
    });
}

#[test]
fn vegetation_field_create_inspect_remove_and_singleton_contract() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let first = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-entity", "params": { "name": "Vegetation" } }),
        );
        let second = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-entity", "params": { "name": "Other" } }),
        );
        let first_id = first["result"]["id"].as_str().unwrap().to_owned();
        let second_id = second["result"]["id"].as_str().unwrap().to_owned();

        let added = reg.dispatch(
            ctx,
            &json!({
                "cmd": "add-component",
                "params": { "entity": first_id, "component": "VegetationField" }
            }),
        );
        assert_eq!(added["ok"], json!(true), "add: {added:?}");
        let inspect = reg.dispatch(
            ctx,
            &json!({ "cmd": "inspect", "params": { "entity": first_id } }),
        );
        assert_eq!(
            inspect["result"]["components"]["VegetationField"],
            json!({ "map": "0", "enabled": true })
        );

        let duplicate = reg.dispatch(
            ctx,
            &json!({
                "cmd": "add-component",
                "params": { "entity": second_id, "component": "VegetationField" }
            }),
        );
        assert_eq!(duplicate["ok"], json!(false));
        assert_eq!(
            duplicate["error"]["message"],
            json!("scene already has a VegetationField component")
        );

        let removed = reg.dispatch(
            ctx,
            &json!({
                "cmd": "remove-component",
                "params": { "entity": first_id, "component": "VegetationField" }
            }),
        );
        assert_eq!(removed["ok"], json!(true), "remove: {removed:?}");
        let replacement = reg.dispatch(
            ctx,
            &json!({
                "cmd": "add-component",
                "params": { "entity": second_id, "component": "VegetationField" }
            }),
        );
        assert_eq!(
            replacement["ok"],
            json!(true),
            "replacement: {replacement:?}"
        );
    });
}

/// `set-component-field` with an array `index` merges an object value into just that
/// slot of a `MaterialSet` (leaving its siblings untouched) and rejects an out-of-range
/// index — the per-slot override edit the editor's material inspector drives.
#[test]
fn set_component_field_slot_index_merges_and_rejects_out_of_range() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let created = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-entity", "params": { "name": "Mesh" } }),
        );
        let id = created["result"]["id"].as_str().unwrap().to_owned();

        reg.dispatch(
            ctx,
            &json!({ "cmd": "add-component", "params": { "entity": id, "component": "MaterialSet" } }),
        );
        // Seed two slots, each referencing a distinct material with empty overrides.
        let seeded = reg.dispatch(
            ctx,
            &json!({
                "cmd": "set-component-field",
                "params": {
                    "entity": id, "component": "MaterialSet", "field": "slots",
                    "value": [
                        { "material": "11", "overrides": {} },
                        { "material": "22", "overrides": {} }
                    ]
                }
            }),
        );
        assert_eq!(seeded["ok"], json!(true));

        // Merge an override into slot 1 only.
        let edited = reg.dispatch(
            ctx,
            &json!({
                "cmd": "set-component-field",
                "params": {
                    "entity": id, "component": "MaterialSet", "field": "slots", "index": 1,
                    "value": { "overrides": { "roughness": 0.25 } }
                }
            }),
        );
        assert_eq!(edited["ok"], json!(true));

        let inspect = reg.dispatch(
            ctx,
            &json!({ "cmd": "inspect", "params": { "entity": id } }),
        );
        let slots = &inspect["result"]["components"]["MaterialSet"]["slots"];
        assert_eq!(slots[1]["overrides"]["roughness"], json!(0.25));
        assert!(slots[0]["overrides"]["roughness"].is_null());

        // An out-of-range index is a typed error, not a silent no-op.
        let bad = reg.dispatch(
            ctx,
            &json!({
                "cmd": "set-component-field",
                "params": {
                    "entity": id, "component": "MaterialSet", "field": "slots", "index": 9,
                    "value": { "overrides": { "metallic": 0.5 } }
                }
            }),
        );
        assert_eq!(bad["ok"], json!(false));
    });
}

/// `set-transform` merges the provided fields onto the entity's `Transform` and the write
/// is observable through `inspect`.
#[test]
fn set_transform_is_observable_through_inspect() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let created = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-entity", "params": { "name": "Movable" } }),
        );
        let id = created["result"]["id"].as_str().unwrap().to_owned();

        let set = reg.dispatch(
            ctx,
            &json!({
                "cmd": "set-transform",
                "params": { "entity": id, "translation": { "x": 1.5, "y": -2.0, "z": 3.25 } }
            }),
        );
        assert_eq!(set["ok"], json!(true));

        let inspect = reg.dispatch(
            ctx,
            &json!({ "cmd": "inspect", "params": { "entity": id } }),
        );
        let translation = &inspect["result"]["components"]["Transform"]["translation"];
        assert_eq!(translation["x"], json!(1.5));
        assert_eq!(translation["y"], json!(-2.0));
        assert_eq!(translation["z"], json!(3.25));
    });
}

/// `add-component` appends at the bottom of the order, `set-component-order` reorders the
/// present set, and a list that drops or duplicates a present component is rejected.
#[test]
fn component_order_appends_reorders_and_validates() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let created = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-entity", "params": { "name": "Ordered" } }),
        );
        let id = created["result"]["id"].as_str().unwrap().to_owned();

        let added = reg.dispatch(
            ctx,
            &json!({ "cmd": "add-component", "params": { "entity": id, "component": "Camera" } }),
        );
        assert_eq!(added["ok"], json!(true));
        let inspect = reg.dispatch(
            ctx,
            &json!({ "cmd": "inspect", "params": { "entity": id } }),
        );
        assert_eq!(
            inspect["result"]["componentOrder"],
            json!(["Name", "Transform", "Camera"])
        );

        // An explicit reorder of the present set applies verbatim.
        let reordered = reg.dispatch(
            ctx,
            &json!({
                "cmd": "set-component-order",
                "params": { "entity": id, "components": ["Camera", "Name", "Transform"] }
            }),
        );
        assert_eq!(reordered["ok"], json!(true));
        assert_eq!(
            reordered["result"]["components"],
            json!(["Camera", "Name", "Transform"])
        );

        // A list that duplicates a component (and drops another) is rejected, not applied.
        let bad = reg.dispatch(
            ctx,
            &json!({
                "cmd": "set-component-order",
                "params": { "entity": id, "components": ["Camera", "Name", "Camera"] }
            }),
        );
        assert_eq!(bad["ok"], json!(false));
    });
}

/// `inspect` returns `{id, name, components, componentOrder}` with the order in registry
/// order and the component blob as an opaque object.
#[test]
fn inspect_dumps_components_and_order() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let created = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-entity", "params": { "name": "Inspectable" } }),
        );
        let id = created["result"]["id"].as_str().unwrap().to_owned();

        let inspect = reg.dispatch(
            ctx,
            &json!({ "cmd": "inspect", "params": { "entity": id } }),
        );
        assert_eq!(inspect["ok"], json!(true));
        assert_eq!(inspect["result"]["name"], json!("Inspectable"));
        assert!(inspect["result"]["components"].is_object());
        let order = inspect["result"]["componentOrder"]
            .as_array()
            .expect("componentOrder is an array");
        // A fresh entity carries Name + Transform.
        assert_eq!(order[0], json!("Name"));
        assert_eq!(order[1], json!("Transform"));
    });
}

/// `play` / `pause` / `step` / `stop` produce the expected `PlayStateResult` transitions.
#[test]
fn play_machine_transitions() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let play = reg.dispatch(ctx, &json!({ "cmd": "play" }));
        assert_eq!(play["ok"], json!(true));
        assert_eq!(play["result"]["state"], json!("playing"));

        let pause = reg.dispatch(ctx, &json!({ "cmd": "pause" }));
        assert_eq!(pause["ok"], json!(true));
        assert_eq!(pause["result"]["state"], json!("paused"));

        let step = reg.dispatch(ctx, &json!({ "cmd": "step", "params": { "frames": 1 } }));
        assert_eq!(step["ok"], json!(true));
        assert_eq!(step["result"]["state"], json!("paused"));

        let stop = reg.dispatch(ctx, &json!({ "cmd": "stop" }));
        assert_eq!(stop["ok"], json!(true));
        assert_eq!(stop["result"]["state"], json!("edit"));

        // pause from Edit is rejected (wrong state).
        let bad = reg.dispatch(ctx, &json!({ "cmd": "pause" }));
        assert_eq!(bad["ok"], json!(false));
    });
}

/// `set-gizmo` applies op/space/preserve-children and `get-gizmo` reads them back.
#[test]
fn gizmo_state_round_trips() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let set = reg.dispatch(
            ctx,
            &json!({
                "cmd": "set-gizmo",
                "params": { "op": "rotate", "space": "local", "preserveChildren": true }
            }),
        );
        assert_eq!(set["ok"], json!(true));
        assert_eq!(set["result"]["op"], json!("rotate"));
        assert_eq!(set["result"]["space"], json!("local"));
        assert_eq!(set["result"]["preserveChildren"], json!(true));

        let get = reg.dispatch(ctx, &json!({ "cmd": "get-gizmo" }));
        assert_eq!(get["result"]["op"], json!("rotate"));
        assert_eq!(get["result"]["space"], json!("local"));
    });
}

/// Every scene mutation strictly bumps `sceneVersion` (read through get-selection) — the
/// stamp the editor re-polls on. Covers add / copy / rename / destroy plus set-transform,
/// set-component, and set-environment, with the entity id surfacing as a decimal string.
#[test]
fn scene_mutations_bump_scene_version() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let scene_version = |ctx: &mut EngineContext| -> i64 {
            reg.dispatch(ctx, &json!({ "cmd": "get-selection" }))["result"]["sceneVersion"]
                .as_i64()
                .expect("sceneVersion is an integer")
        };

        // A built-in primitive needs no project (reserved-id geometry).
        let cube = reg.dispatch(
            ctx,
            &json!({ "cmd": "add-entity", "params": { "preset": "cube" } }),
        );
        assert_eq!(cube["ok"], json!(true));
        let id = cube["result"]["id"]
            .as_str()
            .expect("id is a string")
            .to_owned();
        assert!(
            id.parse::<u64>().is_ok(),
            "id round-trips as a decimal string"
        );
        let mut version = scene_version(ctx);

        for command in [
            json!({ "cmd": "copy-entity", "params": { "entity": id } }),
            json!({ "cmd": "rename-entity", "params": { "entity": id, "name": "Renamed" } }),
            json!({ "cmd": "set-transform",
                    "params": { "entity": id, "translation": { "x": 1, "y": 2, "z": 3 } } }),
            json!({ "cmd": "set-component",
                    "params": { "entity": id, "component": "Name", "json": { "name": "Again" } } }),
            json!({ "cmd": "set-environment", "params": { "skyIntensity": 2.0 } }),
            json!({ "cmd": "destroy-entity", "params": { "entity": id } }),
        ] {
            let reply = reg.dispatch(ctx, &command);
            assert_eq!(reply["ok"], json!(true), "{command}");
            let next = scene_version(ctx);
            assert!(next > version, "{command} bumps sceneVersion");
            version = next;
        }
    });
}

/// `set-atmosphere` reflects every field it is given, a partial call merges over the current
/// atmosphere block (it does not reset), and the free-form `{json}` path merges arbitrary keys.
#[test]
fn set_atmosphere_echoes_fields_and_merges_over_state() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let env = reg.dispatch(
            ctx,
            &json!({
                "cmd": "set-atmosphere",
                "params": {
                    "enabled": true,
                    "planetRadius": 6360000.0,
                    "rayleighScattering": { "x": 5.8, "y": 13.5, "z": 33.1 },
                    "sunDiskIntensity": 20.0
                }
            }),
        );
        assert_eq!(env["ok"], json!(true));
        let atmos = &env["result"]["atmosphere"];
        assert_eq!(atmos["enabled"], json!(true));
        assert!((atmos["planetRadius"].as_f64().unwrap() - 6_360_000.0).abs() < 1.0);
        assert!((atmos["rayleighScattering"]["y"].as_f64().unwrap() - 13.5).abs() < 1e-3);
        assert!((atmos["sunDiskIntensity"].as_f64().unwrap() - 20.0).abs() < 1e-3);

        // A partial call merges over the read-back — the earlier fields survive.
        let merged = reg.dispatch(
            ctx,
            &json!({ "cmd": "set-atmosphere", "params": { "mieAnisotropy": 0.8 } }),
        );
        let atmos = &merged["result"]["atmosphere"];
        assert!((atmos["mieAnisotropy"].as_f64().unwrap() - 0.8).abs() < 1e-3);
        assert_eq!(
            atmos["enabled"],
            json!(true),
            "prior fields survive the merge"
        );
        assert!((atmos["sunDiskIntensity"].as_f64().unwrap() - 20.0).abs() < 1e-3);

        // The free-form {json} path merges arbitrary keys over the same block.
        let free = reg.dispatch(
            ctx,
            &json!({ "cmd": "set-atmosphere", "params": { "json": { "mieScattering": 4.2 } } }),
        );
        let atmos = &free["result"]["atmosphere"];
        assert!((atmos["mieScattering"].as_f64().unwrap() - 4.2).abs() < 1e-3);
        assert_eq!(atmos["enabled"], json!(true));
    });
}

/// A smooth `set-transform` registers a per-frame animation target instead of writing; a
/// following non-smooth write cancels that target and applies the exact value.
#[test]
fn set_transform_smooth_defers_then_exact_write_cancels() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let created = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-entity", "params": { "name": "Mover" } }),
        );
        let id = created["result"]["id"].as_str().unwrap().to_owned();

        reg.dispatch(
            ctx,
            &json!({
                "cmd": "set-transform",
                "params": { "entity": id, "translation": { "x": 10, "y": 0, "z": 0 }, "smooth": true }
            }),
        );
        assert_eq!(
            ctx.scene_edit.transform_smoothing.len(),
            1,
            "a smooth edit defers to a target instead of writing"
        );

        reg.dispatch(
            ctx,
            &json!({
                "cmd": "set-transform",
                "params": { "entity": id, "translation": { "x": -1, "y": 2.5, "z": 0.75 } }
            }),
        );
        assert!(
            ctx.scene_edit.transform_smoothing.is_empty(),
            "an exact write cancels the pending target"
        );

        let info = reg.dispatch(
            ctx,
            &json!({ "cmd": "inspect", "params": { "entity": id } }),
        );
        let t = &info["result"]["components"]["Transform"]["translation"];
        assert_eq!(t["x"], json!(-1.0));
        assert_eq!(t["y"], json!(2.5));
        assert_eq!(t["z"], json!(0.75));
    });
}

/// `inspect` keys its `components` map by registry name and the wire schema for it is
/// `saffron_protocol::Components`, which the protocol crate cannot check against the registry —
/// it does not depend on `saffron-scene`. A component registered without a DTO field serializes
/// into a reply that fails the generated schema, so the two lists are compared here.
#[test]
fn the_component_dto_aggregate_matches_the_scene_registry() {
    assert_eq!(
        saffron_protocol::COMPONENT_NAMES,
        saffron_scene::BUILTIN_COMPONENT_NAMES,
        "every registered component needs a saffron_protocol::Components field, in registry order"
    );
}

/// The pick command's whole vocabulary comes from one GPU selection-ID answer: a drawn entity
/// selects, a macro plant returns its stable identity and selects nothing, a micro blade returns
/// a bare point with no identity at all, and a pixel the frame drew nothing at is a miss.
#[test]
fn pick_translates_every_selection_id_answer() {
    let reg = registry();
    let plant = saffron_vegetation::PlantId::runtime([0x11; 16]).unwrap();
    let cell = saffron_spatial::WorldCellKey::base(0, 0, 0);

    let mut renderer = StubRenderer {
        selection_pick: Some(crate::SelectionPick::Plant {
            cell,
            plant,
            position: [1.0, 2.0, 3.0],
            normal: [0.0, 1.0, 0.0],
        }),
        ..StubRenderer::default()
    };
    with_stub(&mut renderer, |ctx| {
        let reply = reg.dispatch(
            ctx,
            &json!({ "cmd": "pick", "params": { "u": 0.5, "v": 0.5 } }),
        );
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["kind"], json!("vegetation"));
        assert_eq!(reply["result"]["plant"], json!(plant.to_string()));
        assert_eq!(reply["result"]["id"], json!(null));
        assert_eq!(reply["result"]["position"], json!([1.0, 2.0, 3.0]));
    });

    let mut renderer = StubRenderer {
        selection_pick: Some(crate::SelectionPick::Micro {
            position: [4.0, 0.0, 5.0],
            normal: [0.0, 1.0, 0.0],
        }),
        ..StubRenderer::default()
    };
    with_stub(&mut renderer, |ctx| {
        let reply = reg.dispatch(
            ctx,
            &json!({ "cmd": "pick", "params": { "u": 0.5, "v": 0.5 } }),
        );
        assert_eq!(reply["result"]["kind"], json!("micro-vegetation"));
        assert_eq!(reply["result"]["plant"], json!(null));
        assert_eq!(reply["result"]["id"], json!(null));
        assert_eq!(reply["result"]["position"], json!([4.0, 0.0, 5.0]));
    });

    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let reply = reg.dispatch(
            ctx,
            &json!({ "cmd": "pick", "params": { "u": 0.5, "v": 0.5 } }),
        );
        assert_eq!(reply["result"]["hit"], json!(false));
        assert_eq!(reply["result"]["kind"], json!(null));
    });
}
