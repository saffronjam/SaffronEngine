//! The whole-scene document serde and the v1→v4 version migrations. Every key spelling and every
//! migration branch is load-bearing.
//!
//! [`Scene::scene_from_json`] is the single reader and carries every migration: v1 has no
//! `environment` block, pre-v3 no per-entity `Relationship`, pre-v4 no `componentOrder`. An unknown
//! component name warns and is skipped; a version outside `1..=SCENE_VERSION` is an error.
//!
//! Entities are created preserving their uuids and cross-entity references resolve in the post-loop
//! [`Scene::relink_hierarchy`] pass, so a child entry may precede its parent. `enter_play` round-trips
//! through this serde, so round-trip fidelity here is what makes play mode correct.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use saffron_core::Uuid;
use saffron_json::{dump_json_sorted, json_u64_or, parse_json, uuid_to_json};

use crate::component::{ComponentOrder, IdComponent};
use crate::error::{Error, Result};
use crate::registry::ComponentRegistry;
use crate::scene::Scene;
use crate::serde::{environment_from_json, environment_to_json};

/// The scene document schema version. Version history:
///
/// - **1** — entities only.
/// - **2** — adds the top-level `environment` block.
/// - **3** — adds the per-entity `Relationship` (durable parent uuid).
/// - **4** — adds the per-entity `componentOrder` array.
///
/// [`Scene::scene_from_json`] migrates every older document forward; a document written by
/// this build is always version `4`.
pub const SCENE_VERSION: i64 = 4;

impl Scene {
    /// Serializes the scene to a `{version, environment, entities:[{id, components,
    /// componentOrder}]}` document (no file IO), embeddable in a larger project document.
    ///
    /// `&mut self` because reconciling each entity's [`ComponentOrder`] (via
    /// [`ComponentRegistry::component_order`]) writes the reconciled order back onto the
    /// entity.
    #[must_use]
    pub fn scene_to_json(&mut self, reg: &ComponentRegistry) -> Value {
        let mut ids: Vec<(Uuid, crate::Entity)> = Vec::new();
        self.for_each::<&IdComponent, _>(|entity, id| ids.push((id.id, entity)));

        let mut entities: Vec<Value> = Vec::with_capacity(ids.len());
        for (uuid, entity) in ids {
            // Preview ghosts only render; they are never persisted.
            if self.has_component::<crate::PreviewGhost>(entity) {
                continue;
            }
            let components = reg.serialize_entity(self, entity);
            let order = reg.component_order(self, entity);
            let order_values: Vec<Value> = order.into_iter().map(Value::String).collect();
            entities.push(Value::Object(Map::from_iter([
                ("id".to_string(), uuid_to_json(uuid.value())),
                ("components".to_string(), components),
                ("componentOrder".to_string(), Value::Array(order_values)),
            ])));
        }

        Value::Object(Map::from_iter([
            ("version".to_string(), Value::from(SCENE_VERSION)),
            (
                "environment".to_string(),
                environment_to_json(&self.environment),
            ),
            ("entities".to_string(), Value::Array(entities)),
        ]))
    }

    /// Replaces the scene's entities from a [`Scene::scene_to_json`] document, migrating
    /// older versions forward.
    ///
    /// The world is cleared, then each entry creates a raw entity preserving its uuid and
    /// deserializes its components; cross-entity references resolve in the post-loop
    /// [`Scene::relink_hierarchy`]. See the module docs for the per-version migration
    /// branches.
    ///
    /// # Errors
    ///
    /// [`Error::Document`] when the root is not an object, the `entities` array is missing,
    /// an entry is not an object, or an entity is missing its `id`;
    /// [`Error::UnsupportedVersion`] when the version is outside `[1, SCENE_VERSION]`;
    /// [`Error::Deserialize`] when a component body fails.
    pub fn scene_from_json(&mut self, reg: &ComponentRegistry, doc: &Value) -> Result<()> {
        if !doc.is_object() {
            return Err(Error::Document("scene root is not an object".into()));
        }
        let version = i64::try_from(json_u64_or(doc, "version", 0)).unwrap_or(i64::MAX);
        if !(1..=SCENE_VERSION).contains(&version) {
            return Err(Error::UnsupportedVersion(version));
        }
        let Some(Value::Array(entries)) = doc.as_object().and_then(|m| m.get("entities")) else {
            return Err(Error::Document("scene missing 'entities' array".into()));
        };

        // v1 has no "environment" block; environment_from_json defaults it. v2+ carries one.
        let env_value = doc
            .as_object()
            .and_then(|m| m.get("environment"))
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        self.environment = environment_from_json(&env_value);

        self.clear();

        for entry in entries {
            if !entry.is_object() {
                return Err(Error::Document("entity entry is not an object".into()));
            }
            let uuid = json_u64_or(entry, "id", 0);
            if uuid == 0 {
                return Err(Error::Document("entity missing 'id'".into()));
            }
            let entity = self.spawn_with_id(Uuid(uuid));

            if let Some(components) = entry.as_object().and_then(|m| m.get("components"))
                && components.is_object()
            {
                reg.deserialize_entity(self, entity, components)?;
            }

            let order = entry.as_object().and_then(|m| m.get("componentOrder"));
            if version >= 4 && matches!(order, Some(Value::Array(_))) {
                let names: Vec<String> = order
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect();
                let _ = self.add_component(entity, ComponentOrder { names });
            } else {
                reg.sort_component_order(self, entity);
            }
            let _ = reg.component_order(self, entity);
        }

        // Resolve uuid → live handle after the whole loop, since a parent uuid may point at an
        // entity created later in the array.
        self.relink_hierarchy();
        Ok(())
    }

    /// Writes the scene to `path` as a pretty-printed (2-space) document.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be written.
    pub fn write_scene(&mut self, reg: &ComponentRegistry, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let text = dump_json_sorted(&self.scene_to_json(reg), 2);
        fs::write(path, text).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })
    }

    /// Reads a scene document from `path`, parses it, and loads it.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read; [`Error::JsonGateway`] when the contents
    /// are not valid JSON; the [`Scene::scene_from_json`] errors otherwise.
    pub fn read_scene(&mut self, reg: &ComponentRegistry, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        let doc = parse_json(&text)?;
        self.scene_from_json(reg, &doc)
    }
}

#[cfg(test)]
mod tests {
    use glam::{Mat4, Vec3};
    use saffron_json::dump_json_sorted;
    use serde_json::Value;

    use saffron_core::Uuid;

    use crate::component::{
        Camera, IdComponent, Name, PreviewGhost, Relationship, SkinnedMesh, Transform,
        VegetationField, WorldTransform,
    };
    use crate::registry::register_builtin_components;
    use crate::scene::{Entity, Scene};

    use super::SCENE_VERSION;

    /// The component registry used by every document test.
    fn registry() -> crate::registry::ComponentRegistry {
        register_builtin_components()
    }

    /// Finds an entity by its `Name`.
    fn find_by_name(scene: &mut Scene, name: &str) -> Entity {
        let mut found = Entity::NULL;
        let target = name.to_string();
        scene.for_each::<(&IdComponent, &Name), _>(|e, (_, n)| {
            if n.name == target {
                found = e;
            }
        });
        found
    }

    fn id_of(scene: &Scene, e: Entity) -> Uuid {
        scene.component::<IdComponent>(e).unwrap().id
    }

    /// A `PreviewGhost`-tagged entity is an asset-placement preview: it renders, but is never
    /// persisted. `scene_to_json` skips it (and only it), so a save taken mid-drag is identical to
    /// one with no preview active.
    #[test]
    fn preview_ghost_is_excluded_from_serialization() {
        let reg = registry();
        let mut scene = Scene::new();
        scene.create_entity("Keep");
        let ghost = scene.create_entity("Ghost");
        scene.add_component(ghost, PreviewGhost::default()).unwrap();

        let doc = scene.scene_to_json(&reg);
        let mut loaded = Scene::new();
        loaded.scene_from_json(&reg, &doc).unwrap();

        let mut names: Vec<String> = Vec::new();
        loaded.for_each::<&Name, _>(|_, n| names.push(n.name.clone()));
        assert!(
            names.contains(&"Keep".to_string()),
            "untagged entity persists"
        );
        assert!(
            !names.contains(&"Ghost".to_string()),
            "PreviewGhost entity must not persist"
        );
    }

    /// `subtree_entities` returns the root plus every descendant, so the placement command can tag
    /// (and the save/pick/outliner filters can skip) a whole instantiated model subtree.
    #[test]
    fn subtree_entities_walks_root_and_descendants() {
        let mut scene = Scene::new();
        let root = scene.create_entity("root");
        let child = scene.create_entity("child");
        let grandchild = scene.create_entity("grandchild");
        scene.set_parent(child, Some(root), false).unwrap();
        scene.set_parent(grandchild, Some(child), false).unwrap();

        let subtree = scene.subtree_entities(root);
        assert_eq!(subtree.len(), 3, "root + child + grandchild");
        assert!(subtree.contains(&root));
        assert!(subtree.contains(&child));
        assert!(subtree.contains(&grandchild));

        assert_eq!(scene.subtree_entities(grandchild), vec![grandchild]);
    }

    /// Basic round-trip: a two-entity scene survives `scene_to_json` → `scene_from_json`
    /// with the entity count and the cube translation intact.
    #[test]
    fn basic_round_trip_count_and_cube_position() {
        let reg = registry();
        let mut scene = Scene::new();
        scene.create_entity("Camera");
        let cube = scene.create_entity("Cube");
        scene
            .with_component_mut::<Transform, _>(cube, |t| t.translation = Vec3::new(1.0, 2.0, 3.0))
            .unwrap();

        let doc = scene.scene_to_json(&reg);
        let mut loaded = Scene::new();
        loaded.scene_from_json(&reg, &doc).unwrap();

        let mut count = 0;
        let mut cube_pos = Vec3::ZERO;
        loaded.for_each::<(&Name, &Transform), _>(|_, (name, transform)| {
            count += 1;
            if name.name == "Cube" {
                cube_pos = transform.translation;
            }
        });
        assert_eq!(count, 2, "two entities round-trip");
        assert!(
            cube_pos.distance(Vec3::new(1.0, 2.0, 3.0)) < 1e-6,
            "cube translation survives"
        );
        assert_eq!(doc.get("version"), Some(&Value::from(SCENE_VERSION)));
    }

    #[test]
    fn vegetation_field_round_trips_and_duplicate_fields_are_rejected() {
        let reg = registry();
        let mut scene = Scene::new();
        let field_owner = scene.create_entity("Vegetation");
        scene
            .add_component(
                field_owner,
                VegetationField {
                    map: Uuid(91),
                    enabled: false,
                },
            )
            .unwrap();

        let doc = scene.scene_to_json(&reg);
        let mut loaded = Scene::new();
        loaded.scene_from_json(&reg, &doc).unwrap();
        let loaded_owner = find_by_name(&mut loaded, "Vegetation");
        assert_eq!(
            loaded.component::<VegetationField>(loaded_owner).unwrap(),
            VegetationField {
                map: Uuid(91),
                enabled: false,
            }
        );

        let mut duplicate = doc;
        let entities = duplicate["entities"].as_array_mut().unwrap();
        let mut second = entities[0].clone();
        second["id"] = Value::String("92".to_owned());
        second["components"]["Name"]["name"] = Value::String("Duplicate".to_owned());
        entities.push(second);
        let mut rejected = Scene::new();
        assert!(matches!(
            rejected.scene_from_json(&reg, &duplicate),
            Err(crate::Error::Deserialize(message)) if message.contains("VegetationField")
        ));
    }

    /// Hierarchy round-trip: the durable parent uuid survives, the post-loop resolve
    /// rebuilds parent-handle/children caches, and the authored component order survives.
    #[test]
    fn hierarchy_round_trip_resolves_and_orders() {
        let reg = registry();
        let mut tree = Scene::new();
        let root = tree.create_entity("Root");
        let leaf = tree.create_entity("Leaf");
        tree.with_component_mut::<Transform, _>(root, |t| t.translation = Vec3::new(5.0, 0.0, 0.0))
            .unwrap();
        tree.set_parent(leaf, Some(root), false).unwrap();
        reg.set_component_order(
            &mut tree,
            root,
            vec!["Transform".to_string(), "Name".to_string()],
        )
        .unwrap();

        let doc = tree.scene_to_json(&reg);
        let entities = doc.get("entities").and_then(Value::as_array).unwrap();
        assert!(
            entities
                .iter()
                .all(|e| e.get("componentOrder").is_some_and(Value::is_array)),
            "scene json carries component order"
        );

        let mut loaded = Scene::new();
        loaded.scene_from_json(&reg, &doc).unwrap();
        let loaded_root = find_by_name(&mut loaded, "Root");
        let loaded_leaf = find_by_name(&mut loaded, "Leaf");

        assert_eq!(
            reg.component_order(&mut loaded, loaded_root),
            vec!["Transform".to_string(), "Name".to_string()],
            "component order survives the round trip"
        );
        let parent_handle = loaded
            .with_component::<Relationship, _>(loaded_leaf, |r| r.parent_handle)
            .unwrap();
        assert_eq!(
            parent_handle,
            Some(loaded_root),
            "loaded leaf resolves its parent handle"
        );
        let children = loaded
            .with_component::<Relationship, _>(loaded_root, |r| r.children.clone())
            .unwrap();
        assert!(
            children.contains(&loaded_leaf),
            "loaded root lists the leaf as a child"
        );
    }

    /// A child entry preceding its parent in the array still resolves (post-loop relink).
    #[test]
    fn reversed_array_child_before_parent_resolves() {
        let reg = registry();
        let mut tree = Scene::new();
        let root = tree.create_entity("Root");
        let leaf = tree.create_entity("Leaf");
        tree.set_parent(leaf, Some(root), false).unwrap();

        let mut doc = tree.scene_to_json(&reg);
        // Reverse the entities array so the leaf precedes the root.
        if let Some(Value::Array(entities)) = doc.get_mut("entities") {
            entities.reverse();
        }

        let mut loaded = Scene::new();
        loaded.scene_from_json(&reg, &doc).unwrap();
        let loaded_root = find_by_name(&mut loaded, "Root");
        let loaded_leaf = find_by_name(&mut loaded, "Leaf");
        let parent_handle = loaded
            .with_component::<Relationship, _>(loaded_leaf, |r| r.parent_handle)
            .unwrap();
        assert_eq!(
            parent_handle,
            Some(loaded_root),
            "child-before-parent order still resolves"
        );
    }

    /// A v2 document has no `Relationship` key and no `componentOrder`: every entity
    /// migrates to a root, and the canonical component order is derived.
    #[test]
    fn v2_migration_roots_and_derived_order() {
        let reg = registry();
        let mut tree = Scene::new();
        let root = tree.create_entity("Root");
        let leaf = tree.create_entity("Leaf");
        tree.set_parent(leaf, Some(root), false).unwrap();

        let mut doc = tree.scene_to_json(&reg);
        // Downgrade to v2: drop Relationship from every entity's components and remove the
        // per-entity componentOrder (pre-v4).
        doc.as_object_mut()
            .unwrap()
            .insert("version".to_string(), Value::from(2));
        if let Some(Value::Array(entities)) = doc.get_mut("entities") {
            for entry in entities.iter_mut() {
                if let Some(components) = entry.get_mut("components").and_then(Value::as_object_mut)
                {
                    components.remove("Relationship");
                }
                entry.as_object_mut().unwrap().remove("componentOrder");
            }
        }

        let mut migrated = Scene::new();
        migrated.scene_from_json(&reg, &doc).unwrap();

        let mut total = 0;
        let mut roots = 0;
        migrated.for_each::<&Relationship, _>(|_, rel| {
            total += 1;
            if rel.parent == Uuid(0) && rel.parent_handle.is_none() {
                roots += 1;
            }
        });
        assert_eq!((total, roots), (2, 2), "v2 entities migrate to roots");

        let migrated_root = find_by_name(&mut migrated, "Root");
        assert_eq!(
            reg.component_order(&mut migrated, migrated_root),
            vec!["Name".to_string(), "Transform".to_string()],
            "pre-v4 scene derives the canonical component order"
        );
    }

    /// A skinned mesh round-trips by uuid: the mesh id, two bones, and two inverse-bind
    /// matrices survive (values to 1e-6), and the relink rebuilds `bone_handles` to the
    /// live joints.
    #[test]
    fn skinned_rig_round_trip_resolves_joints() {
        let reg = registry();
        let mut rig = Scene::new();
        let bone_a = rig.create_entity("BoneA");
        let bone_b = rig.create_entity("BoneB");
        rig.set_parent(bone_b, Some(bone_a), false).unwrap();
        let skinned = rig.create_entity("Skinned");
        rig.add_component(
            skinned,
            SkinnedMesh {
                mesh: Uuid(777),
                root_bone: id_of(&rig, bone_a),
                bones: vec![id_of(&rig, bone_a), id_of(&rig, bone_b)],
                inverse_bind: vec![
                    Mat4::from_translation(Vec3::new(-1.0, 0.0, 0.0)),
                    Mat4::from_translation(Vec3::new(0.0, -2.0, 0.0)),
                ],
                ..SkinnedMesh::default()
            },
        )
        .unwrap();
        rig.relink_hierarchy();

        let doc = rig.scene_to_json(&reg);
        let mut loaded = Scene::new();
        loaded.scene_from_json(&reg, &doc).unwrap();

        let loaded_skinned = find_by_name(&mut loaded, "Skinned");
        let loaded_bone_b = find_by_name(&mut loaded, "BoneB");
        let skin = loaded
            .with_component::<SkinnedMesh, _>(loaded_skinned, Clone::clone)
            .unwrap();
        assert_eq!(skin.mesh, Uuid(777), "mesh id survives");
        assert_eq!(skin.bones.len(), 2, "bone count survives");
        assert_eq!(skin.inverse_bind.len(), 2, "inverse-bind count survives");
        // inverseBind[1] is translate(0, -2, 0): its w-axis y is -2.
        assert!(
            (skin.inverse_bind[1].w_axis.y + 2.0).abs() < 1e-6,
            "inverse-bind matrix values survive to 1e-6"
        );
        assert_eq!(skin.bone_handles.len(), 2, "bone handles resolved");
        assert_eq!(
            skin.bone_handles[1], loaded_bone_b,
            "second joint resolves to the live handle"
        );
    }

    /// A dangling parent uuid downgrades to root (with a warning), never a crash.
    #[test]
    fn dangling_parent_downgrades_to_root() {
        let reg = registry();
        let mut tree = Scene::new();
        let root = tree.create_entity("Root");
        let leaf = tree.create_entity("Leaf");
        tree.set_parent(leaf, Some(root), false).unwrap();

        let mut doc = tree.scene_to_json(&reg);
        // Point every non-root parent at a nonexistent uuid.
        if let Some(Value::Array(entities)) = doc.get_mut("entities") {
            for entry in entities.iter_mut() {
                if let Some(rel) = entry
                    .get_mut("components")
                    .and_then(|c| c.get_mut("Relationship"))
                    .and_then(Value::as_object_mut)
                    && rel.get("parent").and_then(Value::as_str) != Some("0")
                {
                    rel.insert("parent".to_string(), Value::String("424242".to_string()));
                }
            }
        }

        let mut orphaned = Scene::new();
        orphaned.scene_from_json(&reg, &doc).unwrap();
        let loaded_leaf = find_by_name(&mut orphaned, "Leaf");
        let rel = orphaned
            .with_component::<Relationship, _>(loaded_leaf, Clone::clone)
            .unwrap();
        assert_eq!(rel.parent, Uuid(0), "dangling parent resolves to root");
        assert!(rel.parent_handle.is_none());
    }

    /// `scene_from_json` rejects an out-of-range version and a malformed document.
    #[test]
    fn rejects_bad_version_and_malformed_document() {
        let reg = registry();
        let mut scene = Scene::new();

        let doc = serde_json::json!({ "version": 0, "entities": [] });
        assert!(matches!(
            scene.scene_from_json(&reg, &doc),
            Err(crate::Error::UnsupportedVersion(0))
        ));
        let doc = serde_json::json!({ "version": SCENE_VERSION + 1, "entities": [] });
        assert!(matches!(
            scene.scene_from_json(&reg, &doc),
            Err(crate::Error::UnsupportedVersion(_))
        ));
        let doc = serde_json::json!({ "version": SCENE_VERSION });
        assert!(matches!(
            scene.scene_from_json(&reg, &doc),
            Err(crate::Error::Document(_))
        ));
        assert!(matches!(
            scene.scene_from_json(&reg, &serde_json::json!([1, 2, 3])),
            Err(crate::Error::Document(_))
        ));
        let doc = serde_json::json!({
            "version": SCENE_VERSION,
            "entities": [{ "components": {} }],
        });
        assert!(matches!(
            scene.scene_from_json(&reg, &doc),
            Err(crate::Error::Document(_))
        ));
    }

    /// `write_scene` → `read_scene` to a tmp path confirms disk fidelity, including the
    /// environment block and an unregistered runtime-only component being skipped.
    #[test]
    fn write_then_read_disk_round_trip() {
        let reg = registry();
        let mut scene = Scene::new();
        let cube = scene.create_entity("Cube");
        scene
            .with_component_mut::<Transform, _>(cube, |t| t.translation = Vec3::new(4.0, 5.0, 6.0))
            .unwrap();
        scene.add_component(cube, Camera::default()).unwrap();
        // A runtime-only component must not serialize, so it must not break the round trip.
        scene
            .add_component(cube, WorldTransform::default())
            .unwrap();
        scene.environment.exposure = 2.5;

        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "saffron_scene_doc_test_{}.json",
            std::process::id()
        ));
        scene.write_scene(&reg, &path).unwrap();

        let mut loaded = Scene::new();
        loaded.read_scene(&reg, &path).unwrap();
        let _ = std::fs::remove_file(&path);

        let loaded_cube = find_by_name(&mut loaded, "Cube");
        assert!(
            loaded
                .component::<Transform>(loaded_cube)
                .unwrap()
                .translation
                .distance(Vec3::new(4.0, 5.0, 6.0))
                < 1e-6
        );
        assert!(loaded.has_component::<Camera>(loaded_cube));
        assert!(
            (loaded.environment.exposure - 2.5).abs() < 1e-6,
            "environment survives the disk round trip"
        );
    }

    /// Byte-equality against a hand-assembled full document — version, environment, and
    /// the per-entity `id` / `components` / `componentOrder`. This pins the whole-document
    /// shape (key order, decimal-string ids, the environment block) to the frozen
    /// `project.json` scene block.
    #[test]
    fn document_bytes_match_captured_block() {
        let reg = registry();
        let mut scene = Scene::new();
        // One deterministic entity: id 1024, default Name + Transform + root Relationship.
        let e = scene.spawn_with_id(Uuid(1024));
        scene
            .add_component(
                e,
                Name {
                    name: "Cube".to_string(),
                },
            )
            .unwrap();
        scene.add_component(e, Transform::default()).unwrap();
        scene.add_component(e, Relationship::default()).unwrap();
        reg.sort_component_order(&mut scene, e);

        // Alphabetical keys, decimal-string id, default environment and components.
        const EXPECT: &str = concat!(
            r#"{"entities":[{"componentOrder":["Name","Transform"],"components":{"#,
            r#""Name":{"name":"Cube"},"#,
            r#""Relationship":{"parent":"0"},"#,
            r#""Transform":{"rotation":{"x":0.0,"y":0.0,"z":0.0},"scale":{"x":1.0,"y":1.0,"z":1.0},"translation":{"x":0.0,"y":0.0,"z":0.0}}"#,
            r#"},"id":"1024"}],"#,
            r#""environment":{"ambientColor":{"x":1.0,"y":1.0,"z":1.0},"ambientIntensity":0.15000000596046448,"#,
            r#""atmosphere":{"atmosphereHeight":100.0,"enabled":false,"mieAnisotropy":0.800000011920929,"#,
            r#""mieScaleHeight":1.2000000476837158,"mieScattering":3.996000051498413,"#,
            r#""moonDiskAngularRadius":0.0049600000493228436,"moonDiskIntensity":1.0,"moonEarthshine":0.019999999552965164,"#,
            r#""ozoneAbsorption":{"x":0.6499999761581421,"y":1.88100004196167,"z":0.08500000089406967},"#,
            r#""perPixelTransmittance":false,"planetRadius":6360.0,"rayleighScaleHeight":8.0,"#,
            r#""rayleighScattering":{"x":5.802000045776367,"y":13.557999610900879,"z":33.099998474121094},"#,
            r#""skyCaptureCadence":9.0,"sunDiskAngularRadius":0.004650000017136335,"sunDiskIntensity":1.0},"#,
            r#""clearColor":{"x":0.05000000074505806,"y":0.05999999865889549,"z":0.07999999821186066},"#,
            r#""cloud":{"anvilBias":0.0,"baseScale":0.00007999999797903001,"castCloudShadows":true,"cloudShadowOnSurfaceStrength":1.0,"cloudShadowStrength":1.0,"cloudType":0.4000000059604645,"coverage":0.5,"curlStrength":120.0,"detailScale":0.0010000000474974513,"detailStrength":0.3499999940395355,"dropletDiameter":20.0,"enabled":false,"layerAltitude":1500.0,"layerHeight":2500.0,"lightSteps":6,"precipitation":0.0,"primarySteps":64,"temporalFactor":0.10000000149011612,"weatherOffset":{"x":0.0,"y":0.0,"z":0.0},"weatherScale":0.000019999999494757503,"weatherTexture":"0"},"#,
            r#""exposure":1.0,"#,
            r#""fog":{"aerialIntensity":1.0,"aerialPerspective":false,"albedo":{"x":0.5,"y":0.6000000238418579,"z":0.699999988079071},"baseDensity":0.019999999552965164,"density":0.019999999552965164,"directionalColor":{"x":1.0,"y":0.8999999761581421,"z":0.699999988079071},"directionalExponent":8.0,"emissive":{"x":0.0,"y":0.0,"z":0.0},"enabled":false,"height":0.0,"heightFalloff":0.20000000298023224,"historyBlend":0.05000000074505806,"layer2Density":0.0,"layer2Falloff":0.5,"layer2Height":0.0,"lightClamp":0.0,"maxOpacity":1.0,"mode":"analytic","neighborhoodClamp":false,"phaseG":0.6000000238418579,"quality":"medium","scatterAlbedo":0.8999999761581421,"startDistance":0.0},"#,
            r#""skyIntensity":1.0,"skyMode":"procedural","skyRotation":0.0,"skyTexture":"0","#,
            r#""timeOfDay":{"cloudTypeCurve":[],"coverageCurve":[],"day":21,"dayLengthSeconds":600.0,"enabled":false,"exposureCurve":[],"latitude":0.0,"longitude":0.0,"manualOverride":false,"month":6,"timeOfDay":0.5,"tintCurve":{"blue":[],"green":[],"master":[],"red":[]},"year":2025},"#,
            r#""useSkyForAmbient":true,"visible":true,"wind":{"gust":0.25,"gustFrequency":0.15000000596046448,"heightExponent":0.20000000298023224,"orientation":0.0,"referenceHeight":10.0,"seed":0,"speed":10.0,"turbulenceOctaves":3,"turbulenceRoughness":0.550000011920929}},"#,
            r#""version":4}"#
        );

        let doc = scene.scene_to_json(&reg);
        assert_eq!(dump_json_sorted(&doc, -1), EXPECT);

        // The captured block re-parses and re-serializes byte-stable through the reader.
        let parsed: Value = serde_json::from_str(EXPECT).unwrap();
        let mut reloaded = Scene::new();
        reloaded.scene_from_json(&reg, &parsed).unwrap();
        assert_eq!(dump_json_sorted(&reloaded.scene_to_json(&reg), -1), EXPECT);
    }
}
