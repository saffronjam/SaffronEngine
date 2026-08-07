//! Collider and bone-capsule auto-fit from mesh bounds.

use glam::{Mat4, Vec3};

use saffron_core::Uuid;
use saffron_geometry::Mesh;
use saffron_scene::{
    BonePhysics, BonePhysicsComponent, Collider, Entity, Mesh as MeshComponent, Relationship,
    Scene, Shape, SkinnedMesh,
};

use super::*;

/// Auto-fit an entity's [`Collider`] to its mesh AABB, baking the entity's world scale into the
/// fitted extents/offset so the scale-free Jolt body matches the scaled visual mesh. Returns
/// `false` (leaving the collider unchanged) when the entity has no collider, no mesh to size
/// against, or a degenerate (single-point) mesh.
///
/// `cook` reads the entity's `Mesh`/`SkinnedMesh` `.smesh` (the same source cooking uses, so the
/// fit needs no GPU upload and works identically in Edit and headless) — it is the seam that keeps
/// the asset reader out of this crate. The control crate calls this from add-component + the
/// `fit-collider` command.
pub fn fit_collider_to_mesh(scene: &mut Scene, entity: Entity, cook: &mut MeshCook<'_>) -> bool {
    if !scene.has_component::<Collider>(entity) {
        return false;
    }
    // The mesh-bearing entities to size against: the collider entity itself when it carries a
    // mesh, else its forest — the meshes of a multi-node model ride child nodes under the
    // container the collider sits on, so probing only `entity` finds nothing.
    let mesh_entities: Vec<Entity> = if scene.has_component::<MeshComponent>(entity)
        || scene.has_component::<SkinnedMesh>(entity)
    {
        vec![entity]
    } else {
        scene.model_mesh_entities(entity)
    };
    if mesh_entities.is_empty() {
        return false; // no mesh to size against — keep the collider's defaults
    }

    // The Jolt body is built scale-free (world translation + rotation only). Union every mesh's
    // AABB in world space, then express it in the body's local frame `inv(T·R)`; for a single
    // mesh on the collider entity this reduces to the mesh-local box scaled by the entity's world
    // scale (since `inv(T·R) · (T·R·S) = S`), matching the prior single-mesh fit exactly.
    let body = scene.world_matrix(entity);
    let (_, body_rot, body_pos) = body.to_scale_rotation_translation();
    let to_body = Mat4::from_rotation_translation(body_rot, body_pos).inverse();
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    let mut source_mesh = Uuid(0);
    let mut found = false;
    for mesh_entity in mesh_entities {
        let mesh_id = scene
            .with_component::<MeshComponent, _>(mesh_entity, |m| m.mesh)
            .ok()
            .or_else(|| {
                scene
                    .with_component::<SkinnedMesh, _>(mesh_entity, |s| s.mesh)
                    .ok()
            })
            .unwrap_or(Uuid(0));
        if mesh_id.0 == 0 {
            continue;
        }
        let Ok(mesh) = cook(mesh_id) else {
            continue;
        };
        let Some((mlo, mhi)) = mesh_aabb(&mesh) else {
            continue;
        };
        if source_mesh.0 == 0 {
            source_mesh = mesh_id;
        }
        let to_local = to_body * scene.world_matrix(mesh_entity);
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { mlo.x } else { mhi.x },
                if i & 2 == 0 { mlo.y } else { mhi.y },
                if i & 4 == 0 { mlo.z } else { mhi.z },
            );
            let p = to_local.transform_point3(corner);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        found = true;
    }
    if !found {
        return false;
    }
    if (hi - lo).cmple(Vec3::ZERO).all() {
        return false; // a single degenerate point — nothing to size against (a planar mesh is fine)
    }

    let half = (hi - lo) * 0.5;
    let offset = (lo + hi) * 0.5;
    let half_extents = match scene.with_component::<Collider, _>(entity, |c| c.shape) {
        Ok(Shape::Box | Shape::ConvexHull | Shape::Mesh) => {
            // Hull/mesh fit a fallback box into half_extents; the cook uses the actual geometry.
            half
        }
        Ok(Shape::Sphere) => {
            // Bounding sphere of the box (never smaller than the mesh); radius packed in .x.
            Vec3::splat(half.x.max(half.y).max(half.z))
        }
        Ok(Shape::Capsule) => {
            // Y-up capsule: long axis = Y, radius = the larger of X/Z, half-height excludes the caps.
            let radius = half.x.max(half.z);
            let half_height = (half.y - radius).max(0.0);
            Vec3::new(radius, half_height, radius)
        }
        Err(_) => return false,
    };
    scene
        .with_component_mut::<Collider, _>(entity, |c| {
            c.offset = offset;
            c.source_mesh = source_mesh; // cook source for hull/mesh; analytic shapes ignore it
            c.half_extents = half_extents;
        })
        .is_ok()
}

/// Size each rest-pose bone capsule of a rig's [`BonePhysicsComponent`] from the distance to its
/// farthest child joint, writing the radius/half-height into `shape_half_extents`. Adds an empty
/// `BonePhysicsComponent` (then sizes it) when the rig has none. Returns `false` (unchanged) when
/// the entity has no `SkinnedMesh` or its bone list is empty.
pub fn fit_bone_capsules(scene: &mut Scene, rig: Entity) -> bool {
    let Ok(bone_handles) = scene.with_component::<SkinnedMesh, _>(rig, |s| s.bone_handles.clone())
    else {
        return false;
    };
    let count = bone_handles.len();
    if count == 0 {
        return false;
    }
    if !scene.has_component::<BonePhysicsComponent>(rig) {
        let _ = scene.add_component(rig, BonePhysicsComponent::default());
    }

    // Rest-pose world positions + ids per joint (Edit reads the authored rest skeleton).
    let mut rest_pos = vec![Vec3::ZERO; count];
    let mut uuid = vec![Uuid(0); count];
    for (i, &joint) in bone_handles.iter().enumerate() {
        if scene.valid(joint) {
            rest_pos[i] = scene.world_translation(joint);
            uuid[i] = id_of(scene, joint);
        }
    }

    let mut sized = vec![BonePhysics::default(); count];
    for i in 0..count {
        // Capsule half-height spans toward the farthest child joint; radius a fraction of that.
        let mut length = 0.0f32;
        for (child, &child_joint) in bone_handles.iter().enumerate() {
            if child == i || !scene.valid(child_joint) {
                continue;
            }
            let Ok(parent) = scene.with_component::<Relationship, _>(child_joint, |r| r.parent)
            else {
                continue;
            };
            if uuid[i].0 != 0 && parent == uuid[i] {
                length = length.max((rest_pos[child] - rest_pos[i]).length());
            }
        }
        let half_height = if length > 0.001 { length * 0.5 } else { 0.05 }; // leaf default
        let radius = (half_height * 0.3).max(0.03);
        sized[i].shape_half_extents = Vec3::new(radius, half_height, radius);
    }

    // Preserve any authored per-bone fields (mass/joint/limits/drive) the size pass does not touch:
    // resize to the bone count, then overwrite only `shape_half_extents`.
    scene
        .with_component_mut::<BonePhysicsComponent, _>(rig, |phys| {
            phys.bones.resize(count, BonePhysics::default());
            for (slot, sized) in phys.bones.iter_mut().zip(sized.iter()) {
                slot.shape_half_extents = sized.shape_half_extents;
            }
        })
        .is_ok()
}

/// The axis-aligned bounds of a cooked mesh's vertex positions, or `None` for an empty mesh.
fn mesh_aabb(mesh: &Mesh) -> Option<(Vec3, Vec3)> {
    let first = mesh.vertices.first()?.position;
    let mut lo = first;
    let mut hi = first;
    for vertex in &mesh.vertices {
        lo = lo.min(vertex.position);
        hi = hi.max(vertex.position);
    }
    Some((lo, hi))
}
