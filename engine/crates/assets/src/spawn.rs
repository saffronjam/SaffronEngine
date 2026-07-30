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

/// The reconstructed spawn input [`spawn_model`] consumes: the mesh sub-id, the material table,
/// and — for a rigged glTF — the node forest plus the skin descriptor instantiated as bone entities.
///
/// Reconstructed by [`AssetServer::instantiate_model`](crate::AssetServer::instantiate_model) from
/// a container's META; it is
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
mod tests {
    use crate::error::Error;
    use crate::{AssetServer, ImportOptions};
    use saffron_core::Uuid;
    use saffron_geometry::glam::{Mat4, Quat, Vec3};
    use saffron_geometry::{
        ImportedMaterial, ImportedModel, ImportedNode, ImportedSkin, Mesh, SkinPayload, Submesh,
        Vertex, VertexSkin,
    };
    use saffron_scene::{
        AnimationPlayer, AssetType, Bone, BonePhysicsComponent, MaterialSet, ModelInstance, Scene,
        SkinnedMesh,
    };
    use std::path::PathBuf;

    /// A unique scratch dir under the system temp, removed and recreated per test.
    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("saffron-assets-spawn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A one-submesh triangle mesh (the minimal renderable shape).
    fn tri_mesh() -> Mesh {
        Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::ZERO,
                    normal: Vec3::Z,
                    uv0: saffron_geometry::glam::Vec2::ZERO,
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::X,
                    normal: Vec3::Z,
                    uv0: saffron_geometry::glam::Vec2::new(1.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::Y,
                    normal: Vec3::Z,
                    uv0: saffron_geometry::glam::Vec2::new(0.0, 1.0),
                    ..Vertex::default()
                },
            ],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        }
    }

    /// A two-submesh quad (slots 0 and 1) so a multi-material spawn produces a `MaterialSet`.
    fn quad_mesh() -> Mesh {
        let mut mesh = tri_mesh();
        mesh.vertices.push(Vertex {
            position: Vec3::ONE,
            normal: Vec3::Z,
            uv0: saffron_geometry::glam::Vec2::ONE,
            ..Vertex::default()
        });
        mesh.indices = vec![0, 1, 2, 0, 2, 3];
        mesh.submeshes = vec![
            Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            },
            Submesh {
                first_index: 3,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 1,
            },
        ];
        mesh
    }

    /// Bakes `graph` into the server's catalog, returning the model id ready to instantiate.
    fn bake_into_catalog(assets: &mut AssetServer, graph: &ImportedModel, source: &str) -> Uuid {
        let bake = assets
            .bake_model(graph, ImportOptions::default(), source, Uuid(0))
            .expect("bake");
        for row in &bake.rows {
            assets.catalog.put(row.clone());
        }
        bake.model_id
    }

    #[test]
    fn instantiate_flat_model_spawns_one_mesh_entity_referencing_its_material() {
        let dir = scratch("flat");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let graph = ImportedModel {
            nodes: vec![ImportedNode {
                name: "mesh".to_owned(),
                mesh: Some(tri_mesh()),
                ..ImportedNode::default()
            }],
            materials: vec![ImportedMaterial {
                name: "flat".to_owned(),
                base_color: saffron_geometry::glam::Vec4::new(0.25, 0.5, 0.75, 1.0),
                metallic: 0.3,
                roughness: 0.4,
                ..ImportedMaterial::default()
            }],
            animations: Vec::new(),
            skin: None,
            morph: None,
            origin: Default::default(),
        };
        let model_id = bake_into_catalog(&mut assets, &graph, "/tmp/flat.obj");

        let mut scene = Scene::new();
        let entity = assets
            .instantiate_model(&mut scene, model_id, "Cube")
            .expect("instantiate");

        // One material -> a `MaterialSet` with a single slot *referencing* the baked `.smat`
        // sub-id (no inline factor copy).
        assert!(scene.has_component::<saffron_scene::Mesh>(entity));
        assert!(scene.has_component::<MaterialSet>(entity));
        assert!(scene.has_component::<ModelInstance>(entity));

        let model = assets.load_model_asset(model_id).unwrap();
        let mesh_id = scene.component::<saffron_scene::Mesh>(entity).unwrap().mesh;
        let baked_mesh = model
            .meta
            .sub_assets
            .iter()
            .find(|s| s.asset_type == AssetType::Mesh)
            .unwrap()
            .sub_id;
        assert_eq!(
            mesh_id, baked_mesh,
            "the spawned mesh id is the baked sub-id"
        );

        let baked_material = model
            .meta
            .sub_assets
            .iter()
            .find(|s| s.asset_type == AssetType::Material)
            .unwrap()
            .sub_id;
        let slots = scene
            .with_component::<MaterialSet, _>(entity, |s| s.slots.clone())
            .expect("a material set");
        assert_eq!(slots.len(), 1, "one slot for the single source material");
        assert_ne!(
            slots[0].material.value(),
            0,
            "the slot references a real id"
        );
        assert_eq!(
            slots[0].material, baked_material,
            "the slot references the baked `.smat` sub-id"
        );

        let instance = scene.component::<ModelInstance>(entity).unwrap();
        assert_eq!(instance.model_id, model_id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An *animated* single identity node must NOT collapse to one entity: the clip needs an
    /// `AnimationPlayer` on a container root, so a collapsed (player-less) entity would lose the
    /// animation. This is the glTF `SimpleMorph` shape — one identity node carrying a morph mesh
    /// with a morph-weights clip.
    #[test]
    fn instantiate_animated_single_morph_node_keeps_its_player() {
        let dir = scratch("morphclip");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let clip = saffron_geometry::AnimClip {
            name: "morph".to_owned(),
            duration: 1.0,
            tracks: vec![saffron_geometry::AnimTrack {
                target: saffron_geometry::AnimTarget::Node,
                index: -1,
                target_name: "morphMesh".to_owned(),
                path: saffron_geometry::AnimPath::Weights,
                morph_count: 1,
                times: vec![0.0, 1.0],
                values: vec![0.0, 1.0],
                ..saffron_geometry::AnimTrack::default()
            }],
        };
        let graph = ImportedModel {
            // One node, identity transform, parent -1 — the exact collapse predicate, but animated.
            nodes: vec![ImportedNode {
                name: "morphMesh".to_owned(),
                mesh: Some(tri_mesh()),
                ..ImportedNode::default()
            }],
            materials: vec![ImportedMaterial {
                name: "m".to_owned(),
                ..ImportedMaterial::default()
            }],
            animations: vec![clip],
            skin: None,
            morph: Some(saffron_geometry::MorphData {
                targets: vec![saffron_geometry::MorphTarget {
                    name: "bulge".to_owned(),
                    rest_weight: 0.0,
                    deltas: vec![saffron_geometry::MorphDelta {
                        vertex_index: 0,
                        d_position: Vec3::new(0.0, 1.0, 0.0),
                        d_normal: Vec3::ZERO,
                    }],
                }],
            }),
            origin: Default::default(),
        };
        let model_id = bake_into_catalog(&mut assets, &graph, "/tmp/morph.gltf");

        let mut scene = Scene::new();
        let container = assets
            .instantiate_model(&mut scene, model_id, "SimpleMorph")
            .expect("instantiate");
        assert!(scene.has_component::<ModelInstance>(container));

        // The clip survived on exactly ONE player (no rival player on the leaf), stopped,
        // autoplay opt-in (off), with the first clip attached.
        let mut players: Vec<AnimationPlayer> = Vec::new();
        scene.for_each::<&AnimationPlayer, _>(|_, p| players.push(*p));
        assert_eq!(
            players.len(),
            1,
            "exactly one AnimationPlayer for the model (no duplicate on the leaf)"
        );
        let player = players[0];
        assert!(!player.playing, "imported clips spawn stopped");
        assert!(!player.autoplay, "autoplay is opt-in (off on import)");
        assert_ne!(player.clip.value(), 0, "the morph clip id is attached");

        // The durable Morph component seeded on the mesh node (its names from the targets).
        let mut morph_names: Option<Vec<String>> = None;
        scene.for_each::<&saffron_scene::MorphComponent, _>(|_, m| {
            morph_names = Some(m.names.clone())
        });
        assert_eq!(
            morph_names.as_deref(),
            Some(["bulge".to_owned()].as_slice()),
            "the morph mesh keeps its MorphComponent + target names",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn instantiate_multi_material_model_spawns_a_material_set_in_slot_order() {
        let dir = scratch("multimat");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let graph = ImportedModel {
            nodes: vec![ImportedNode {
                name: "mesh".to_owned(),
                mesh: Some(quad_mesh()),
                ..ImportedNode::default()
            }],
            materials: vec![
                ImportedMaterial {
                    name: "a".to_owned(),
                    base_color: saffron_geometry::glam::Vec4::new(1.0, 0.0, 0.0, 1.0),
                    ..ImportedMaterial::default()
                },
                ImportedMaterial {
                    name: "b".to_owned(),
                    base_color: saffron_geometry::glam::Vec4::new(0.0, 1.0, 0.0, 1.0),
                    ..ImportedMaterial::default()
                },
            ],
            animations: Vec::new(),
            skin: None,
            morph: None,
            origin: Default::default(),
        };
        let model_id = bake_into_catalog(&mut assets, &graph, "/tmp/two.obj");

        let mut scene = Scene::new();
        let entity = assets
            .instantiate_model(&mut scene, model_id, "Two")
            .expect("instantiate");

        // Two source materials -> two slots, each referencing its baked `.smat` sub-id in the
        // baked (slot) order.
        let material_ids: Vec<Uuid> = assets
            .load_model_asset(model_id)
            .unwrap()
            .meta
            .sub_assets
            .iter()
            .filter(|s| s.asset_type == AssetType::Material)
            .map(|s| s.sub_id)
            .collect();
        assert_eq!(material_ids.len(), 2, "two baked material sub-assets");

        let set = scene
            .with_component::<MaterialSet, _>(entity, |s| s.slots.clone())
            .expect("a material set");
        assert_eq!(set.len(), 2, "two slots, in slot order");
        assert_eq!(
            set[0].material, material_ids[0],
            "slot 0 references material a"
        );
        assert_eq!(
            set[1].material, material_ids[1],
            "slot 1 references material b"
        );
        assert!(
            set.iter().all(|slot| slot.material.value() != 0),
            "both slots reference real sub-ids"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rigged graph: two nodes (root, joint) with a non-identity joint rotation, one joint,
    /// one clip — so the skinned spawn path exercises the node forest, bone tagging, the skin
    /// descriptor, the animation player, and the META quaternion decode.
    fn rigged_graph() -> ImportedModel {
        let clip = saffron_geometry::AnimClip {
            name: "idle".to_owned(),
            duration: 1.0,
            tracks: vec![saffron_geometry::AnimTrack {
                index: 1,
                target_name: "joint".to_owned(),
                ..saffron_geometry::AnimTrack::default()
            }],
        };
        // A 90-degree rotation about Y, stored on the joint node.
        let joint_rotation = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
        ImportedModel {
            origin: Default::default(),
            nodes: vec![
                // The skinned mesh node (mesh_node 0) carries the mesh node-locally.
                ImportedNode {
                    name: "root".to_owned(),
                    mesh: Some(tri_mesh()),
                    ..ImportedNode::default()
                },
                ImportedNode {
                    name: "joint".to_owned(),
                    parent: 0,
                    translation: Vec3::new(0.0, 1.0, 0.0),
                    rotation: joint_rotation,
                    ..ImportedNode::default()
                },
            ],
            materials: vec![ImportedMaterial {
                name: "skin".to_owned(),
                ..ImportedMaterial::default()
            }],
            animations: vec![clip],
            skin: Some(SkinPayload {
                desc: ImportedSkin {
                    joints: vec![1],
                    inverse_bind: vec![Mat4::IDENTITY],
                    skeleton_root: 0,
                    mesh_node: 0,
                },
                stream: vec![VertexSkin::default(); 3],
            }),
            morph: None,
        }
    }

    #[test]
    fn instantiate_skinned_model_spawns_node_forest_bones_and_skin() {
        let dir = scratch("skinned");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let graph = rigged_graph();
        let model_id = bake_into_catalog(&mut assets, &graph, "/tmp/rig.glb");

        let mut scene = Scene::new();
        let container = assets
            .instantiate_model(&mut scene, model_id, "Rig")
            .expect("instantiate");

        // The container root carries ModelInstance.
        assert!(scene.has_component::<ModelInstance>(container));

        // Find the skinned-mesh entity by query.
        let mut skinned: Option<(Uuid, usize, Uuid)> = None;
        scene.for_each::<&SkinnedMesh, _>(|_, skin| {
            skinned = Some((skin.mesh, skin.bones.len(), skin.root_bone));
        });
        let (skin_mesh, bone_count, root_bone) = skinned.expect("a skinned mesh exists");
        assert_eq!(bone_count, 1, "one joint in the skin");
        assert_ne!(skin_mesh.value(), 0, "the skinned mesh has a real sub-id");
        assert_ne!(root_bone.value(), 0, "skeletonRoot resolves to a node uuid");

        // Exactly one bone tag (the single joint).
        let mut bones = 0;
        scene.for_each::<&Bone, _>(|_, _| bones += 1);
        assert_eq!(bones, 1, "the single joint is bone-tagged");

        // The animation player is attached with the first clip, stopped, looping.
        let mut player: Option<AnimationPlayer> = None;
        scene.for_each::<&AnimationPlayer, _>(|_, p| player = Some(*p));
        let player = player.expect("an animation player exists");
        assert!(!player.playing, "imported rigs spawn stopped");
        assert_ne!(player.clip.value(), 0, "the first clip id is attached");

        // Auto-fit ran: a BonePhysicsComponent with one bone entry.
        let mut phys_bones = None;
        scene.for_each::<&BonePhysicsComponent, _>(|_, p| phys_bones = Some(p.bones.len()));
        assert_eq!(phys_bones, Some(1), "auto-fit produced one bone capsule");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn meta_quaternion_decode_reorders_to_glam_xyzw() {
        // The import writes r=[w,x,y,z]; the decode must rebuild glam xyzw.
        let rot = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
        let nodes = serde_json::json!([{
            "name": "j",
            "parent": -1,
            "t": [0.0, 0.0, 0.0],
            "r": [rot.w, rot.x, rot.y, rot.z],
            "s": [1.0, 1.0, 1.0],
        }]);
        let decoded = crate::spawn::imported_nodes_from_json(&nodes);
        assert_eq!(decoded.len(), 1);
        let q = decoded[0].rotation;
        assert!((q.x - rot.x).abs() < 1e-5);
        assert!((q.y - rot.y).abs() < 1e-5);
        assert!((q.z - rot.z).abs() < 1e-5);
        assert!((q.w - rot.w).abs() < 1e-5);
    }

    #[test]
    fn instantiating_twice_yields_stable_soft_references() {
        let dir = scratch("twice");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let graph = rigged_graph();
        let model_id = bake_into_catalog(&mut assets, &graph, "/tmp/rig.glb");

        let mut scene = Scene::new();
        let a = assets
            .instantiate_model(&mut scene, model_id, "Rig A")
            .expect("instantiate a");
        let b = assets
            .instantiate_model(&mut scene, model_id, "Rig B")
            .expect("instantiate b");
        assert_ne!(a, b, "two independent entity trees");

        // Both instances reference the same mesh sub-id (soft references stable).
        let mut meshes: Vec<Uuid> = Vec::new();
        scene.for_each::<&SkinnedMesh, _>(|_, skin| meshes.push(skin.mesh));
        assert_eq!(meshes.len(), 2);
        assert_eq!(
            meshes[0], meshes[1],
            "the same baked sub-id across instances"
        );

        let mut models: Vec<Uuid> = Vec::new();
        scene.for_each::<&ModelInstance, _>(|_, m| models.push(m.model_id));
        assert_eq!(models.len(), 2);
        assert!(models.iter().all(|m| *m == model_id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_skinned_node_decode_recovers_the_joint_local_transform() {
        let skin = serde_json::json!({
            "joints": [1],
            "inverseBind": [[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]],
            "skeletonRoot": 0,
            "meshNode": 0,
        });
        let decoded = crate::spawn::imported_skin_from_json(&skin);
        assert_eq!(decoded.joints, vec![1]);
        assert_eq!(decoded.skeleton_root, 0);
        assert_eq!(decoded.mesh_node, 0);
        assert_eq!(decoded.inverse_bind.len(), 1);
        assert_eq!(decoded.inverse_bind[0], Mat4::IDENTITY);

        // A null / non-object skin decodes to an empty descriptor.
        assert!(
            crate::spawn::imported_skin_from_json(&serde_json::Value::Null)
                .joints
                .is_empty()
        );
    }

    /// `instantiate_model` for an id that is not in the catalog returns `NotInCatalog`.
    #[test]
    fn instantiate_missing_model_errors() {
        let dir = scratch("missing");
        let mut assets = AssetServer::new(dir.join("assets"));
        let mut scene = Scene::new();
        let err = assets
            .instantiate_model(&mut scene, Uuid(424_242), "Nope")
            .unwrap_err();
        assert!(matches!(err, Error::NotInCatalog(424_242)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
