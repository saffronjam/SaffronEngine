//! Scene spawning: a `.smodel` container's metadata reconstructed into scene entities.
//!
//! [`AssetServer::instantiate_model`] is the public entry; it rebuilds a
//! [`ModelSpawnInput`] from a container's META (mesh + material table, the node forest,
//! the skin descriptor, the animation clip ids) and hands it to [`spawn_model`], which
//! dispatches to [`spawn_skinned_model`] for a rigged glTF.
//!
//! Spawned components hold **soft references** — sub-ids resolved at draw time by the
//! loaders, never live `Arc` handles — so a spawned entity serializes cleanly into
//! `project.json` and re-resolves on load. The META quaternion is stored glTF
//! `w,x,y,z`; [`imported_nodes_from_json`] reorders it to glam's `xyzw` at the byte
//! boundary, and the per-bone transform takes the engine's ZYX Euler from there.

use saffron_geometry::glam::{Mat4, Quat, Vec3};
use saffron_geometry::{ImportedNode, ImportedSkin};
use saffron_scene::{
    AnimationPlayer, Bone, BonePhysics, BonePhysicsComponent, IdComponent, Joint, MaterialSet,
    MaterialSlot, Mesh, ModelInstance, MorphComponent, Relationship, SkinnedMesh, Transform, Wrap,
    quat_to_euler_zyx,
};
use saffron_scene::{Entity, Scene};

use saffron_core::Uuid;
use saffron_scene::AssetType;
use serde_json::Value;

use crate::error::{Error, Result};

/// The reconstructed spawn input [`spawn_model`] / [`spawn_skinned_model`] consume: the
/// mesh sub-id, the material table, and — for a rigged glTF — the node forest plus the
/// skin descriptor instantiated as bone entities.
///
/// Reconstructed by [`AssetServer::instantiate_model`] from a container's META; it is
/// never an import output (`bake_model` produces a container, not a `ModelSpawnInput`).
/// `materials` become a [`MaterialSet`] whose slots reference the imported `.smat` chunks.
#[derive(Clone, Debug, Default)]
pub struct ModelSpawnInput {
    /// The mesh sub-id (a soft reference resolved at draw time).
    pub mesh: Uuid,
    /// The imported material table — one reference slot per source material.
    pub materials: Vec<MaterialSlot>,
    /// Whether the import carries a skin (gates the skinned spawn path).
    pub has_skin: bool,
    /// The source node forest.
    pub nodes: Vec<ImportedNode>,
    /// The mesh sub-id per node, parallel to [`ModelSpawnInput::nodes`] (`Uuid(0)` when a
    /// node carries no mesh). The node-forest spawn attaches each node's node-local mesh.
    pub node_meshes: Vec<Uuid>,
    /// The skin descriptor (joints, inverse-bind, roots).
    pub skin_desc: ImportedSkin,
    /// The registered animation clip sub-ids (skinned imports).
    pub animations: Vec<Uuid>,
    /// The morph target names (META `morph.targetNames`); empty when the model has no
    /// blend shapes. Seeds the durable [`MorphComponent`] labels.
    pub morph_target_names: Vec<String>,
    /// The authored rest weights (META `morph.restWeights`), parallel to
    /// `morph_target_names`. Seeds the durable [`MorphComponent`] weights (else zeros).
    pub morph_rest_weights: Vec<f32>,
}

/// Decodes the META `nodes` block into the node forest.
///
/// Each record carries a `name`, a `parent` index (`-1` for a root), and the local TRS.
/// The `r` array is glTF `w,x,y,z`; it is reordered to glam's `xyzw`
/// ([`Quat::from_xyzw`]) here, the one place the byte order crosses into engine space.
/// A non-array input or a non-object record decodes to nothing / is skipped.
#[must_use]
pub fn imported_nodes_from_json(nodes: &Value) -> Vec<ImportedNode> {
    let Some(array) = nodes.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(array.len());
    for record in array {
        let Some(record) = record.as_object() else {
            continue;
        };
        let mut node = ImportedNode {
            name: record
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            parent: record
                .get("parent")
                .and_then(Value::as_i64)
                .map_or(-1, |v| v as i32),
            ..ImportedNode::default()
        };
        if let Some(t) = vec3_from(record.get("t")) {
            node.translation = t;
        }
        if let Some(r) = record.get("r").and_then(Value::as_array)
            && r.len() == 4
        {
            let w = f32_at(r, 0);
            let x = f32_at(r, 1);
            let y = f32_at(r, 2);
            let z = f32_at(r, 3);
            node.rotation = Quat::from_xyzw(x, y, z, w);
        }
        if let Some(s) = vec3_from(record.get("s")) {
            node.scale = s;
        }
        out.push(node);
    }
    out
}

/// Decodes the per-node `mesh` sub-id from the META `nodes` block, parallel to
/// [`imported_nodes_from_json`]. A missing / `"0"` field decodes to `Uuid(0)` (no mesh).
#[must_use]
pub fn node_mesh_ids_from_json(nodes: &Value) -> Vec<Uuid> {
    let Some(array) = nodes.as_array() else {
        return Vec::new();
    };
    array
        .iter()
        .map(|record| {
            record
                .get("mesh")
                .and_then(decimal_u64)
                .map(Uuid)
                .unwrap_or(Uuid(0))
        })
        .collect()
}

/// Decodes the META `skin` block into the skin descriptor.
///
/// `inverseBind` matrices are 16 floats each, column-major (the glam layout), read back
/// straight into a [`Mat4`]; a malformed matrix decodes to identity. A non-object input
/// decodes to a default (empty) descriptor.
#[must_use]
pub fn imported_skin_from_json(skin: &Value) -> ImportedSkin {
    let Some(skin) = skin.as_object() else {
        return ImportedSkin::default();
    };
    let mut out = ImportedSkin::default();
    if let Some(joints) = skin.get("joints").and_then(Value::as_array) {
        out.joints = joints
            .iter()
            .map(|j| j.as_i64().map_or(-1, |v| v as i32))
            .collect();
    }
    out.skeleton_root = skin
        .get("skeletonRoot")
        .and_then(Value::as_i64)
        .map_or(-1, |v| v as i32);
    out.mesh_node = skin
        .get("meshNode")
        .and_then(Value::as_i64)
        .map_or(-1, |v| v as i32);
    if let Some(matrices) = skin.get("inverseBind").and_then(Value::as_array) {
        for flat in matrices {
            let matrix =
                flat.as_array()
                    .filter(|cols| cols.len() == 16)
                    .map_or(Mat4::IDENTITY, |cols| {
                        let mut data = [0.0f32; 16];
                        for (i, slot) in data.iter_mut().enumerate() {
                            *slot = f32_at(cols, i);
                        }
                        Mat4::from_cols_array(&data)
                    });
            out.inverse_bind.push(matrix);
        }
    }
    out
}

/// Reads a 3-element float array into a [`Vec3`], or `None` if it is missing / mis-shaped.
fn vec3_from(value: Option<&Value>) -> Option<Vec3> {
    let array = value?.as_array()?;
    if array.len() != 3 {
        return None;
    }
    Some(Vec3::new(
        f32_at(array, 0),
        f32_at(array, 1),
        f32_at(array, 2),
    ))
}

/// The `i`-th array element as an `f32` (`0.0` if absent or non-numeric).
fn f32_at(array: &[Value], i: usize) -> f32 {
    array.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32
}

/// Attaches the spawn input's material table to `entity` as a [`MaterialSet`] — one slot
/// per source material, each *referencing* the imported `.smat` chunk (no inline copy). An
/// empty table leaves a single default-material slot so the entity always resolves a
/// material.
fn apply_imported_materials(scene: &mut Scene, entity: Entity, input: &ModelSpawnInput) {
    let slots = if input.materials.is_empty() {
        vec![MaterialSlot::default()]
    } else {
        input.materials.clone()
    };
    let _ = scene.add_component(entity, MaterialSet { slots });
}

/// Seeds the durable [`MorphComponent`] on a mesh-bearing entity when the import carries
/// morph targets: weights from the authored rest weights (else zeros), names from META.
/// Import-managed, so it is non-addable / non-removable in the editor.
fn seed_morph(scene: &mut Scene, entity: Entity, input: &ModelSpawnInput) {
    let count = input.morph_target_names.len();
    if count == 0 {
        return;
    }
    let weights = if input.morph_rest_weights.len() == count {
        input.morph_rest_weights.clone()
    } else {
        vec![0.0; count]
    };
    let _ = scene.add_component(
        entity,
        MorphComponent {
            weights,
            names: input.morph_target_names.clone(),
        },
    );
}

/// The stable id of `entity` (its [`IdComponent`]); `Uuid(0)` if it carries none.
fn entity_uuid(scene: &Scene, entity: Entity) -> Uuid {
    scene
        .with_component::<IdComponent, _>(entity, |id| id.id)
        .unwrap_or(Uuid(0))
}

/// Spawns an unrigged model: one entity carrying the mesh + material table.
fn spawn_unskinned(scene: &mut Scene, name: String, input: &ModelSpawnInput) -> Entity {
    let entity = scene.create_entity(name);
    let _ = scene.add_component(entity, Mesh { mesh: input.mesh });
    apply_imported_materials(scene, entity, input);
    seed_morph(scene, entity, input);
    entity
}

/// Instantiates a rigged import: one entity per glTF node (local TRS, parented by uuid),
/// [`Bone`] tags on the joints, and a [`SkinnedMesh`] on the mesh node listing the joints
/// in glTF order, all wrapped under one identity container root. Returns the container
/// root.
fn spawn_skinned_model(scene: &mut Scene, name: String, input: &ModelSpawnInput) -> Entity {
    let mut node_entities: Vec<Entity> = Vec::with_capacity(input.nodes.len());
    let mut node_uuids: Vec<Uuid> = Vec::with_capacity(input.nodes.len());
    for node in &input.nodes {
        let entity = scene.create_entity(node.name.clone());
        let _ = scene.with_component_mut::<Transform, _>(entity, |transform| {
            transform.translation = node.translation;
            transform.rotation = quat_to_euler_zyx(node.rotation);
            transform.scale = node.scale;
        });
        node_uuids.push(entity_uuid(scene, entity));
        node_entities.push(entity);
    }
    for (i, node) in input.nodes.iter().enumerate() {
        let parent = node.parent;
        if parent >= 0 && (parent as usize) < node_uuids.len() {
            let parent_uuid = node_uuids[parent as usize];
            let _ = scene.with_component_mut::<Relationship, _>(node_entities[i], |rel| {
                rel.parent = parent_uuid
            });
        }
    }

    let mut bones: Vec<Uuid> = Vec::with_capacity(input.skin_desc.joints.len());
    for &joint in &input.skin_desc.joints {
        if joint < 0 || (joint as usize) >= node_entities.len() {
            bones.push(Uuid(0));
            continue;
        }
        let bone = node_entities[joint as usize];
        if !scene.has_component::<Bone>(bone) {
            let _ = scene.add_component(bone, Bone::default());
        }
        bones.push(node_uuids[joint as usize]);
    }

    let mesh_node = input.skin_desc.mesh_node;
    let (mesh_entity, mesh_node_owned) =
        if mesh_node >= 0 && (mesh_node as usize) < node_entities.len() {
            (node_entities[mesh_node as usize], true)
        } else {
            (scene.create_entity("Mesh"), false)
        };

    let root = input.skin_desc.skeleton_root;
    let root_bone = if root >= 0 && (root as usize) < node_uuids.len() {
        node_uuids[root as usize]
    } else {
        bones.first().copied().unwrap_or(Uuid(0))
    };
    let _ = scene.add_component(
        mesh_entity,
        SkinnedMesh {
            mesh: input.mesh,
            root_bone,
            bones,
            inverse_bind: input.skin_desc.inverse_bind.clone(),
            bone_handles: Vec::new(),
        },
    );
    apply_imported_materials(scene, mesh_entity, input);
    seed_morph(scene, mesh_entity, input);

    if let Some(&clip) = input.animations.first() {
        let _ = scene.add_component(
            mesh_entity,
            AnimationPlayer {
                clip,
                playing: false,
                wrap: Wrap::Loop,
                ..AnimationPlayer::default()
            },
        );
    }

    let container = scene.create_entity(name);
    let container_uuid = entity_uuid(scene, container);
    for &node in &node_entities {
        let _ = scene.with_component_mut::<Relationship, _>(node, |rel| {
            if rel.parent.value() == 0 {
                rel.parent = container_uuid;
            }
        });
    }
    if !mesh_node_owned {
        let _ = scene
            .with_component_mut::<Relationship, _>(mesh_entity, |rel| rel.parent = container_uuid);
    }

    scene.relink_hierarchy();
    autofit_bone_physics(scene, mesh_entity);
    container
}

/// Auto-fits a per-bone capsule into a [`BonePhysicsComponent`] from the rest skeleton so
/// a freshly imported rig is ragdoll-ready: the half-height spans toward the child joint,
/// the radius is a fraction of it.
fn autofit_bone_physics(scene: &mut Scene, mesh_entity: Entity) {
    let handles = scene
        .with_component::<SkinnedMesh, _>(mesh_entity, |skin| skin.bone_handles.clone())
        .unwrap_or_default();
    if handles.is_empty() {
        return;
    }
    scene.update_world_transforms();
    let count = handles.len();
    let mut rest_pos = vec![Vec3::ZERO; count];
    let mut joint_uuid = vec![0u64; count];
    for (i, &joint) in handles.iter().enumerate() {
        if scene.valid(joint) {
            rest_pos[i] = scene.world_translation(joint);
            joint_uuid[i] = entity_uuid(scene, joint).value();
        }
    }
    let mut child_parent = vec![0u64; count];
    for (child, &joint) in handles.iter().enumerate() {
        if scene.valid(joint) {
            child_parent[child] = scene
                .with_component::<Relationship, _>(joint, |rel| rel.parent.value())
                .unwrap_or(0);
        }
    }

    let mut phys = BonePhysicsComponent {
        bones: vec![BonePhysics::default(); count],
    };
    for i in 0..count {
        let mut length = 0.0f32;
        for child in 0..count {
            if child == i || joint_uuid[i] == 0 {
                continue;
            }
            if child_parent[child] == joint_uuid[i] {
                length = length.max((rest_pos[child] - rest_pos[i]).length());
            }
        }
        let half_height = if length > 0.001 { length * 0.5 } else { 0.05 };
        let radius = (half_height * 0.3).max(0.03);
        phys.bones[i].shape_half_extents = Vec3::new(radius, half_height, radius);
        phys.bones[i].mass = 1.0;
        phys.bones[i].joint = Joint::SwingTwist;
    }
    let _ = scene.add_component(mesh_entity, phys);
}

/// Whether the forest is a single identity-transform root — the only shape that
/// collapses to one entity (a non-identity, multi-node, or *animated* forest keeps its live
/// local transforms + its container, which a node-TRS or morph-weights track drives). An
/// animated single node never collapses: the clip needs an [`AnimationPlayer`] on a container
/// root, and a collapsed entity has no player (e.g. the glTF `SimpleMorph` — one identity node
/// with a morph-weights clip — would otherwise lose its animation entirely).
fn is_single_identity_root(input: &ModelSpawnInput) -> bool {
    input.animations.is_empty() && input.nodes.len() == 1 && {
        let n = &input.nodes[0];
        n.parent < 0
            && n.translation == Vec3::ZERO
            && n.rotation == Quat::IDENTITY
            && n.scale == Vec3::new(1.0, 1.0, 1.0)
    }
}

/// Instantiates an unskinned node forest: one entity per node (local TRS, parented by
/// uuid), the node-local mesh + material table on each mesh-bearing node, all under one
/// container root that holds the single [`AnimationPlayer`]. Returns the container root.
fn spawn_node_forest(scene: &mut Scene, name: String, input: &ModelSpawnInput) -> Entity {
    let mut node_entities: Vec<Entity> = Vec::with_capacity(input.nodes.len());
    let mut node_uuids: Vec<Uuid> = Vec::with_capacity(input.nodes.len());
    for node in &input.nodes {
        let entity = scene.create_entity(node.name.clone());
        let _ = scene.with_component_mut::<Transform, _>(entity, |transform| {
            transform.translation = node.translation;
            transform.rotation = quat_to_euler_zyx(node.rotation);
            transform.scale = node.scale;
        });
        node_uuids.push(entity_uuid(scene, entity));
        node_entities.push(entity);
    }
    for (i, node) in input.nodes.iter().enumerate() {
        let parent = node.parent;
        if parent >= 0 && (parent as usize) < node_uuids.len() {
            let parent_uuid = node_uuids[parent as usize];
            let _ = scene.with_component_mut::<Relationship, _>(node_entities[i], |rel| {
                rel.parent = parent_uuid
            });
        }
    }

    for (i, &mesh_id) in input.node_meshes.iter().enumerate() {
        if mesh_id.value() == 0 || i >= node_entities.len() {
            continue;
        }
        let _ = scene.add_component(node_entities[i], Mesh { mesh: mesh_id });
        apply_imported_materials(scene, node_entities[i], input);
    }
    // The mesh-global morph rides the first mesh-bearing node.
    if let Some(i) = input
        .node_meshes
        .iter()
        .position(|id| id.value() != 0)
        .filter(|&i| i < node_entities.len())
    {
        seed_morph(scene, node_entities[i], input);
    }

    let container = scene.create_entity(name);
    let container_uuid = entity_uuid(scene, container);
    for &node in &node_entities {
        let _ = scene.with_component_mut::<Relationship, _>(node, |rel| {
            if rel.parent.value() == 0 {
                rel.parent = container_uuid;
            }
        });
    }
    if let Some(&clip) = input.animations.first() {
        let _ = scene.add_component(
            container,
            AnimationPlayer {
                clip,
                playing: false,
                wrap: Wrap::Loop,
                ..AnimationPlayer::default()
            },
        );
    }

    scene.relink_hierarchy();
    container
}

/// Spawns a model, dispatching on shape: a skin spawns the rigged path; a single
/// identity root collapses to one entity; any other forest spawns a live entity forest.
/// Returns the root entity.
pub fn spawn_model(scene: &mut Scene, name: impl Into<String>, input: &ModelSpawnInput) -> Entity {
    let name = name.into();
    if input.has_skin {
        return spawn_skinned_model(scene, name, input);
    }
    if is_single_identity_root(input) {
        return spawn_unskinned(scene, name, input);
    }
    spawn_node_forest(scene, name, input)
}

impl crate::AssetServer {
    /// Expands a `.smodel` container into the scene, reconstructing the spawn input from
    /// its META (mesh/material/animation sub-ids, the node forest, the skin) and reusing
    /// [`spawn_model`].
    ///
    /// Spawned components hold **soft references** — sub-ids resolved at draw time
    /// through the container — so reimport/extract changes flow through and a spawned
    /// entity serializes cleanly. The root is tagged [`ModelInstance`] so the editor
    /// treats the placed model as a unit. No GPU upload; one asset instantiates into
    /// many independent entity trees.
    ///
    /// The per-material base color / metallic / roughness come from the META `materials`
    /// block (the import-written flat factors), not a `.smat` resolve.
    ///
    /// # Errors
    ///
    /// [`Error::NotInCatalog`] if `model_id` is not a loadable container.
    pub fn instantiate_model(
        &mut self,
        scene: &mut Scene,
        model_id: Uuid,
        name: impl Into<String>,
    ) -> Result<Entity> {
        let model = self
            .load_model_asset(model_id)
            .ok_or(Error::NotInCatalog(model_id.value()))?;
        let meta = &model.meta;

        let mut input = ModelSpawnInput::default();

        // Each baked material sub-asset is catalog-addressable by its `sub_id` (a row with
        // `container == model_id`), so a slot *references* the `.smat` chunk directly — the
        // resolve path loads factors + textures from it at draw time. Editing that material
        // then propagates to every instance.
        for sub in &meta.sub_assets {
            if sub.asset_type == AssetType::Material {
                input.materials.push(MaterialSlot {
                    material: sub.sub_id,
                    ..MaterialSlot::default()
                });
            }
        }

        for sub in &meta.sub_assets {
            if sub.asset_type == AssetType::Animation {
                input.animations.push(sub.sub_id);
            }
        }

        input.nodes = imported_nodes_from_json(&meta.nodes);
        input.node_meshes = node_mesh_ids_from_json(&meta.nodes);
        if !meta.skin.is_null() {
            input.skin_desc = imported_skin_from_json(&meta.skin);
            input.has_skin = !input.skin_desc.joints.is_empty();
        }
        if let Some(morph) = meta.morph.as_object() {
            if let Some(names) = morph.get("targetNames").and_then(Value::as_array) {
                input.morph_target_names = names
                    .iter()
                    .filter_map(|n| n.as_str().map(str::to_owned))
                    .collect();
            }
            if let Some(rest) = morph.get("restWeights").and_then(Value::as_array) {
                input.morph_rest_weights = rest
                    .iter()
                    .map(|w| w.as_f64().unwrap_or(0.0) as f32)
                    .collect();
            }
        }
        // The mesh the collapse / skinned path places: the skin's mesh node when rigged,
        // else the first mesh-bearing node. The forest path reads `node_meshes` per node.
        input.mesh = if input.has_skin {
            input
                .node_meshes
                .get(input.skin_desc.mesh_node.max(0) as usize)
                .copied()
                .unwrap_or(Uuid(0))
        } else {
            input
                .node_meshes
                .iter()
                .copied()
                .find(|id| id.value() != 0)
                .unwrap_or(Uuid(0))
        };

        let root = spawn_model(scene, name, &input);
        let _ = scene.add_component(root, ModelInstance { model_id });
        Ok(root)
    }
}

/// A uuid encoded the wire way — a decimal string (preferred) or a JSON number.
fn decimal_u64(value: &Value) -> Option<u64> {
    if let Some(s) = value.as_str() {
        return s.parse().ok();
    }
    value.as_u64()
}

#[cfg(test)]
#[path = "spawn_tests.rs"]
mod tests;
