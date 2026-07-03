//! The hierarchy and transform core: local/world matrix composition, the
//! parent-uuid → handle cache rebuild, the per-frame world-transform write, the
//! skinning joint palette, the sanctioned reparent, and the numerically-stable ZYX
//! Euler extraction.
//!
//! This is the pure-CPU math the renderer, animation, and gizmo all sit on top of. Two
//! pieces are hand-rolled rather than delegated to glam because glam's conventions do not
//! match the engine's: the Euler-XYZ → quaternion composition in [`transform_matrix`] and
//! the numerically-stable Rz·Ry·Rx extraction in [`quat_to_euler_zyx`]. Getting either
//! wrong silently corrupts every gizmo-rotate and reparent-rebase that round-trips a
//! quaternion through the `Transform`'s Euler, so both carry dedicated round-trip tests.

use glam::{Mat3, Mat4, Quat, Vec3};

use saffron_core::Uuid;

use crate::component::{
    AnimationPlayer, Camera, IdComponent, Mesh, ModelInstance, MorphComponent, PoseOverride,
    Relationship, SkinnedMesh, Transform, WorldTransform,
};
use crate::error::{Error, Result};
use crate::scene::{Entity, Scene};

/// The resolved primary camera: its view matrix plus projection parameters.
///
/// The projection is left un-flipped; the renderer applies the Vulkan Y-flip where it
/// samples and the editor gizmo consumes it as-is, so there is one source of truth for
/// both. A scene with no primary camera yields `None` from [`Scene::primary_camera`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraView {
    /// The view matrix (inverse of the camera's world matrix).
    pub view: Mat4,
    /// Vertical field of view, degrees.
    pub fov: f32,
    /// Near clip plane.
    pub near_plane: f32,
    /// Far clip plane.
    pub far_plane: f32,
}

/// The local matrix `T · R · S` for a [`Transform`], with `R` built from the Euler-XYZ
/// triple.
///
/// The Euler → quaternion step is hand-rolled rather than delegated to glam's
/// `Quat::from_euler`, whose `EulerRot` conventions do not match the engine's for a generic
/// (non-axis-aligned) rotation. This is the single place the authored Euler becomes a
/// rotation, so the convention here is load-bearing across the whole engine.
#[must_use]
pub fn transform_matrix(transform: &Transform) -> Mat4 {
    Mat4::from_translation(transform.translation)
        * Mat4::from_quat(quat_from_euler_xyz(transform.rotation))
        * Mat4::from_scale(transform.scale)
}

/// A quaternion from an Euler-XYZ triple in the engine's GLM-compatible convention.
///
/// The half-angle product, which is the inverse of [`quat_to_euler_zyx`] up to the
/// degenerate gimbal case.
/// The scene owns this convention so consumers ([`Transform`]-reading animation rest
/// poses) build the rest rotation identically to [`transform_matrix`] rather than
/// re-deriving the Euler order.
#[must_use]
pub fn quat_from_euler_xyz(euler: Vec3) -> Quat {
    let c = Vec3::new(
        (euler.x * 0.5).cos(),
        (euler.y * 0.5).cos(),
        (euler.z * 0.5).cos(),
    );
    let s = Vec3::new(
        (euler.x * 0.5).sin(),
        (euler.y * 0.5).sin(),
        (euler.z * 0.5).sin(),
    );
    let w = c.x * c.y * c.z + s.x * s.y * s.z;
    let x = s.x * c.y * c.z - c.x * s.y * s.z;
    let y = c.x * s.y * c.z + s.x * c.y * s.z;
    let z = c.x * c.y * s.z - s.x * s.y * c.z;
    Quat::from_xyzw(x, y, z, w)
}

/// A quaternion in the engine's stable Rz·Ry·Rx Euler convention (radians, XYZ order).
///
/// glam's `Quat::to_euler` is unstable at yaw ±90° (its asin/atan2 split poisons
/// pitch/roll), so this hand-rolls the `Rz·Ry·Rx` matrix extraction. The degenerate branch is implicit in the
/// `atan2` formulation: at the gimbal pole the recovered triple differs from the input
/// triple but reproduces the same rotation matrix, which is exactly what the
/// reparent-rebase needs. This is the one place a quaternion becomes a [`Transform`]
/// Euler.
#[must_use]
pub fn quat_to_euler_zyx(q: Quat) -> Vec3 {
    let m = Mat4::from_quat(q);
    let m00 = m.x_axis.x;
    let m01 = m.x_axis.y;
    let m02 = m.x_axis.z;
    let m10 = m.y_axis.x;
    let m11 = m.y_axis.y;
    let m12 = m.y_axis.z;
    let m20 = m.z_axis.x;
    let m21 = m.z_axis.y;
    let m22 = m.z_axis.z;

    let t1 = m01.atan2(m00);
    let c2 = m12.mul_add(m12, m22 * m22).sqrt();
    let t2 = (-m02).atan2(c2);
    let s1 = t1.sin();
    let c1 = t1.cos();
    let t3 = (s1 * m20 - c1 * m21).atan2(c1 * m11 - s1 * m10);

    // Fill `(t1, t2, t3)` into `(euler.z, euler.y, euler.x)`.
    Vec3::new(t3, t2, t1)
}

impl Scene {
    /// A transformable entity's effective local matrix: the animation [`PoseOverride`]
    /// when present (composed from its quaternion directly, no Euler round-trip), else
    /// the authored [`Transform`].
    ///
    /// Preferring the override keeps the rest pose pristine under non-destructive Edit
    /// preview. Returns identity when the entity carries neither.
    #[must_use]
    pub fn local_matrix(&self, entity: Entity) -> Mat4 {
        if let Ok(pose) = self.component::<PoseOverride>(entity) {
            return Mat4::from_translation(pose.translation)
                * Mat4::from_quat(pose.rotation)
                * Mat4::from_scale(pose.scale);
        }
        self.with_component::<Transform, _>(entity, transform_matrix)
            .unwrap_or(Mat4::IDENTITY)
    }

    /// The exact world matrix composed by walking the parent chain.
    ///
    /// Used where the cached per-frame matrix may lag a just-edited local transform
    /// (the reparenting math). An entity with no [`Transform`] contributes identity for
    /// its own link; an entity with no [`Relationship`] is treated as a root.
    #[must_use]
    pub fn compose_world_matrix(&self, entity: Entity) -> Mat4 {
        let local = if self.has_component::<Transform>(entity) {
            self.local_matrix(entity)
        } else {
            Mat4::IDENTITY
        };
        if let Some(parent) = self.parent_handle(entity) {
            return self.compose_world_matrix(parent) * local;
        }
        local
    }

    /// The cached world matrix written by [`Scene::update_world_transforms`], composing
    /// on a cache miss.
    #[must_use]
    pub fn world_matrix(&self, entity: Entity) -> Mat4 {
        if let Ok(world) = self.component::<WorldTransform>(entity) {
            return world.matrix;
        }
        self.compose_world_matrix(entity)
    }

    /// The world-space translation of an entity (the world matrix's translation column).
    #[must_use]
    pub fn world_translation(&self, entity: Entity) -> Vec3 {
        self.world_matrix(entity).w_axis.truncate()
    }

    /// The world-space rotation with scale divided out (gizmo Local axes, spot/camera
    /// aim).
    #[must_use]
    pub fn world_rotation(&self, entity: Entity) -> Quat {
        let world = self.world_matrix(entity);
        let mut scale = Vec3::new(
            world.x_axis.truncate().length(),
            world.y_axis.truncate().length(),
            world.z_axis.truncate().length(),
        );
        scale = scale.max(Vec3::splat(1e-8));
        let rotation = Mat3::from_cols(
            world.x_axis.truncate() / scale.x,
            world.y_axis.truncate() / scale.y,
            world.z_axis.truncate() / scale.z,
        );
        Quat::from_mat3(&rotation)
    }

    /// The resolved parent handle for `entity`, or `None` for a root / an entity with no
    /// [`Relationship`].
    fn parent_handle(&self, entity: Entity) -> Option<Entity> {
        self.with_component::<Relationship, _>(entity, |rel| rel.parent_handle)
            .unwrap_or(None)
    }

    /// Rebuilds the `parent_handle`/`children` caches from the durable parent uuids.
    ///
    /// One O(N) pass that keeps the runtime caches a clean forest: entities missing a
    /// [`Relationship`] (e.g. created by the raw loader path) get a default root one;
    /// dangling parent uuids, self-parents, and parent cycles in the source data reset to
    /// root with a warning; and [`SkinnedMesh::bone_handles`] is resolved from the joint
    /// uuids. Call after any structural change (load, reparent, copy) before traversing
    /// the tree.
    pub fn relink_hierarchy(&mut self) {
        let mut uuid_to_handle: std::collections::HashMap<Uuid, Entity> =
            std::collections::HashMap::new();
        let mut missing_relationship: Vec<Entity> = Vec::new();
        self.for_each::<&IdComponent, _>(|e, id| {
            uuid_to_handle.insert(id.id, e);
        });
        // Default a root relationship onto every entity that lacks one, so the whole world
        // stays hierarchy-addressable.
        self.for_each::<(&IdComponent, Option<&Relationship>), _>(|e, (_, rel)| {
            if rel.is_none() {
                missing_relationship.push(e);
            }
        });
        for e in missing_relationship {
            let _ = self.add_component(e, Relationship::default());
        }

        // Clear the caches before rebuilding.
        self.for_each::<&mut Relationship, _>(|_, rel| {
            rel.parent_handle = None;
            rel.children.clear();
        });

        // Resolve each parent uuid, sanitizing dangling / self parents to root.
        let mut resolved: Vec<(Entity, Entity)> = Vec::new();
        let mut reset_to_root: Vec<Entity> = Vec::new();
        self.for_each::<&Relationship, _>(|e, rel| {
            if rel.parent == Uuid(0) {
                return;
            }
            match uuid_to_handle.get(&rel.parent) {
                Some(&handle) if handle != e => resolved.push((e, handle)),
                other => {
                    let reason = if other.is_none() {
                        "not found"
                    } else {
                        "is the entity itself"
                    };
                    tracing::warn!(
                        "relationship parent {} {}; treating as root",
                        rel.parent.0,
                        reason
                    );
                    reset_to_root.push(e);
                }
            }
        });
        for e in reset_to_root {
            let _ = self.with_component_mut::<Relationship, _>(e, |rel| rel.parent = Uuid(0));
        }
        for &(child, parent) in &resolved {
            let _ = self.with_component_mut::<Relationship, _>(child, |rel| {
                rel.parent_handle = Some(parent);
            });
        }

        // Cut any parent cycle the source data carried (a hand-edited file can hold one;
        // `set_parent` refuses to create them). A walk longer than the entity count must
        // be looping; resetting the current entity to root breaks the loop for all members.
        let entity_count = uuid_to_handle.len();
        let mut starts: Vec<(Entity, Option<Entity>, Uuid)> = Vec::new();
        self.for_each::<&Relationship, _>(|e, rel| {
            starts.push((e, rel.parent_handle, rel.parent));
        });
        let mut cycle_roots: Vec<Entity> = Vec::new();
        for (e, parent_handle, parent_uuid) in starts {
            let mut steps = 0usize;
            let mut ancestor = parent_handle;
            while let Some(a) = ancestor {
                steps += 1;
                if steps > entity_count {
                    tracing::warn!(
                        "relationship parent {} forms a cycle; treating as root",
                        parent_uuid.0
                    );
                    cycle_roots.push(e);
                    break;
                }
                ancestor = self.parent_handle(a);
            }
        }
        for e in cycle_roots {
            let _ = self.with_component_mut::<Relationship, _>(e, |rel| {
                rel.parent = Uuid(0);
                rel.parent_handle = None;
            });
        }

        // Fill the children caches from the resolved parent handles.
        let mut links: Vec<(Entity, Entity)> = Vec::new();
        self.for_each::<&Relationship, _>(|e, rel| {
            if let Some(parent) = rel.parent_handle {
                links.push((parent, e));
            }
        });
        for &(parent, child) in &links {
            let _ = self.with_component_mut::<Relationship, _>(parent, |rel| {
                rel.children.push(child);
            });
        }

        // Resolve skinned-mesh joint uuids to live handles with the same map; an
        // unresolved joint warns once here and deforms by identity in `joint_matrices`.
        let mut skin_resolves: Vec<(Entity, Vec<Option<Entity>>)> = Vec::new();
        self.for_each::<&SkinnedMesh, _>(|e, skin| {
            let handles: Vec<Option<Entity>> = skin
                .bones
                .iter()
                .map(|bone| {
                    let resolved = uuid_to_handle.get(bone).copied();
                    if resolved.is_none() {
                        tracing::warn!(
                            "skinned mesh joint {} not found; deforming with identity",
                            bone.0
                        );
                    }
                    resolved
                })
                .collect();
            skin_resolves.push((e, handles));
        });
        for (e, handles) in skin_resolves {
            let _ = self.with_component_mut::<SkinnedMesh, _>(e, |skin| {
                skin.bone_handles = handles.iter().map(|h| h.unwrap_or(Entity::NULL)).collect();
            });
        }
    }

    /// Writes the cached [`WorldTransform`] for every transformable entity, roots-first
    /// then down the children caches.
    ///
    /// Ordering comes from the recursion, never from ECS iteration order. Full `Mat4`
    /// composition preserves non-uniform parent scale so the downstream
    /// `normal_matrix = transpose(inverse(mat3(world)))` stays correct. Runs once per
    /// frame before render; relies on [`Scene::relink_hierarchy`]-fresh caches.
    pub fn update_world_transforms(&mut self) {
        let mut roots: Vec<Entity> = Vec::new();
        self.for_each::<&Relationship, _>(|e, rel| {
            if rel.parent_handle.is_none() {
                roots.push(e);
            }
        });
        for root in roots {
            self.write_subtree(root, Mat4::IDENTITY);
        }
    }

    /// Recursive helper for [`Scene::update_world_transforms`]: writes `entity`'s world
    /// matrix (when it is transformable) then descends its children.
    fn write_subtree(&mut self, entity: Entity, parent_world: Mat4) {
        let mut world = parent_world;
        if self.has_component::<Transform>(entity) {
            world = parent_world * self.local_matrix(entity);
            if self.has_component::<WorldTransform>(entity) {
                let _ = self.with_component_mut::<WorldTransform, _>(entity, |w| w.matrix = world);
            } else {
                let _ = self.add_component(entity, WorldTransform { matrix: world });
            }
        }
        let children = self
            .with_component::<Relationship, _>(entity, |rel| rel.children.clone())
            .unwrap_or_default();
        for child in children {
            self.write_subtree(child, world);
        }
    }

    /// Fills the joint palette with `world(bones[i]) · inverse_bind[i]` per joint — the
    /// matrices the GPU skinning pass blends.
    ///
    /// Run after [`Scene::update_world_transforms`] so the cached bone world matrices are
    /// current; an unresolved joint (the relink already warned) deforms by identity. The
    /// skinned node's own transform is deliberately absent — per glTF, the joints place
    /// the vertices entirely.
    #[must_use]
    pub fn joint_matrices(&self, skin: &SkinnedMesh) -> Vec<Mat4> {
        let count = skin.bones.len();
        let mut out = vec![Mat4::IDENTITY; count];
        for (i, slot) in out.iter_mut().enumerate() {
            let Some(&bone) = skin.bone_handles.get(i) else {
                continue;
            };
            if bone == Entity::NULL || !self.valid(bone) {
                continue;
            }
            let inverse_bind = skin.inverse_bind.get(i).copied().unwrap_or(Mat4::IDENTITY);
            *slot = self.world_matrix(bone) * inverse_bind;
        }
        out
    }

    /// Decomposes `local` into `entity`'s [`Transform`].
    ///
    /// TRS-only: under a sheared source matrix the shear is lost (accepted — a
    /// [`Transform`] stores Euler + scale, no shear). Returns `false`, leaving the
    /// transform untouched, when the matrix does not decompose. The rotation is stored as
    /// the stable ZYX Euler via [`quat_to_euler_zyx`].
    pub fn set_local_from_matrix(&mut self, entity: Entity, local: Mat4) -> bool {
        let (scale, rotation, translation) = local.to_scale_rotation_translation();
        if !scale.is_finite() || !translation.is_finite() || !rotation.is_finite() {
            return false;
        }
        self.with_component_mut::<Transform, _>(entity, |transform| {
            transform.translation = translation;
            transform.rotation = quat_to_euler_zyx(rotation);
            transform.scale = scale;
        })
        .is_ok()
    }

    /// The engine-authoritative reparent — the only sanctioned one.
    ///
    /// Refuses self-parenting and cycles (walks `new_parent`'s ancestry). With
    /// `keep_world` (the editor convention) the child's local TRS is rebased so its world
    /// transform is unchanged. A `None` `new_parent` detaches the child to root. Sets the
    /// durable parent uuid (not the handle) and calls [`Scene::relink_hierarchy`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Reparent`] when `child` or `new_parent` is invalid, when the
    /// target is the child itself, or when the reparent would create a cycle.
    pub fn set_parent(
        &mut self,
        child: Entity,
        new_parent: Option<Entity>,
        keep_world: bool,
    ) -> Result<()> {
        if !self.valid(child) {
            return Err(Error::Reparent("invalid child entity".into()));
        }
        if let Some(parent) = new_parent {
            if !self.valid(parent) {
                return Err(Error::Reparent("invalid parent entity".into()));
            }
            if parent == child {
                return Err(Error::Reparent("cannot parent an entity to itself".into()));
            }
            let mut ancestor = Some(parent);
            while let Some(a) = ancestor {
                if a == child {
                    return Err(Error::Reparent("reparent would create a cycle".into()));
                }
                if !self.has_component::<Relationship>(a) {
                    break;
                }
                ancestor = self.parent_handle(a);
            }
        }

        if !self.has_component::<Relationship>(child) {
            let _ = self.add_component(child, Relationship::default());
        }

        let child_world = if keep_world {
            self.compose_world_matrix(child)
        } else {
            Mat4::IDENTITY
        };

        let parent_uuid = match new_parent {
            None => Uuid(0),
            Some(parent) => self.component::<IdComponent>(parent).map(|id| id.id)?,
        };
        let _ = self.with_component_mut::<Relationship, _>(child, |rel| rel.parent = parent_uuid);

        if keep_world && self.has_component::<Transform>(child) {
            let parent_world = match new_parent {
                Some(parent) => self.compose_world_matrix(parent),
                None => Mat4::IDENTITY,
            };
            self.set_local_from_matrix(child, parent_world.inverse() * child_world);
        }

        self.relink_hierarchy();
        Ok(())
    }

    /// The resolved primary camera (view matrix + projection params), or `None` when the
    /// scene has no primary camera.
    ///
    /// The world matrix composes the parent chain, so a parented camera views from its
    /// world placement (and inherits parent scale into the view basis).
    #[must_use]
    pub fn primary_camera(&mut self) -> Option<CameraView> {
        let mut found: Option<(Entity, Camera)> = None;
        self.for_each::<(&Transform, &Camera), _>(|entity, (_, camera)| {
            if found.is_none() && camera.primary {
                found = Some((entity, *camera));
            }
        });
        let (entity, camera) = found?;
        Some(CameraView {
            view: self.world_matrix(entity).inverse(),
            fov: camera.fov,
            near_plane: camera.near_plane,
            far_plane: camera.far_plane,
        })
    }

    /// The entity that carries a model instance's rig — the first descendant with a
    /// [`SkinnedMesh`] or [`AnimationPlayer`].
    ///
    /// Returns `root` when none, so a non-skinned single-entity model resolves to itself.
    /// Walks the children caches, so call after [`Scene::relink_hierarchy`].
    #[must_use]
    pub fn animatable_descendant(&self, root: Entity) -> Entity {
        self.find_animatable(root).unwrap_or(root)
    }

    /// Pre-order search for the first [`SkinnedMesh`]/[`AnimationPlayer`] descendant.
    fn find_animatable(&self, entity: Entity) -> Option<Entity> {
        if self.has_component::<SkinnedMesh>(entity)
            || self.has_component::<AnimationPlayer>(entity)
        {
            return Some(entity);
        }
        let children = self
            .with_component::<Relationship, _>(entity, |rel| rel.children.clone())
            .unwrap_or_default();
        for child in children {
            if let Some(found) = self.find_animatable(child) {
                return Some(found);
            }
        }
        None
    }

    /// The model's single animation authority for any entity in its forest: resolve up to
    /// the [`ModelInstance`] root, then down to its [`SkinnedMesh`]/[`AnimationPlayer`]
    /// entity. So a selected leaf (e.g. a `Morph` mesh child) resolves to the *same* player
    /// the container holds — never a sibling/leaf that would spawn a rival player. An entity
    /// with no `ModelInstance` ancestor resolves through itself (identical to the bare
    /// down-walk).
    #[must_use]
    pub fn model_player(&self, entity: Entity) -> Entity {
        self.animatable_descendant(self.model_root_of(entity))
    }

    /// The model instance's root: the nearest ancestor (including `entity`) carrying a
    /// [`ModelInstance`], or `entity` if none.
    ///
    /// Lets a viewport pick of an inner mesh or bone resolve to the whole model.
    #[must_use]
    pub fn model_root_of(&self, entity: Entity) -> Entity {
        let mut cursor = Some(entity);
        while let Some(c) = cursor {
            if !self.valid(c) {
                break;
            }
            if self.has_component::<ModelInstance>(c) {
                return c;
            }
            cursor = if self.has_component::<Relationship>(c) {
                self.parent_handle(c)
            } else {
                None
            };
        }
        entity
    }

    /// Every entity in `root`'s subtree (inclusive) carrying a renderable mesh — a [`Mesh`]
    /// or [`SkinnedMesh`]. This is the model's actual draw set, independent of which node the
    /// importer placed the geometry on: a single-identity model resolves to one entity, while
    /// a multi-node forest resolves to every mesh-bearing child. Walks the children caches, so
    /// call after [`Scene::relink_hierarchy`].
    #[must_use]
    pub fn model_mesh_entities(&self, root: Entity) -> Vec<Entity> {
        let mut out = Vec::new();
        self.collect_mesh_entities(root, &mut out);
        out
    }

    fn collect_mesh_entities(&self, entity: Entity, out: &mut Vec<Entity>) {
        if !self.valid(entity) {
            return;
        }
        if self.has_component::<Mesh>(entity) || self.has_component::<SkinnedMesh>(entity) {
            out.push(entity);
        }
        for child in self.children_of(entity) {
            self.collect_mesh_entities(child, out);
        }
    }

    /// Whether `root`'s subtree contains any renderable mesh. The renderability predicate the
    /// asset-preview gate keys off: true for a multi-node forest whose meshes ride child
    /// nodes, not only for a model whose root entity carries the mesh itself.
    #[must_use]
    pub fn model_has_renderable(&self, root: Entity) -> bool {
        self.find_subtree(root, &|s, e| {
            s.has_component::<Mesh>(e) || s.has_component::<SkinnedMesh>(e)
        })
        .is_some()
    }

    /// The rig carrier of a model: the first subtree entity (pre-order) with a [`SkinnedMesh`],
    /// or `None` for an unrigged model.
    ///
    /// Distinct from [`Scene::animatable_descendant`], which resolves the *animation authority*
    /// (a [`SkinnedMesh`] **or** [`AnimationPlayer`]); the skeleton overlay and rig tooling need
    /// the skinned-mesh entity specifically, which on an animated forest is a child of the
    /// container that carries the player.
    #[must_use]
    pub fn model_rig_entity(&self, root: Entity) -> Option<Entity> {
        self.find_subtree(root, &|s, e| s.has_component::<SkinnedMesh>(e))
    }

    /// The morph carrier of a model: the first subtree entity with a [`MorphComponent`], or
    /// `None`. The write target for morph-weight commands when a forest's morph mesh rides a
    /// child node while the selection resolves to the container.
    #[must_use]
    pub fn model_morph_entity(&self, root: Entity) -> Option<Entity> {
        self.find_subtree(root, &|s, e| s.has_component::<MorphComponent>(e))
    }

    fn children_of(&self, entity: Entity) -> Vec<Entity> {
        self.with_component::<Relationship, _>(entity, |rel| rel.children.clone())
            .unwrap_or_default()
    }

    /// Pre-order search for the first subtree entity (inclusive of `entity`) satisfying `pred`.
    fn find_subtree(&self, entity: Entity, pred: &dyn Fn(&Self, Entity) -> bool) -> Option<Entity> {
        if !self.valid(entity) {
            return None;
        }
        if pred(self, entity) {
            return Some(entity);
        }
        for child in self.children_of(entity) {
            if let Some(found) = self.find_subtree(child, pred) {
                return Some(found);
            }
        }
        None
    }
}

/// An un-flipped perspective projection for the resolved camera (GL clip convention).
#[must_use]
pub fn camera_projection(camera: &CameraView, aspect: f32) -> Mat4 {
    Mat4::perspective_rh_gl(
        camera.fov.to_radians(),
        aspect,
        camera.near_plane,
        camera.far_plane,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::SkinnedMesh;

    /// The forest-aware model resolvers across the five spawn shapes: a single-mesh root, a
    /// static multi-node forest (meshes on children, no player), a rigged forest (SkinnedMesh
    /// on a child), an animated forest (AnimationPlayer on the container, mesh on a child), and
    /// a morph child. These back the asset-preview gate, the bounds, and the overlay; the bug
    /// they fix is resolving a single entity and assuming it carries the mesh.
    #[test]
    fn model_resolvers_cover_every_spawn_shape() {
        // S1: a single entity that carries the mesh directly (the collapsed single-identity root).
        let mut scene = Scene::new();
        let single = scene.create_entity("Single");
        scene
            .add_component(single, Mesh { mesh: Uuid(11) })
            .unwrap();
        assert_eq!(scene.model_mesh_entities(single), vec![single]);
        assert!(scene.model_has_renderable(single));
        assert_eq!(scene.model_rig_entity(single), None);

        // S2: a static multi-node forest — the container holds nothing; two child nodes hold
        // the meshes. A single-entity probe on the container finds no mesh, so the resolver must
        // descend into the children.
        let mut scene = Scene::new();
        let container = scene.create_entity("Forest");
        let door_a = scene.create_entity("DoorA");
        let door_b = scene.create_entity("DoorB");
        scene
            .add_component(door_a, Mesh { mesh: Uuid(21) })
            .unwrap();
        scene
            .add_component(door_b, Mesh { mesh: Uuid(22) })
            .unwrap();
        scene.set_parent(door_a, Some(container), false).unwrap();
        scene.set_parent(door_b, Some(container), false).unwrap();
        assert!(!scene.has_component::<Mesh>(container));
        assert!(scene.model_has_renderable(container));
        let meshes = scene.model_mesh_entities(container);
        assert_eq!(
            meshes.len(),
            2,
            "both child meshes resolve, not the container"
        );
        assert!(meshes.contains(&door_a) && meshes.contains(&door_b));
        assert_eq!(scene.model_rig_entity(container), None);

        // S3: a rigged forest — SkinnedMesh on a child mesh entity, container bare.
        let mut scene = Scene::new();
        let container = scene.create_entity("Rig");
        let mesh_entity = scene.create_entity("RigMesh");
        scene
            .add_component(
                mesh_entity,
                SkinnedMesh {
                    mesh: Uuid(31),
                    ..SkinnedMesh::default()
                },
            )
            .unwrap();
        scene
            .set_parent(mesh_entity, Some(container), false)
            .unwrap();
        assert_eq!(scene.model_rig_entity(container), Some(mesh_entity));
        assert!(scene.model_has_renderable(container));
        assert_eq!(scene.model_mesh_entities(container), vec![mesh_entity]);

        // S4: an animated forest — the container carries the AnimationPlayer (so a player-first
        // resolver short-circuits there), but the mesh rides a child.
        let mut scene = Scene::new();
        let container = scene.create_entity("Animated");
        let child = scene.create_entity("AnimMesh");
        scene
            .add_component(container, AnimationPlayer::default())
            .unwrap();
        scene.add_component(child, Mesh { mesh: Uuid(41) }).unwrap();
        scene.set_parent(child, Some(container), false).unwrap();
        assert!(
            scene.model_has_renderable(container),
            "renderable even though the player-bearing container holds no mesh"
        );
        assert_eq!(scene.model_mesh_entities(container), vec![child]);

        // S5: a morph child resolves through the container.
        let mut scene = Scene::new();
        let container = scene.create_entity("Morph");
        let child = scene.create_entity("MorphMesh");
        scene.add_component(child, Mesh { mesh: Uuid(51) }).unwrap();
        scene
            .add_component(child, MorphComponent::default())
            .unwrap();
        scene.set_parent(child, Some(container), false).unwrap();
        assert_eq!(scene.model_morph_entity(container), Some(child));
        assert!(scene.model_has_renderable(container));
    }

    /// A `1e-4` matrix tolerance, applied element-wise (column-major).
    fn near_equal(a: Mat4, b: Mat4) -> bool {
        let cols = [
            (a.x_axis, b.x_axis),
            (a.y_axis, b.y_axis),
            (a.z_axis, b.z_axis),
            (a.w_axis, b.w_axis),
        ];
        cols.iter()
            .all(|(ca, cb)| (*ca - *cb).abs().max_element() <= 1e-4)
    }

    fn set_translation(scene: &mut Scene, e: Entity, t: Vec3) {
        scene
            .with_component_mut::<Transform, _>(e, |tr| tr.translation = t)
            .unwrap();
    }

    fn local_translation(scene: &Scene, e: Entity) -> Vec3 {
        scene.component::<Transform>(e).unwrap().translation
    }

    fn id_of(scene: &Scene, e: Entity) -> Uuid {
        scene.component::<IdComponent>(e).unwrap().id
    }

    /// The full hierarchy self-test: the parent/child/grandchild composition, the cycle +
    /// self-parent guards, the `keep_world` rebase under axis-aligned and generic rotations,
    /// the recursive destroy, the two-roots-after-reparent count, the parented camera view,
    /// and the bind-pose + moved-joint palette.
    #[test]
    fn scene_hierarchy_self_test() {
        let mut scene = Scene::new();
        let parent = scene.create_entity("Parent");
        let child = scene.create_entity("Child");
        let grandchild = scene.create_entity("Grandchild");
        set_translation(&mut scene, parent, Vec3::new(10.0, 0.0, 0.0));

        // Parent-before-child composition: locals set after parenting (keep_world=false
        // keeps them as authored), so the pass must compose parent * local.
        scene.set_parent(child, Some(parent), false).unwrap();
        scene.set_parent(grandchild, Some(child), false).unwrap();
        set_translation(&mut scene, child, Vec3::new(0.0, 2.0, 0.0));
        set_translation(&mut scene, grandchild, Vec3::new(0.0, 0.0, 3.0));
        scene.update_world_transforms();

        let child_local = transform_matrix(&scene.component::<Transform>(child).unwrap());
        assert!(
            near_equal(
                scene.world_matrix(child),
                scene.world_matrix(parent) * child_local
            ),
            "child world == parent world * child local"
        );
        assert!(
            scene
                .world_translation(grandchild)
                .distance(Vec3::new(10.0, 2.0, 3.0))
                < 1e-4,
            "grandchild world translation"
        );

        // Guards: parenting an ancestor under its descendant, or an entity under itself.
        assert!(
            scene.set_parent(parent, Some(grandchild), true).is_err(),
            "cycle guard"
        );
        assert!(
            scene.set_parent(child, Some(child), true).is_err(),
            "self-parent guard"
        );

        // keep_world rebase: move grandchild under a rotated, scaled mover; the world
        // transform must not change while the local TRS does.
        let mover = scene.create_entity("Mover");
        scene
            .with_component_mut::<Transform, _>(mover, |t| {
                t.translation = Vec3::new(-4.0, 1.0, 0.5);
                t.rotation = Vec3::new(0.0, std::f32::consts::FRAC_PI_2, 0.0);
                t.scale = Vec3::splat(2.0);
            })
            .unwrap();
        let before = scene.compose_world_matrix(grandchild);
        scene.set_parent(grandchild, Some(mover), true).unwrap();
        assert!(
            near_equal(scene.compose_world_matrix(grandchild), before),
            "keep_world preserves world transform"
        );
        assert!(
            local_translation(&scene, grandchild).distance(Vec3::new(0.0, 0.0, 3.0)) > 1e-3,
            "keep_world rebases the local transform"
        );

        // Same rebase under a generic (non-axis-aligned) parent rotation.
        let mover2 = scene.create_entity("Mover2");
        scene
            .with_component_mut::<Transform, _>(mover2, |t| {
                t.translation = Vec3::new(2.0, -3.0, 1.0);
                t.rotation = Vec3::new(0.4, 0.9, -0.3);
                t.scale = Vec3::splat(1.5);
            })
            .unwrap();
        let before_generic = scene.compose_world_matrix(grandchild);
        scene.set_parent(grandchild, Some(mover2), true).unwrap();
        assert!(
            near_equal(scene.compose_world_matrix(grandchild), before_generic),
            "keep_world preserves world transform under a generic rotation"
        );

        // Recursive destroy takes the subtree; the reparented grandchild survives.
        scene.destroy_entity(parent);
        assert!(
            !scene.valid(parent) && !scene.valid(child),
            "destroy removes the subtree"
        );
        assert!(
            scene.valid(grandchild),
            "reparented entity survives its old ancestor's destroy"
        );

        // Gather every cached child handle, then check validity in a fresh pass (the
        // `for_each` closure borrows the scene, so the check cannot run inside it).
        let mut child_handles: Vec<Entity> = Vec::new();
        scene.for_each::<&Relationship, _>(|_, rel| {
            child_handles.extend(rel.children.iter().copied());
        });
        let dangling = child_handles.iter().filter(|&&c| !scene.valid(c)).count();
        assert_eq!(dangling, 0, "no children cache holds a destroyed handle");

        let mut roots = 0;
        let mut total = 0;
        scene.for_each::<&Relationship, _>(|_, rel| {
            total += 1;
            if rel.parent == Uuid(0) && rel.parent_handle.is_none() {
                roots += 1;
            }
        });
        assert_eq!(
            (total, roots),
            (3, 2),
            "expected mover + mover2 + grandchild with two roots"
        );

        // A parented primary camera views from its world placement.
        let cam_parent = scene.create_entity("CamParent");
        set_translation(&mut scene, cam_parent, Vec3::new(3.0, 4.0, 5.0));
        let cam = scene.create_entity("Camera");
        scene.add_component(cam, Camera::default()).unwrap();
        scene.set_parent(cam, Some(cam_parent), false).unwrap();
        set_translation(&mut scene, cam, Vec3::new(1.0, 0.0, 0.0));
        scene.update_world_transforms();
        let view = scene.primary_camera().expect("primary camera resolves");
        assert!(
            view.view
                .inverse()
                .w_axis
                .truncate()
                .distance(Vec3::new(4.0, 4.0, 5.0))
                < 1e-4,
            "parented camera views from its world position"
        );

        // Research gate (CPU half): joint_matrices() must produce world_bone *
        // inverse_bind in joint order, identity at bind pose, and never compose the
        // skinned node's own transform.
        let joint_root = scene.create_entity("JointRoot");
        let joint_tip = scene.create_entity("JointTip");
        set_translation(&mut scene, joint_root, Vec3::new(1.0, 0.0, 0.0));
        scene
            .set_parent(joint_tip, Some(joint_root), false)
            .unwrap();
        set_translation(&mut scene, joint_tip, Vec3::new(0.0, 3.0, 0.0));
        let skinned_node = scene.create_entity("SkinnedNode");
        set_translation(&mut scene, skinned_node, Vec3::new(50.0, 0.0, 0.0));
        scene
            .add_component(
                skinned_node,
                SkinnedMesh {
                    bones: vec![id_of(&scene, joint_root), id_of(&scene, joint_tip)],
                    inverse_bind: vec![
                        Mat4::from_translation(Vec3::new(-1.0, 0.0, 0.0)),
                        Mat4::from_translation(Vec3::new(-1.0, -3.0, 0.0)),
                    ],
                    ..SkinnedMesh::default()
                },
            )
            .unwrap();
        scene.relink_hierarchy();
        scene.update_world_transforms();

        let skin = clone_skin(&scene, skinned_node);
        let palette = scene.joint_matrices(&skin);
        assert_eq!(palette.len(), 2);
        assert!(
            near_equal(palette[0], Mat4::IDENTITY) && near_equal(palette[1], Mat4::IDENTITY),
            "bind-pose joint matrices are identity"
        );

        set_translation(&mut scene, joint_tip, Vec3::new(0.0, 5.0, 0.0));
        scene.update_world_transforms();
        let palette = scene.joint_matrices(&skin);
        let moved = palette[1] * glam::Vec4::new(1.0, 3.0, 0.0, 1.0);
        assert!(
            moved.truncate().distance(Vec3::new(1.0, 5.0, 0.0)) < 1e-4,
            "a tip-bound vertex follows the moved joint"
        );
    }

    fn clone_skin(scene: &Scene, e: Entity) -> SkinnedMesh {
        scene
            .with_component::<SkinnedMesh, _>(e, Clone::clone)
            .unwrap()
    }

    /// The dedicated `quat_to_euler_zyx` coverage: a generic round-trip and the yaw ±90°
    /// degenerate case, both asserted by the rotation matrix the recovered triple
    /// rebuilds (the gimbal pole gives a different triple but the same rotation, which is
    /// exactly the stability the engine relies on).
    #[test]
    fn quat_to_euler_zyx_round_trips_through_transform() {
        let cases = [
            Vec3::new(0.4, 0.9, -0.3),
            Vec3::new(1.1, -0.7, 0.5),
            // Yaw +90°: glam's `Quat::to_euler` is unstable here; the ZYX extraction is
            // not.
            Vec3::new(0.1, std::f32::consts::FRAC_PI_2, -0.2),
            // Yaw -90°.
            Vec3::new(0.3, -std::f32::consts::FRAC_PI_2, 0.7),
        ];
        for euler in cases {
            let q = quat_from_euler_xyz(euler);
            let recovered = quat_to_euler_zyx(q);
            let rebuilt = quat_from_euler_xyz(recovered);
            assert!(
                near_equal(Mat4::from_quat(q), Mat4::from_quat(rebuilt)),
                "ZYX extraction reproduces the rotation for euler {euler:?} (recovered {recovered:?})"
            );
        }

        // Away from the pole the extracted triple matches the authored one exactly.
        let generic = Vec3::new(0.4, 0.9, -0.3);
        let recovered = quat_to_euler_zyx(quat_from_euler_xyz(generic));
        assert!(
            (recovered - generic).abs().max_element() < 1e-4,
            "non-degenerate euler round-trips component-wise"
        );

        // At the yaw +90° pole the recovered yaw still pins to +90°.
        let pole = Vec3::new(0.1, std::f32::consts::FRAC_PI_2, -0.2);
        let recovered_pole = quat_to_euler_zyx(quat_from_euler_xyz(pole));
        assert!(
            (recovered_pole.y - std::f32::consts::FRAC_PI_2).abs() < 1e-3,
            "yaw pins to +90° at the gimbal pole"
        );
    }

    /// `model_root_of` resolves an inner pick to its nearest `ModelInstance` ancestor and
    /// `animatable_descendant` finds the first rig below a container root.
    #[test]
    fn model_root_and_animatable_descendant() {
        use crate::component::{AnimationPlayer, ModelInstance};

        let mut scene = Scene::new();
        let root = scene.create_entity("ModelRoot");
        scene
            .add_component(root, ModelInstance { model_id: Uuid(42) })
            .unwrap();
        let mid = scene.create_entity("Mid");
        let rig = scene.create_entity("Rig");
        scene
            .add_component(rig, AnimationPlayer::default())
            .unwrap();
        scene.set_parent(mid, Some(root), false).unwrap();
        scene.set_parent(rig, Some(mid), false).unwrap();

        assert_eq!(
            scene.model_root_of(rig),
            root,
            "inner pick resolves to model"
        );
        assert_eq!(
            scene.model_root_of(root),
            root,
            "the model root resolves to itself"
        );
        assert_eq!(
            scene.animatable_descendant(root),
            rig,
            "first rig below the container is found"
        );
        // A leaf with no rig descendant resolves to itself.
        let lone = scene.create_entity("Lone");
        assert_eq!(scene.animatable_descendant(lone), lone);
    }

    /// `model_player` resolves any entity in a model's forest to its single animation
    /// authority (up to the `ModelInstance` root, then down to the player) — so selecting a
    /// leaf never points at a player-less sibling that would spawn a rival player.
    #[test]
    fn model_player_resolves_a_leaf_up_to_the_container_player() {
        use crate::component::{AnimationPlayer, ModelInstance};

        // Container holds ModelInstance + the AnimationPlayer; a child mesh leaf holds no
        // player (the node-forest morph shape).
        let mut scene = Scene::new();
        let container = scene.create_entity("Container");
        scene
            .add_component(container, ModelInstance { model_id: Uuid(7) })
            .unwrap();
        scene
            .add_component(container, AnimationPlayer::default())
            .unwrap();
        let leaf = scene.create_entity("Main");
        scene.set_parent(leaf, Some(container), false).unwrap();

        // From the leaf, resolve up-then-down to the container's player (not the leaf).
        assert_eq!(
            scene.model_player(leaf),
            container,
            "a leaf resolves to the model's single player on the container"
        );
        // From the container itself, the same authority.
        assert_eq!(scene.model_player(container), container);

        // A rig whose player lives on a descendant of the ModelInstance root: model_player
        // walks up to the root then down to the player entity.
        let mut rigged = Scene::new();
        let root = rigged.create_entity("Root");
        rigged
            .add_component(root, ModelInstance { model_id: Uuid(8) })
            .unwrap();
        let mesh = rigged.create_entity("Mesh");
        rigged
            .add_component(mesh, AnimationPlayer::default())
            .unwrap();
        rigged.set_parent(mesh, Some(root), false).unwrap();
        let inner = rigged.create_entity("Inner");
        rigged.set_parent(inner, Some(mesh), false).unwrap();
        assert_eq!(
            rigged.model_player(inner),
            mesh,
            "an inner pick resolves to the rig's player entity"
        );

        // An entity with no ModelInstance ancestor resolves through itself (bare down-walk).
        let mut bare = Scene::new();
        let solo = bare.create_entity("Solo");
        assert_eq!(bare.model_player(solo), solo);
    }

    /// `relink_hierarchy` sanitizes a hand-edited dangling parent uuid to root.
    #[test]
    fn relink_sanitizes_dangling_parent_to_root() {
        let mut scene = Scene::new();
        let e = scene.create_entity("Orphan");
        scene
            .with_component_mut::<Relationship, _>(e, |rel| rel.parent = Uuid(999_999))
            .unwrap();
        scene.relink_hierarchy();
        let rel = scene
            .with_component::<Relationship, _>(e, Clone::clone)
            .unwrap();
        assert_eq!(rel.parent, Uuid(0), "dangling parent reset to root");
        assert!(rel.parent_handle.is_none());
    }

    /// `set_local_from_matrix` decomposes a TRS matrix back into the transform and
    /// reproduces the matrix.
    #[test]
    fn set_local_from_matrix_round_trips() {
        let mut scene = Scene::new();
        let e = scene.create_entity("E");
        let src = Mat4::from_translation(Vec3::new(2.0, -1.0, 4.0))
            * Mat4::from_quat(quat_from_euler_xyz(Vec3::new(0.3, -0.8, 0.5)))
            * Mat4::from_scale(Vec3::new(1.5, 2.0, 0.5));
        assert!(scene.set_local_from_matrix(e, src));
        assert!(
            near_equal(scene.local_matrix(e), src),
            "decomposed transform rebuilds the source matrix"
        );
    }
}
