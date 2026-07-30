use std::collections::BTreeMap;

use saffron_assets::{
    default_material_asset, save_biome_asset, save_material_asset, save_plant_family_asset,
};
use saffron_protocol::PlantSourceSelectorDto;
use saffron_scene::{AssetEntry, AssetType, MaterialSet, Mesh, VegetationField};
use saffron_sceneedit::ProjectPhase;
use saffron_spatial::{DecisionScalar, UnitInterval};
use saffron_vegetation::{
    BIOME_ASSET_VERSION, BiomeAsset, BiomeGraphPolicy, BiomePaletteEntry, BiomeRole,
    BotanicalGraphDocument, InteractionPolicy, MechanicalResponse, PLANT_ASSET_VERSION,
    PhenotypeRole, PlantDimensions, PlantFamilyAsset, PlantFamilySource, PlantPart,
    PlantPartSemantic, PlantPhenotype, PlantVariation,
};
use serde_json::json;

use crate::registry::{CommandRegistry, EngineContext, register_builtin_commands};
use crate::selector::entity_uuid;
use crate::test_support::{StubRenderer, with_stub};

use super::plant_source_reference_dto;

fn registry() -> CommandRegistry {
    let mut reg = CommandRegistry::new();
    register_builtin_commands(&mut reg);
    reg
}

/// Roots the test's asset server at a unique scratch dir so the material file-writing
/// commands never collide across parallel tests.
fn scratch_root(ctx: &mut EngineContext<'_>, tag: &str) {
    let dir = std::env::temp_dir().join(format!(
        "saffron-control-asset-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    ctx.assets.set_asset_root(dir.join("assets"));
}

/// Seeds a mesh catalog row, returning its decimal-string id.
fn seed_mesh(ctx: &mut EngineContext<'_>, name: &str) -> u64 {
    let id = saffron_core::Uuid::new();
    ctx.assets.register_imported_asset(AssetEntry {
        id,
        name: name.to_owned(),
        asset_type: AssetType::Mesh,
        path: format!("models/{}.smesh", id.value()),
        ..AssetEntry::default()
    });
    id.value()
}

fn fixed(value: i32) -> DecisionScalar {
    DecisionScalar::from_integer(value).expect("fixture scalar")
}

fn seed_native_plant(ctx: &mut EngineContext<'_>) -> u64 {
    let material = save_material_asset(ctx.assets, &default_material_asset(), "Leaf", "plants")
        .expect("save plant material");
    let plant = PlantFamilyAsset {
        role: saffron_vegetation::PlantFamilyRole::Family,
        modules: Vec::new(),
        module_recursion_limit: saffron_vegetation::MAX_PLANT_MODULE_RECURSION,
        version: PLANT_ASSET_VERSION,
        id: saffron_core::Uuid(9_100),
        name: "Oak".to_owned(),
        tags: Vec::new(),
        source: PlantFamilySource::Native {
            graph: BotanicalGraphDocument::sapling(0x5a11),
            grafts: Vec::new(),
        },
        parts: vec![PlantPart {
            id: 12,
            parent: None,
            semantic: PlantPartSemantic::Trunk,
            material_slot: 0,
            sources: Vec::new(),
        }],
        dimensions: PlantDimensions {
            height: fixed(8),
            trunk_radius: fixed(1),
            crown_radius: [fixed(3); 2],
            root_radius: [fixed(4); 2],
            local_bounds_min: [fixed(-4), fixed(0), fixed(-4)],
            local_bounds_max: [fixed(4), fixed(8), fixed(4)],
        },
        material_slots: vec![material],
        spines: Vec::new(),
        mechanics: MechanicalResponse {
            stiffness: fixed(2),
            damping: UnitInterval::from_bits(1),
            drag: fixed(1),
            flutter: DecisionScalar::from_bits(1),
            bend_limit: UnitInterval::from_bits(2),
            damage_threshold: fixed(3),
            break_threshold: fixed(4),
        },
        variations: vec![PlantVariation {
            id: 0,
            name: "Default".to_owned(),
            sources: vec![saffron_vegetation::native_variation_source_id(0)],
            active_parts: Vec::new(),
        }],
        phenotypes: vec![PlantPhenotype {
            id: 0,
            role: PhenotypeRole::Healthy,
            season_window: None,
            variation: 0,
            material_remap: Vec::new(),
            active_parts: Vec::new(),
        }],
        collision_proxies: Vec::new(),
        navigation_proxies: Vec::new(),
        interaction_policy: InteractionPolicy::Structural,
        habitat: None,
        ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
    };
    save_plant_family_asset(ctx.assets, plant, "Oak", "plants")
        .expect("save plant")
        .value()
}

fn seed_biome(ctx: &mut EngineContext<'_>, plant: u64) -> u64 {
    let biome = BiomeAsset {
        version: BIOME_ASSET_VERSION,
        id: saffron_core::Uuid(9_101),
        name: "Forest".to_owned(),
        role: BiomeRole::Root,
        parameters: Vec::new(),
        palette: vec![BiomePaletteEntry {
            plant: saffron_core::Uuid(plant),
            weight: UnitInterval::ONE,
            seed_namespace: 23,
        }],
        density: fixed(1),
        clustering: UnitInterval::from_bits(24),
        suitability: Vec::new(),
        competition: Vec::new(),
        companions: Vec::new(),
        succession: Vec::new(),
        seed_namespaces: vec![("canopy".to_owned(), 23)],
        modules: Vec::new(),
        policy: BiomeGraphPolicy {
            maximum_recursion: 8,
            maximum_influence_radius: fixed(64),
            require_authoritative_fields: true,
        },
        graph: serde_json::Value::Object(Default::default()),
    };
    save_biome_asset(ctx.assets, biome, "Forest", "biomes")
        .expect("save biome")
        .value()
}

/// `list-assets` and `scan-assets` round-trip on an empty (just-loaded) project.
#[test]
fn list_and_scan_on_empty_project() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        scratch_root(ctx, "empty");
        ctx.scene_edit.project_phase = ProjectPhase::Ready;

        let list = reg.dispatch(ctx, &json!({ "cmd": "list-assets" }));
        assert_eq!(list["ok"], json!(true));
        assert_eq!(list["result"]["assets"], json!([]));
        assert_eq!(list["result"]["folders"], json!([]));

        let scan = reg.dispatch(ctx, &json!({ "cmd": "scan-assets" }));
        assert_eq!(scan["ok"], json!(true));
        assert_eq!(scan["result"]["added"], json!(0));
        assert_eq!(scan["result"]["removed"], json!(0));
    });
}

/// `plant-phenotypes` reads a family's appearances and replaces them as one operation,
/// refusing a set the family validator will not accept.
#[test]
fn plant_phenotypes_reads_replaces_and_refuses_an_invalid_set() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        scratch_root(ctx, "phenotypes");
        ctx.scene_edit.project_phase = ProjectPhase::Ready;
        let plant = seed_native_plant(ctx);

        // A scaffolded family carries exactly one appearance, which is why nothing could
        // author a transition before this command existed.
        let read = reg.dispatch(
            ctx,
            &json!({ "cmd": "plant-phenotypes", "params": { "plant": plant.to_string() } }),
        );
        assert_eq!(read["ok"], json!(true));
        assert_eq!(
            read["result"]["phenotypes"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(read["result"]["phenotypes"][0]["role"], json!("healthy"));

        // A second appearance on the same variation with a different role is exactly what a
        // seasonal transition needs, and what the family validator allows.
        let replaced = reg.dispatch(
            ctx,
            &json!({
                "cmd": "plant-phenotypes",
                "params": {
                    "plant": plant.to_string(),
                    "phenotypes": [
                        { "id": 0, "role": "healthy", "variation": 0 },
                        { "id": 1, "role": "senescent", "variation": 0, "seasonWindow": [700, 900] },
                    ],
                },
            }),
        );
        assert_eq!(replaced["ok"], json!(true));
        assert_eq!(
            replaced["result"]["phenotypes"].as_array().map(Vec::len),
            Some(2)
        );

        // It persisted rather than only being echoed: a later read sees the new set.
        let again = reg.dispatch(
            ctx,
            &json!({ "cmd": "plant-phenotypes", "params": { "plant": plant.to_string() } }),
        );
        assert_eq!(again["result"]["phenotypes"][1]["role"], json!("senescent"));
        assert_eq!(
            again["result"]["phenotypes"][1]["seasonWindow"],
            json!([700, 900])
        );

        // And the validator is the authority: a family with no healthy appearance is refused,
        // and refusing means the stored set did not change.
        let refused = reg.dispatch(
            ctx,
            &json!({
                "cmd": "plant-phenotypes",
                "params": {
                    "plant": plant.to_string(),
                    "phenotypes": [{ "id": 0, "role": "senescent", "variation": 0 }],
                },
            }),
        );
        assert_eq!(refused["ok"], json!(false));
        let unchanged = reg.dispatch(
            ctx,
            &json!({ "cmd": "plant-phenotypes", "params": { "plant": plant.to_string() } }),
        );
        assert_eq!(
            unchanged["result"]["phenotypes"].as_array().map(Vec::len),
            Some(2),
            "a refused replacement still changed the family"
        );
    });
}

/// `plant-season-phenotype` resolves through the engine's own rule: lifecycle first, then the
/// seasonal window, with the cooked phenotype as the fallback.
#[test]
fn plant_season_phenotype_follows_lifecycle_then_season() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        scratch_root(ctx, "season");
        ctx.scene_edit.project_phase = ProjectPhase::Ready;
        let plant = seed_native_plant(ctx);
        reg.dispatch(
            ctx,
            &json!({
                "cmd": "plant-phenotypes",
                "params": {
                    "plant": plant.to_string(),
                    "phenotypes": [
                        { "id": 0, "role": "healthy", "variation": 0 },
                        { "id": 1, "role": "senescent", "variation": 0, "seasonWindow": [600, 800] },
                    ],
                },
            }),
        );
        let mut at = |mille: u32, lifecycle: Option<&str>| {
            let mut params = json!({ "plant": plant.to_string(), "seasonMille": mille });
            if let Some(state) = lifecycle {
                params["lifecycle"] = json!(state);
            }
            reg.dispatch(
                ctx,
                &json!({ "cmd": "plant-season-phenotype", "params": params }),
            )
        };

        // Inside the authored window the senescent appearance wins; outside it the healthy one
        // does. Both directions, because a resolver stuck on either answer passes one of them.
        assert_eq!(at(700, None)["result"]["phenotype"], json!(1));
        assert_eq!(at(100, None)["result"]["phenotype"], json!(0));

        // Lifecycle overrides the season: a dead plant in autumn is dead, not autumnal. With no
        // dead appearance authored it falls back to the cooked one rather than taking the
        // seasonal match, which is the part a season-first resolver gets wrong.
        assert_eq!(at(700, Some("dead"))["result"]["phenotype"], json!(0));

        assert_eq!(at(1000, None)["ok"], json!(false));
    });
}

/// `plant-proxies` reports the derived proxies in metres, which is what the overlay draws.
#[test]
fn plant_proxies_reports_what_the_family_derived() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        scratch_root(ctx, "proxies");
        ctx.scene_edit.project_phase = ProjectPhase::Ready;
        let plant = seed_native_plant(ctx);
        let reply = reg.dispatch(
            ctx,
            &json!({ "cmd": "plant-proxies", "params": { "plant": plant.to_string() } }),
        );
        assert_eq!(reply["ok"], json!(true));

        // Proxies are derived, so a family that grew nothing thick enough legitimately reports
        // none. What must hold either way is the SHAPE of the reply — the overlay reads these
        // fields directly, and a missing array would draw nothing while looking fine.
        assert!(reply["result"]["collision"].is_array());
        assert!(reply["result"]["navigation"].is_array());
        for proxy in reply["result"]["collision"]
            .as_array()
            .unwrap_or(&Vec::new())
        {
            // Metres, not Q15.16 bits: a dimension in raw fixed point would draw a capsule
            // sixty-five thousand times too big and look like a broken overlay rather than a
            // unit mistake.
            let dimensions = proxy["dimensionsM"].as_array().expect("dimensions");
            assert_eq!(dimensions.len(), 3);
            assert!(
                dimensions
                    .iter()
                    .all(|value| value.as_f64().is_some_and(|v| v < 1.0e4))
            );
        }
    });
}

/// `scan-assets` (and the other project-gated commands) refuse without a loaded project.
#[test]
fn scan_assets_requires_a_project() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let scan = reg.dispatch(ctx, &json!({ "cmd": "scan-assets" }));
        assert_eq!(scan["ok"], json!(false));
        assert_eq!(scan["error"]["message"], json!("no project loaded"));
    });
}

/// `assign-asset` resolves an `AssetSelector` by id and by name, assigns the mesh slot,
/// and returns a decimal-string id matching the catalog row.
#[test]
fn assign_asset_resolves_id_and_name() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let mesh_id = seed_mesh(ctx, "cube");
        let entity = ctx.scene_edit.active_scene().create_entity("box");
        let entity_uuid = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();

        let by_id = reg.dispatch(
            ctx,
            &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "mesh", "asset": mesh_id.to_string() } }),
        );
        assert_eq!(by_id["ok"], json!(true));
        assert_eq!(by_id["result"]["id"], json!(mesh_id.to_string()));
        assert_eq!(by_id["result"]["slot"], json!("mesh"));
        assert_eq!(by_id["result"]["name"], json!("cube"));
        assert_eq!(
            ctx.scene_edit
                .active_scene()
                .component::<Mesh>(entity)
                .unwrap()
                .mesh
                .value(),
            mesh_id
        );

        let by_name = reg.dispatch(
            ctx,
            &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "mesh", "asset": "cube" } }),
        );
        assert_eq!(by_name["result"]["id"], json!(mesh_id.to_string()));

        // The null sentinel (id 0) clears the slot rather than resolving an asset.
        let clear = reg.dispatch(
            ctx,
            &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "mesh", "asset": "0" } }),
        );
        assert_eq!(clear["result"]["id"], json!("0"));
        assert_eq!(
            ctx.scene_edit
                .active_scene()
                .component::<Mesh>(entity)
                .unwrap()
                .mesh
                .value(),
            0
        );
    });
}

/// `assign-asset` on a texture slot attaches a `Material` and writes the texture id.
#[test]
fn assign_asset_albedo_overrides_slot_zero() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let tex_id = {
            let id = saffron_core::Uuid::new();
            ctx.assets.register_imported_asset(AssetEntry {
                id,
                name: "albedo".to_owned(),
                asset_type: AssetType::Texture,
                ..AssetEntry::default()
            });
            id.value()
        };
        let entity = ctx.scene_edit.active_scene().create_entity("box");
        let entity_uuid = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();
        let reply = reg.dispatch(
            ctx,
            &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "albedo", "asset": tex_id.to_string() } }),
        );
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["slot"], json!("albedo"));
        // A MaterialSet with a default slot is attached, and the albedo texture lands as
        // a per-object override (a decimal-string uuid) on slot 0.
        let albedo = ctx
            .scene_edit
            .active_scene()
            .with_component::<MaterialSet, _>(entity, |set| {
                set.slots
                    .first()
                    .and_then(|s| s.overrides.as_object())
                    .and_then(|o| o.get("albedoTexture"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .ok()
            .flatten();
        assert_eq!(albedo.as_deref(), Some(tex_id.to_string().as_str()));
    });
}

/// `set-active-view` maps `scene` / `assetPreview` and errors on an unknown view.
#[test]
fn set_active_view_maps_and_errors() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let scene = reg.dispatch(
            ctx,
            &json!({ "cmd": "set-active-view", "params": { "view": "scene" } }),
        );
        assert_eq!(scene["ok"], json!(true));
        assert_eq!(scene["result"]["view"], json!("scene"));

        let preview = reg.dispatch(
            ctx,
            &json!({ "cmd": "set-active-view", "params": { "view": "assetPreview" } }),
        );
        assert_eq!(preview["result"]["view"], json!("assetPreview"));

        let bad = reg.dispatch(
            ctx,
            &json!({ "cmd": "set-active-view", "params": { "view": "nope" } }),
        );
        assert_eq!(bad["ok"], json!(false));
        assert_eq!(
            bad["error"]["message"],
            json!("unknown view 'nope' (expected 'scene' or 'assetPreview')")
        );
    });
}

/// `rename-asset` renames the catalog row and returns the new `{id, name}`.
#[test]
fn rename_asset_round_trips() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let id = seed_mesh(ctx, "old-name");
        let reply = reg.dispatch(
            ctx,
            &json!({ "cmd": "rename-asset", "params": { "asset": id.to_string(), "name": "new-name" } }),
        );
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["id"], json!(id.to_string()));
        assert_eq!(reply["result"]["name"], json!("new-name"));
        assert_eq!(
            ctx.assets
                .catalog()
                .find(saffron_core::Uuid(id))
                .unwrap()
                .name,
            "new-name"
        );
    });
}

/// `rename-asset` persists the new name to a durable `<path>.smeta` sidecar, so it survives a
/// cold scan without a project save. Asserts the on-disk sidecar (not a rescan): `with_stub`
/// reuses one `AssetServer`, so `preserve_name_folder` would mask a regression — the true
/// cold-scan proof lives in the assets-crate unit test.
#[test]
fn rename_asset_writes_a_durable_smeta() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        scratch_root(ctx, "renamesmeta");
        // A texture row + its directory so the sidecar writer's parent exists.
        let tex = saffron_core::Uuid::new();
        let rel = format!("textures/{}.png", tex.value());
        std::fs::create_dir_all(ctx.assets.root.join("textures")).unwrap();
        ctx.assets.register_imported_asset(AssetEntry {
            id: tex,
            name: "old".to_owned(),
            asset_type: AssetType::Texture,
            path: rel.clone(),
            ..AssetEntry::default()
        });

        let reply = reg.dispatch(
            ctx,
            &json!({ "cmd": "rename-asset", "params": { "asset": tex.value().to_string(), "name": "Brick Wall" } }),
        );
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["name"], json!("Brick Wall"));

        let smeta = ctx.assets.root.join(format!("{rel}.smeta"));
        assert!(smeta.exists(), "rename writes the sidecar beside the file");
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&smeta).unwrap()).unwrap();
        assert_eq!(doc["name"], json!("Brick Wall"));
    });
}

/// `create-asset-folder` adds a folder and `list-assets` reflects it; an invalid path
/// errors.
#[test]
fn create_asset_folder_and_list() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let made = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-asset-folder", "params": { "folder": "props/crates" } }),
        );
        assert_eq!(made["ok"], json!(true));
        assert_eq!(made["result"]["folders"], json!(["props/crates"]));

        let bad = reg.dispatch(
            ctx,
            &json!({ "cmd": "create-asset-folder", "params": { "folder": "/leading" } }),
        );
        assert_eq!(bad["ok"], json!(false));
    });
}

/// `material-create` then `material-get` round-trips the `.smat`; `material-set-graph`
/// stores an opaque graph that `material-get` reads back verbatim.
#[test]
fn material_set_graph_keeps_graph_opaque() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        scratch_root(ctx, "material-graph");
        let create = reg.dispatch(
            ctx,
            &json!({ "cmd": "material-create", "params": { "name": "Mat" } }),
        );
        assert_eq!(create["ok"], json!(true));
        let id = create["result"]["id"].as_str().unwrap().to_owned();

        // A graph object with a codegen-only shape (no fold) is stored verbatim.
        let graph = json!({ "nodes": [{ "id": 1, "type": "noise" }], "edges": [] });
        let set = reg.dispatch(
            ctx,
            &json!({ "cmd": "material-set-graph", "params": { "material": id, "graph": graph } }),
        );
        assert_eq!(set["ok"], json!(true), "set-graph: {set:?}");
        assert_eq!(set["result"]["id"], json!(id));

        let get = reg.dispatch(
            ctx,
            &json!({ "cmd": "material-get", "params": { "material": id } }),
        );
        assert_eq!(get["ok"], json!(true), "get: {get:?}");
        assert_eq!(get["result"]["graph"], graph, "graph round-trips opaque");
    });
}

#[test]
fn material_surface_union_round_trips_complete_thin_sheet_parameters() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        scratch_root(ctx, "material-thin-sheet");
        let create = reg.dispatch(
            ctx,
            &json!({ "cmd": "material-create", "params": { "name": "Leaf" } }),
        );
        let id = create["result"]["id"].as_str().unwrap();
        let surface = json!({
            "model": "thin-sheet-foliage",
            "parameters": {
                "frontAlbedoResponse": 20_000,
                "backAlbedoResponse": 21_000,
                "thicknessBits": 655,
                "absorptionColorBits": [1_000, 2_000, 3_000],
                "transmissionColorBits": [10_000, 11_000, 12_000],
                "roughness": 32_768,
                "normalBehavior": "face-forward-back",
                "coverageSource": { "kind": "albedo-alpha" },
                "coverage": {
                    "referenceCutoff": 30_000,
                    "sourceExtent": [512, 256],
                    "spatialHashSalt": "9876543210987654321",
                    "classification": "masked",
                    "mipHashes": []
                },
                "voxelMoments": {
                    "occupancy": 20_000,
                    "albedoMeanBits": [4_000, 5_000, 6_000],
                    "roughnessMean": 30_000,
                    "transmissionMeanBits": [7_000, 8_000, 9_000],
                    "thicknessMeanBits": 327,
                    "normalSecondMomentsBits": [1, 2, 3, 4, 5, 6]
                },
                "opacityMicromap": {
                    "enabled": true,
                    "maxSubdivision": 5,
                    "transparentThreshold": 1_000,
                    "opaqueThreshold": 60_000
                },
                "energyLimit": 50_000
            }
        });
        let update = reg.dispatch(
            ctx,
            &json!({ "cmd": "material-update", "params": { "material": id, "surface": surface } }),
        );
        assert_eq!(update["ok"], json!(true), "update: {update:?}");
        let get = reg.dispatch(
            ctx,
            &json!({ "cmd": "material-get", "params": { "material": id } }),
        );
        assert_eq!(get["ok"], json!(true), "get: {get:?}");
        assert_eq!(get["result"]["surface"], surface);
    });
}

#[test]
fn vegetation_import_and_summary_use_the_native_map_contract() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        scratch_root(ctx, "vegetation-summary");
        ctx.scene_edit.project_phase = ProjectPhase::Ready;
        let source = ctx.assets.root.parent().unwrap().join("world.svegmap");
        let bounds = saffron_spatial::WorldBounds::new([0; 3], [4096; 3]).unwrap();
        let map = saffron_vegetation::VegetationMapAsset {
            version: saffron_vegetation::VEGETATION_MAP_VERSION,
            id: saffron_core::Uuid(9_001),
            name: "World vegetation".to_owned(),
            bounds,
            chunk_layout: saffron_vegetation::VegetationMapChunkLayout {
                level: 0,
                schema_hash: saffron_vegetation::vegetation_map_chunk_schema_hash(),
            },
            generation: 0,
            inventory: Vec::new(),
        };
        std::fs::write(
            &source,
            saffron_vegetation::write_vegetation_map_asset(&map).unwrap(),
        )
        .unwrap();

        let imported = reg.dispatch(
            ctx,
            &json!({ "cmd": "import-vegetation-asset", "params": { "path": source.to_string_lossy() } }),
        );
        assert_eq!(imported["ok"], json!(true), "import: {imported:?}");
        assert_eq!(imported["result"]["id"], json!("9001"));
        assert_eq!(imported["result"]["type"], json!("vegetation-map"));

        let summary = reg.dispatch(
            ctx,
            &json!({ "cmd": "vegetation-asset-summary", "params": { "asset": "9001" } }),
        );
        assert_eq!(summary["ok"], json!(true), "summary: {summary:?}");
        assert_eq!(
            summary["result"]["summary"]["kind"],
            json!("vegetation-map")
        );
        assert_eq!(
            summary["result"]["summary"]["asset"]["name"],
            json!("World vegetation")
        );
        assert_eq!(
            summary["result"]["summary"]["asset"]["layerCount"],
            json!(0)
        );
        assert_eq!(
            summary["result"]["summary"]["asset"]["validation"],
            json!({ "valid": true, "issues": [] })
        );
        assert_eq!(
            summary["result"]["summary"]["asset"]["dependencies"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            summary["result"]["summary"]["asset"]
                .get("latestCook")
                .is_none()
        );
        assert_eq!(summary["result"]["layers"], json!([]));
    });
}

#[test]
fn plant_validate_recook_and_summary_share_the_single_compiler_route() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        scratch_root(ctx, "plant-control");
        ctx.scene_edit.project_phase = ProjectPhase::Ready;
        let plant = seed_native_plant(ctx);

        let validation = reg.dispatch(
            ctx,
            &json!({ "cmd": "plant-validate", "params": { "plant": plant.to_string() } }),
        );
        assert_eq!(validation["ok"], json!(true), "validation: {validation:?}");
        assert_eq!(validation["result"]["plant"], json!(plant.to_string()));
        assert_eq!(validation["result"]["validation"]["valid"], json!(true));
        assert_eq!(validation["result"]["sources"], json!([]));
        assert_eq!(validation["result"]["statistics"]["sources"], json!("1"));
        assert_eq!(validation["result"]["statistics"]["materials"], json!("1"));
        assert!(
            validation["result"]["dependencies"]
                .as_array()
                .is_some_and(|dependencies| dependencies.len() >= 5)
        );

        let first = reg.dispatch(
            ctx,
            &json!({ "cmd": "plant-recook", "params": { "plant": plant.to_string() } }),
        );
        assert_eq!(first["ok"], json!(true), "first recook: {first:?}");
        assert_eq!(first["result"]["validation"]["valid"], json!(true));
        assert_eq!(
            first["result"]["artifactHash"].as_str().map(str::len),
            Some(64)
        );
        assert_eq!(
            first["result"]["familyHash"].as_str().map(str::len),
            Some(64)
        );

        let second = reg.dispatch(
            ctx,
            &json!({ "cmd": "plant-recook", "params": { "plant": plant.to_string() } }),
        );
        assert_eq!(second["ok"], json!(true), "second recook: {second:?}");
        assert_eq!(
            second["result"]["artifactHash"],
            first["result"]["artifactHash"]
        );
        assert_eq!(second["result"]["cacheHit"], json!(true));

        let summary = reg.dispatch(
            ctx,
            &json!({ "cmd": "vegetation-asset-summary", "params": { "asset": plant.to_string() } }),
        );
        assert_eq!(summary["ok"], json!(true), "summary: {summary:?}");
        assert_eq!(summary["result"]["summary"]["kind"], json!("plant"));
        assert_eq!(
            summary["result"]["summary"]["asset"]["validation"]["valid"],
            json!(true)
        );
        assert_eq!(
            summary["result"]["summary"]["asset"]["provenance"],
            json!([])
        );
        assert!(
            summary["result"]["summary"]["asset"]["dependencies"]
                .as_array()
                .is_some_and(|dependencies| dependencies.len() >= 5)
        );
        assert!(
            summary["result"]["summary"]["asset"]
                .get("latestCook")
                .is_none(),
            "standalone plant publications have no durable latest pointer"
        );

        let empty_profile = reg.dispatch(
            ctx,
            &json!({
                "cmd": "plant-recook",
                "params": { "plant": plant.to_string(), "platformProfile": "" }
            }),
        );
        assert_eq!(empty_profile["ok"], json!(false));
        assert_eq!(
            empty_profile["error"]["message"],
            json!("platformProfile cannot be empty")
        );
    });
}

#[test]
fn biome_summary_reports_recursive_source_validation_and_dependencies() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        scratch_root(ctx, "biome-summary");
        ctx.scene_edit.project_phase = ProjectPhase::Ready;
        let plant = seed_native_plant(ctx);
        let biome = seed_biome(ctx, plant);
        let summary = reg.dispatch(
            ctx,
            &json!({ "cmd": "vegetation-asset-summary", "params": { "asset": biome.to_string() } }),
        );
        assert_eq!(summary["ok"], json!(true), "summary: {summary:?}");
        assert_eq!(summary["result"]["summary"]["kind"], json!("biome"));
        assert_eq!(
            summary["result"]["summary"]["asset"]["validation"],
            json!({ "valid": true, "issues": [] })
        );
        assert_eq!(
            summary["result"]["summary"]["asset"]["plantPalette"],
            json!([plant.to_string()])
        );
        assert_eq!(
            summary["result"]["summary"]["asset"]["dependencies"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            summary["result"]["summary"]["asset"]["provenance"],
            json!([])
        );
        assert!(
            summary["result"]["summary"]["asset"]
                .get("latestCook")
                .is_none()
        );
    });
}

/// An exported package carries the cooked artifacts and leaves the authored vegetation sources
/// behind: the runtime binds a generation from the artifact store and never reads them.
#[test]
fn authored_vegetation_sources_are_not_packaged() {
    for name in [
        "forest.splant",
        "meadow.sbiome",
        "world.svegmap",
        "world.svegmap.data",
    ] {
        assert!(
            super::is_authored_vegetation(
                std::path::Path::new("/project/assets/vegetation")
                    .join(name)
                    .as_path()
            ),
            "{name} is an authored source"
        );
    }
    // Everything a runtime does read stays.
    for name in [
        "bark.smat",
        "oak.smesh",
        "oak.smodel",
        "bark.png",
        "world.svegmanifest",
        "cell.svegcell",
        "oak.splantc",
    ] {
        assert!(
            !super::is_authored_vegetation(
                std::path::Path::new("/project/assets").join(name).as_path()
            ),
            "{name} is packaged"
        );
    }
}

#[test]
fn plant_source_dto_keeps_observed_hash_and_complete_provenance() {
    let source = saffron_vegetation::PlantSourceReference {
        id: 7,
        locator: saffron_vegetation::PlantSourceLocator::File("file:///plants/oak.glb".to_owned()),
        role: saffron_vegetation::PlantSourceRole::Geometry,
        selector: saffron_vegetation::PlantSourceSelector::Element {
            id: 8,
            path: "Oak/Trunk".to_owned(),
        },
        content_hash: [1; 32],
        settings: saffron_vegetation::PlantImportSettings::default(),
        provenance: saffron_vegetation::SourceProvenance {
            source: "studio-library".to_owned(),
            source_uri: "https://assets.example/oak".to_owned(),
            license_id: "CC-BY-4.0".to_owned(),
            license_uri: "https://creativecommons.org/licenses/by/4.0/".to_owned(),
            author: "Ada".to_owned(),
            attribution: "Oak by Ada".to_owned(),
            requires_attribution: true,
        },
    };
    let dto = plant_source_reference_dto(&source, &BTreeMap::from([(7, [2; 32])]));
    assert_eq!(dto.content_hash, "02".repeat(32));
    assert_eq!(dto.provenance.author, "Ada");
    assert_eq!(dto.provenance.attribution, "Oak by Ada");
    assert!(dto.provenance.requires_attribution);
    assert!(matches!(
        dto.selector,
        PlantSourceSelectorDto::Element { id, path }
            if id.0 == "00000000000000000000000000000008" && path == "Oak/Trunk"
    ));
}

/// `thumbnail-cache stats` reports a clean cache; an unknown action errors.
#[test]
fn thumbnail_cache_stats_and_unknown_action() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        scratch_root(ctx, "thumb-cache");
        let stats = reg.dispatch(
            ctx,
            &json!({ "cmd": "thumbnail-cache", "params": { "action": "stats" } }),
        );
        assert_eq!(stats["ok"], json!(true));
        assert_eq!(stats["result"]["entries"], json!(0));

        let bad = reg.dispatch(
            ctx,
            &json!({ "cmd": "thumbnail-cache", "params": { "action": "nope" } }),
        );
        assert_eq!(bad["ok"], json!(false));
        assert_eq!(
            bad["error"]["message"],
            json!("unknown action 'nope' (stats|clear)")
        );
    });
}

/// `get-project` reports the editor's project identity; it is loaded after a field set.
#[test]
fn get_project_reports_identity() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        ctx.scene_edit.project_phase = ProjectPhase::Ready;
        ctx.scene_edit.project_name = "demo".to_owned();
        ctx.scene_edit.project_display_name = "Demo".to_owned();
        let reply = reg.dispatch(ctx, &json!({ "cmd": "get-project" }));
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["loaded"], json!(true));
        assert_eq!(reply["result"]["name"], json!("demo"));
        assert_eq!(reply["result"]["displayName"], json!("Demo"));
    });
}

#[test]
fn new_project_rejects_an_invalid_name_before_queueing_load() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let reply = reg.dispatch(
            ctx,
            &json!({ "cmd": "new-project", "params": { "name": "Bad_Name" } }),
        );
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(
            reply["error"]["message"],
            json!("invalid project name 'Bad_Name'")
        );
        assert!(ctx.scene_edit.project_load_inbox.is_none());
    });
}

/// The asset commands register in their frozen manifest order, with the global `quit` command
/// checked separately at the end of the registry.
#[test]
fn asset_commands_register_in_manifest_order() {
    const FROZEN: &[&str] = &[
        "get-project",
        "project-status",
        "cancel-load",
        "new-project",
        "create-script",
        "open-project",
        "import-model",
        "instantiate-model",
        "asset-placement",
        "import-texture",
        "import-lut",
        "import-vegetation-asset",
        "list-assets",
        "vegetation-map-layer-commit",
        "vegetation-map-chunk-commit",
        "vegetation-map-chunk-read",
        "vegetation-asset-summary",
        "scan-assets",
        "extract-subasset",
        "clear-extraction",
        "reimport-model",
        "model-info",
        "asset-references",
        "get-asset-model",
        "enter-asset-preview",
        "exit-asset-preview",
        "set-active-view",
        "clean-assets",
        "delete-unused",
        "rename-asset",
        "create-asset-folder",
        "rename-asset-folder",
        "delete-asset-folder",
        "move-asset",
        "asset-usages",
        "probe-asset",
        "delete-asset",
        "assign-asset",
        "material-create",
        "material-assign",
        "material-import",
        "material-list",
        "material-get",
        "material-schema",
        "material-update",
        "preview-render",
        "material-set-graph",
        "material-create-instance",
        "material-set-override",
        "material-compile-graph",
        "material-cook",
        "export-app",
        "save-scene",
        "load-scene",
        "save-project",
        "load-project",
        "reload-project",
        "get-stores",
        "set-stores",
        "screenshot",
        "get-thumbnail",
        "view-asset",
        "thumbnail-cache",
    ];
    let reg = registry();
    let names: Vec<&str> = reg.rows().iter().map(|c| c.name).collect();
    let start = names
        .iter()
        .position(|&n| n == "get-project")
        .expect("get-project is registered");
    assert_eq!(
        &names[start..start + FROZEN.len()],
        FROZEN,
        "the asset domain registers contiguously in the frozen manifest order"
    );
    // `quit` is the last command in the registry.
    assert_eq!(names.last(), Some(&"quit"));
}

/// `asset-usages` reports a mesh slot that references the queried asset, with the entity
/// id as a decimal string.
#[test]
fn asset_usages_reports_mesh_slot() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let mesh_id = seed_mesh(ctx, "cube");
        let entity = ctx.scene_edit.active_scene().create_entity("box");
        let _ = ctx.scene_edit.active_scene().add_component(
            entity,
            Mesh {
                mesh: saffron_core::Uuid(mesh_id),
            },
        );
        let entity_uuid = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();

        let reply = reg.dispatch(
            ctx,
            &json!({ "cmd": "asset-usages", "params": { "asset": mesh_id.to_string() } }),
        );
        assert_eq!(reply["ok"], json!(true));
        let usages = reply["result"]["usages"].as_array().unwrap();
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0]["slot"], json!("mesh"));
        assert_eq!(usages[0]["entity"], json!(entity_uuid));
    });
}

#[test]
fn vegetation_map_usage_and_delete_clear_the_scene_field() {
    let reg = registry();
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |ctx| {
        let map = saffron_core::Uuid::new();
        ctx.assets.register_imported_asset(AssetEntry {
            id: map,
            name: "World vegetation".to_owned(),
            asset_type: AssetType::VegetationMap,
            ..AssetEntry::default()
        });
        let entity = ctx.scene_edit.active_scene().create_entity("Vegetation");
        ctx.scene_edit
            .active_scene()
            .add_component(entity, VegetationField { map, enabled: true })
            .unwrap();
        let entity_id = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();

        let usages = reg.dispatch(
            ctx,
            &json!({ "cmd": "asset-usages", "params": { "asset": map.value().to_string() } }),
        );
        assert_eq!(usages["ok"], json!(true), "usages: {usages:?}");
        assert_eq!(
            usages["result"]["usages"],
            json!([{
                "entity": entity_id,
                "entityName": "Vegetation",
                "slot": "vegetationField.map"
            }])
        );

        let deleted = reg.dispatch(
            ctx,
            &json!({ "cmd": "delete-asset", "params": { "asset": map.value().to_string() } }),
        );
        assert_eq!(deleted["ok"], json!(true), "delete: {deleted:?}");
        assert_eq!(deleted["result"]["cleared"], usages["result"]["usages"]);
        assert!(ctx.assets.catalog().find(map).is_none());
        assert_eq!(
            ctx.scene_edit
                .active_scene()
                .component::<VegetationField>(entity)
                .unwrap(),
            VegetationField {
                map: saffron_core::Uuid(0),
                enabled: false,
            }
        );
    });
}
