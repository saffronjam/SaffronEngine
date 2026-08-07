//! The ECS speed gate: `hecs` driven through the wrapped [`Scene`] surface on this engine's own
//! access patterns.
//!
//! Two halves of the workload: per-frame iteration (world-transform sync, draw enumeration, the
//! light gather, the primary-camera resolve) and the structural paths (the `enter_play`
//! JSON-roundtrip duplicate, `relink_hierarchy`, and `PoseOverride` add/remove on every animated
//! bone every frame — the one site where archetype moves could cost).
//!
//! The component structs are bench-local stand-ins: the gate measures the access-pattern cost
//! through the wrapper, not the production structs. Run with `cargo bench -p saffron-scene`.

use std::collections::HashMap;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use glam::{Mat4, Quat, Vec3, Vec4};
use saffron_core::Uuid;
use saffron_scene::{Entity, IdComponent, Scene};
use serde::{Deserialize, Serialize};

/// A node in the scene tree. `parent` (a uuid; 0 == root) is the only durable field;
/// `parent_handle` and `children` are runtime caches rebuilt by `relink_hierarchy`.
#[derive(Clone, Debug, Default)]
struct Relationship {
    parent: u64,
    parent_handle: Option<Entity>,
    children: Vec<Entity>,
}

/// Authored local TRS; rotation is the Euler XYZ the editor edits directly.
#[derive(Clone, Copy, Debug)]
struct Transform {
    translation: Vec3,
    rotation: Vec3,
    scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
        }
    }
}

impl Transform {
    /// The local matrix `T * R * S`.
    fn matrix(&self) -> Mat4 {
        Mat4::from_translation(self.translation)
            * Mat4::from_quat(Quat::from_euler(
                glam::EulerRot::XYZ,
                self.rotation.x,
                self.rotation.y,
                self.rotation.z,
            ))
            * Mat4::from_scale(self.scale)
    }
}

/// Cached world matrix, overwritten each frame by `update_world_transforms`.
#[derive(Clone, Copy, Debug)]
struct WorldTransform {
    matrix: Mat4,
}

/// The runtime-only animated local TRS the evaluator writes onto a driven bone each frame,
/// then removes when the rig stops. The single per-frame add/remove churn site.
#[derive(Clone, Copy, Debug)]
struct PoseOverride {
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
}

/// A renderable mesh reference.
#[derive(Clone, Copy, Debug)]
struct Mesh {
    mesh: u64,
}

/// A PBR material. Sized like the real one so the archetype column width is representative.
#[derive(Clone, Copy, Debug)]
struct Material {
    base_color: Vec4,
    albedo_texture: u64,
    metallic: f32,
    roughness: f32,
    emissive: Vec3,
}

/// A perspective camera.
#[derive(Clone, Copy, Debug)]
struct Camera {
    fov: f32,
    near_plane: f32,
    far_plane: f32,
    primary: bool,
}

/// An omnidirectional light.
#[derive(Clone, Copy, Debug)]
struct PointLight {
    color: Vec3,
    intensity: f32,
    range: f32,
}

/// Tags a skeleton joint so the per-frame pose churn can target exactly the animated bones.
#[derive(Clone, Copy, Debug)]
struct Bone;

/// The serialized form of one entity, the row that `scene_to_json`/`scene_from_json`
/// round-trips. Runtime caches (`parent_handle`, `children`, `WorldTransform`,
/// `PoseOverride`) are deliberately absent — they are rebuilt by `relink_hierarchy`, never
/// serialized.
#[derive(Serialize, Deserialize, Default)]
struct EntitySnapshot {
    id: u64,
    parent: u64,
    transform: Option<TransformDto>,
    mesh: Option<u64>,
    material: Option<MaterialDto>,
    camera: Option<CameraDto>,
    point_light: Option<PointLightDto>,
    bone: bool,
}

#[derive(Serialize, Deserialize)]
struct TransformDto {
    translation: [f32; 3],
    rotation: [f32; 3],
    scale: [f32; 3],
}

#[derive(Serialize, Deserialize)]
struct MaterialDto {
    base_color: [f32; 4],
    albedo_texture: u64,
    metallic: f32,
    roughness: f32,
    emissive: [f32; 3],
}

#[derive(Serialize, Deserialize)]
struct CameraDto {
    fov: f32,
    near_plane: f32,
    far_plane: f32,
    primary: bool,
}

#[derive(Serialize, Deserialize)]
struct PointLightDto {
    color: [f32; 3],
    intensity: f32,
    range: f32,
}

/// How many rigged characters the representative scene holds, and how many bones each
/// skeleton carries. The product is the per-frame `PoseOverride`-churn entity count and the
/// bulk of the transform-sync walk — sized to a few-thousand-entity scene.
const CHARACTER_COUNT: usize = 60;
const BONES_PER_CHARACTER: usize = 48;

/// Static (mesh + material) renderables not attached to a rig.
const PROP_COUNT: usize = 1200;

/// Point lights scattered through the scene.
const LIGHT_COUNT: usize = 256;

/// Cameras (only the first primary one is resolved, the rest are decoys the walk skips).
const CAMERA_COUNT: usize = 8;

/// Builds the representative scene and returns it plus the handles of every animated bone
/// (the per-frame `PoseOverride`-churn target set). Every entity carries a `Relationship`
/// and a `Transform`; rigs build a real bone chain, props/lights/cameras are roots.
fn build_scene() -> (Scene, Vec<Entity>) {
    let mut scene = Scene::new();
    let mut bone_handles = Vec::with_capacity(CHARACTER_COUNT * BONES_PER_CHARACTER);

    for c in 0..CHARACTER_COUNT {
        let root = scene.create_entity(format!("character{c}"));
        seed_transform(&mut scene, root, 0);
        let root_id = scene.component::<IdComponent>(root).unwrap().id.0;
        scene.add_component(root, Relationship::default()).unwrap();

        let mut parent_id = root_id;
        for b in 0..BONES_PER_CHARACTER {
            let bone = scene.create_entity(format!("bone{c}_{b}"));
            seed_transform(&mut scene, bone, b);
            scene
                .add_component(
                    bone,
                    Relationship {
                        parent: parent_id,
                        ..Relationship::default()
                    },
                )
                .unwrap();
            scene.add_component(bone, Bone).unwrap();
            // A loose tree so the hierarchy walk sees realistic depth and fan-out.
            parent_id = if b % 6 == 0 {
                root_id
            } else {
                scene.component::<IdComponent>(bone).unwrap().id.0
            };
            bone_handles.push(bone);
        }
    }

    for p in 0..PROP_COUNT {
        let e = scene.create_entity(format!("prop{p}"));
        seed_transform(&mut scene, e, p);
        scene.add_component(e, Relationship::default()).unwrap();
        scene.add_component(e, Mesh { mesh: 1 + p as u64 }).unwrap();
        scene
            .add_component(
                e,
                Material {
                    base_color: Vec4::ONE,
                    albedo_texture: 0,
                    metallic: 0.1,
                    roughness: 0.8,
                    emissive: Vec3::ZERO,
                },
            )
            .unwrap();
    }

    for l in 0..LIGHT_COUNT {
        let e = scene.create_entity(format!("light{l}"));
        seed_transform(&mut scene, e, l);
        scene.add_component(e, Relationship::default()).unwrap();
        scene
            .add_component(
                e,
                PointLight {
                    color: Vec3::ONE,
                    intensity: 5.0,
                    range: 10.0,
                },
            )
            .unwrap();
    }

    for k in 0..CAMERA_COUNT {
        let e = scene.create_entity(format!("camera{k}"));
        seed_transform(&mut scene, e, k);
        scene.add_component(e, Relationship::default()).unwrap();
        scene
            .add_component(
                e,
                Camera {
                    fov: 45.0,
                    near_plane: 0.1,
                    far_plane: 100.0,
                    // Only the last camera is primary, so the resolve walk scans past decoys.
                    primary: k == CAMERA_COUNT - 1,
                },
            )
            .unwrap();
    }

    relink_hierarchy(&mut scene);
    (scene, bone_handles)
}

/// Seeds a non-trivial local transform so the matrix math is not optimized to identity.
fn seed_transform(scene: &mut Scene, e: Entity, seed: usize) {
    let f = seed as f32;
    scene
        .add_component(
            e,
            Transform {
                translation: Vec3::new(f * 0.1, f * 0.2, f * 0.3),
                rotation: Vec3::new(0.01 * f, 0.02 * f, 0.03 * f),
                scale: Vec3::splat(1.0),
            },
        )
        .unwrap();
}

/// Rebuilds the `parent_handle`/`children` caches from the durable parent uuids — the
/// structural rebuild run after any load / reparent / play-duplicate. O(N): one pass to map
/// uuid→handle, one to clear caches, one to resolve parents, one to fill children.
fn relink_hierarchy(scene: &mut Scene) {
    let mut uuid_to_handle: HashMap<u64, Entity> = HashMap::new();
    scene.for_each::<&IdComponent, _>(|e, id| {
        uuid_to_handle.insert(id.id.0, e);
    });

    scene.for_each::<&mut Relationship, _>(|_, rel| {
        rel.parent_handle = None;
        rel.children.clear();
    });

    let mut parents: Vec<(Entity, Entity)> = Vec::new();
    scene.for_each::<&Relationship, _>(|e, rel| {
        if rel.parent != 0
            && let Some(&handle) = uuid_to_handle.get(&rel.parent)
            && handle != e
        {
            parents.push((e, handle));
        }
    });
    for &(child, parent) in &parents {
        scene
            .with_component_mut::<Relationship, _>(child, |rel| rel.parent_handle = Some(parent))
            .unwrap();
        scene
            .with_component_mut::<Relationship, _>(parent, |rel| rel.children.push(child))
            .unwrap();
    }
}

/// Writes the cached `WorldTransform` for every transformable entity, roots-first then down
/// the children caches. A `PoseOverride`, when present, supersedes the authored `Transform`
/// for the local matrix. Relies on `relink_hierarchy`-fresh caches.
fn update_world_transforms(scene: &mut Scene) {
    // Gather roots without holding a query borrow across the recursive walk.
    let mut roots: Vec<Entity> = Vec::new();
    scene.for_each::<&Relationship, _>(|e, rel| {
        if rel.parent_handle.is_none() {
            roots.push(e);
        }
    });
    for root in roots {
        write_subtree(scene, root, Mat4::IDENTITY);
    }
}

fn write_subtree(scene: &mut Scene, entity: Entity, parent_world: Mat4) {
    let local = local_matrix(scene, entity);
    let world = parent_world * local;
    if scene.has_component::<WorldTransform>(entity) {
        scene
            .with_component_mut::<WorldTransform, _>(entity, |w| w.matrix = world)
            .unwrap();
    } else {
        scene
            .add_component(entity, WorldTransform { matrix: world })
            .unwrap();
    }
    let children = scene
        .with_component::<Relationship, _>(entity, |rel| rel.children.clone())
        .unwrap_or_default();
    for child in children {
        write_subtree(scene, child, world);
    }
}

/// The entity's effective local matrix: the `PoseOverride` when present (composed from its
/// quaternion directly, no Euler round-trip), else the authored `Transform`.
fn local_matrix(scene: &Scene, entity: Entity) -> Mat4 {
    if let Ok(pose) = scene.component::<PoseOverride>(entity) {
        Mat4::from_translation(pose.translation)
            * Mat4::from_quat(pose.rotation)
            * Mat4::from_scale(pose.scale)
    } else {
        scene
            .with_component::<Transform, _>(entity, Transform::matrix)
            .unwrap_or(Mat4::IDENTITY)
    }
}

/// The resolved primary camera's view matrix, or `None`. Scans `(&Transform, &Camera)` for
/// the first primary one.
fn primary_camera(scene: &mut Scene) -> Option<Mat4> {
    let mut result = None;
    scene.for_each::<(&Transform, &Camera), _>(|_, (transform, camera)| {
        if result.is_none() && camera.primary {
            result = Some(transform.matrix().inverse());
        }
    });
    result
}

/// Serializes every entity's components into a snapshot list. Runtime caches are skipped.
fn scene_to_snapshots(scene: &mut Scene) -> Vec<EntitySnapshot> {
    let mut snaps: HashMap<Entity, EntitySnapshot> = HashMap::new();
    scene.for_each::<&IdComponent, _>(|e, id| {
        snaps.entry(e).or_default().id = id.id.0;
    });
    scene.for_each::<&Relationship, _>(|e, rel| {
        snaps.entry(e).or_default().parent = rel.parent;
    });
    scene.for_each::<&Transform, _>(|e, t| {
        snaps.entry(e).or_default().transform = Some(TransformDto {
            translation: t.translation.to_array(),
            rotation: t.rotation.to_array(),
            scale: t.scale.to_array(),
        });
    });
    scene.for_each::<&Mesh, _>(|e, m| {
        snaps.entry(e).or_default().mesh = Some(m.mesh);
    });
    scene.for_each::<&Material, _>(|e, m| {
        snaps.entry(e).or_default().material = Some(MaterialDto {
            base_color: m.base_color.to_array(),
            albedo_texture: m.albedo_texture,
            metallic: m.metallic,
            roughness: m.roughness,
            emissive: m.emissive.to_array(),
        });
    });
    scene.for_each::<&Camera, _>(|e, c| {
        snaps.entry(e).or_default().camera = Some(CameraDto {
            fov: c.fov,
            near_plane: c.near_plane,
            far_plane: c.far_plane,
            primary: c.primary,
        });
    });
    scene.for_each::<&PointLight, _>(|e, l| {
        snaps.entry(e).or_default().point_light = Some(PointLightDto {
            color: l.color.to_array(),
            intensity: l.intensity,
            range: l.range,
        });
    });
    scene.for_each::<&Bone, _>(|e, _| {
        snaps.entry(e).or_default().bone = true;
    });
    snaps.into_values().collect()
}

/// Rebuilds a fresh scene from the snapshot list: spawn an entity per row, re-add each
/// component, then `relink_hierarchy`.
fn snapshots_to_scene(snaps: &[EntitySnapshot]) -> Scene {
    let mut scene = Scene::new();
    for snap in snaps {
        let e = scene.spawn_with_id(Uuid(snap.id));
        scene
            .add_component(
                e,
                Relationship {
                    parent: snap.parent,
                    ..Relationship::default()
                },
            )
            .unwrap();
        if let Some(t) = &snap.transform {
            scene
                .add_component(
                    e,
                    Transform {
                        translation: Vec3::from_array(t.translation),
                        rotation: Vec3::from_array(t.rotation),
                        scale: Vec3::from_array(t.scale),
                    },
                )
                .unwrap();
        }
        if let Some(m) = snap.mesh {
            scene.add_component(e, Mesh { mesh: m }).unwrap();
        }
        if let Some(m) = &snap.material {
            scene
                .add_component(
                    e,
                    Material {
                        base_color: Vec4::from_array(m.base_color),
                        albedo_texture: m.albedo_texture,
                        metallic: m.metallic,
                        roughness: m.roughness,
                        emissive: Vec3::from_array(m.emissive),
                    },
                )
                .unwrap();
        }
        if let Some(c) = &snap.camera {
            scene
                .add_component(
                    e,
                    Camera {
                        fov: c.fov,
                        near_plane: c.near_plane,
                        far_plane: c.far_plane,
                        primary: c.primary,
                    },
                )
                .unwrap();
        }
        if let Some(l) = &snap.point_light {
            scene
                .add_component(
                    e,
                    PointLight {
                        color: Vec3::from_array(l.color),
                        intensity: l.intensity,
                        range: l.range,
                    },
                )
                .unwrap();
        }
        if snap.bone {
            scene.add_component(e, Bone).unwrap();
        }
    }
    relink_hierarchy(&mut scene);
    scene
}

/// The `enter_play` duplicate: serialize the authored scene to JSON, parse it back, and
/// rebuild a fresh play scene.
fn enter_play(scene: &mut Scene) -> Scene {
    let snaps = scene_to_snapshots(scene);
    let json = serde_json::to_string(&snaps).unwrap();
    let parsed: Vec<EntitySnapshot> = serde_json::from_str(&json).unwrap();
    snapshots_to_scene(&parsed)
}

/// One frame of pose-override churn: `add_component` (emplace-or-replace) a `PoseOverride`
/// onto every animated bone, then `remove_component` it — the only component added/removed
/// per frame, and the decisive archetype-move measurement.
fn pose_override_churn(scene: &mut Scene, bones: &[Entity]) {
    for &bone in bones {
        scene
            .add_component(
                bone,
                PoseOverride {
                    translation: Vec3::ZERO,
                    rotation: Quat::IDENTITY,
                    scale: Vec3::ONE,
                },
            )
            .unwrap();
    }
    for &bone in bones {
        scene.remove_component::<PoseOverride>(bone);
    }
}

fn bench_per_frame(crit: &mut Criterion) {
    let mut group = crit.benchmark_group("per_frame");
    let (mut scene, _bones) = build_scene();
    let entity_count = scene.len();
    group.throughput(criterion::Throughput::Elements(entity_count as u64));

    group.bench_function("transform_sync", |b| {
        b.iter(|| update_world_transforms(black_box(&mut scene)));
    });

    group.bench_function("draw_enumeration", |b| {
        b.iter(|| {
            let mut count = 0u64;
            scene.for_each::<(&Mesh, &Material), _>(|_, (mesh, mat)| {
                count = count.wrapping_add(mesh.mesh);
                count = count.wrapping_add(mat.albedo_texture);
            });
            black_box(count)
        });
    });

    group.bench_function("light_gather", |b| {
        b.iter(|| {
            let mut sum = 0.0f32;
            scene.for_each::<&PointLight, _>(|_, light| {
                sum += light.intensity + light.range;
            });
            black_box(sum)
        });
    });

    group.bench_function("camera_resolve", |b| {
        b.iter(|| black_box(primary_camera(black_box(&mut scene))));
    });

    group.finish();
}

fn bench_structural(crit: &mut Criterion) {
    let mut group = crit.benchmark_group("structural");

    group.bench_function("enter_play", |b| {
        let (mut scene, _bones) = build_scene();
        b.iter(|| black_box(enter_play(black_box(&mut scene))));
    });

    group.bench_function("relink_hierarchy", |b| {
        let (mut scene, _bones) = build_scene();
        b.iter(|| relink_hierarchy(black_box(&mut scene)));
    });

    // Report throughput against the animated-bone count, so the rate reads per-bone-per-frame.
    group.bench_function("pose_override_churn", |b| {
        let (mut scene, bones) = build_scene();
        b.iter(|| pose_override_churn(black_box(&mut scene), black_box(&bones)));
    });

    group.finish();
}

criterion_group!(benches, bench_per_frame, bench_structural);
criterion_main!(benches);
