+++
title = 'Transforms'
weight = 3
math = true
+++

# Transforms

A `Transform` stores an entity's placement relative to its parent. Anima composes that local translation, rotation, and scale into a matrix, then walks the [scene hierarchy](../scene-hierarchy/) to produce the world matrices used by rendering, physics synchronization, picking, cameras, and editor overlays.

Keeping authored and derived state separate lets a parent move an entire subtree without rewriting each child's local values.

## Local TRS

`Transform` contains `translation: Vec3`, `rotation: Vec3`, and `scale: Vec3`. Rotation uses XYZ [Euler angles](https://en.wikipedia.org/wiki/Euler_angles) in radians; the editor converts only this field to degrees for display.

`transform_matrix` uses translation-rotation-scale order:

$$
M_{local} = T \cdot R \cdot S
$$

Because matrix multiplication applies right to left, a point is scaled, rotated, and then translated.

```rust
pub fn transform_matrix(transform: &Transform) -> Mat4 {
    Mat4::from_translation(transform.translation)
        * Mat4::from_quat(quat_from_euler_xyz(transform.rotation))
        * Mat4::from_scale(transform.scale)
}
```

`quat_from_euler_xyz` defines the engine's Euler-to-quaternion convention with half-angle products. Animation rest poses and matrix composition call this one function, so they cannot disagree about rotation order.

## World composition

`Scene::update_world_transforms` starts at hierarchy roots and recursively computes:

$$
M_{world} = M_{parent} \cdot M_{local}
$$

For example, a parent translated to `(10, 0, 0)` and a child translated locally to `(0, 2, 0)` place the child at `(10, 2, 0)` before rotation or scale changes the result.

Each transformable entity receives a runtime-only `WorldTransform`. Full `Mat4` multiplication preserves non-uniform parent scale; render code derives the corresponding inverse-transpose normal matrix from the final world matrix.

Animation can attach a `PoseOverride`. `Scene::local_matrix` prefers that evaluated translation, quaternion, and scale while it is present, leaving the authored `Transform` unchanged for edit previews and later playback resets.

## Reparenting

`Scene::set_parent` stores the new parent's stable `Uuid`, rejects cycles, and rebuilds the hierarchy caches. Editor reparenting uses `keep_world: true`, which computes a replacement local matrix:

$$
M'_{local} = M^{-1}_{new\ parent} \cdot M_{world}
$$

`set_local_from_matrix` decomposes this matrix back into translation, quaternion, and scale, then converts the quaternion with `quat_to_euler_zyx`. A `Transform` has no shear field, so decomposition discards shear from a source matrix that contains it.

The custom quaternion-to-Euler extraction remains stable at the middle-axis gimbal pole. At that pole it may return a different Euler triple that represents the same rotation matrix, which is sufficient for preserving world placement.

## Camera view

A `Camera` component contains projection settings, not a second transform. `Scene::primary_camera` finds the first primary camera and inverts its world matrix:

```rust
CameraView {
    view: scene.world_matrix(entity).inverse(),
    fov: camera.fov,
    near_plane: camera.near_plane,
    far_plane: camera.far_plane,
}
```

A parented camera therefore inherits its ancestors' placement. The camera's vertical field of view is stored in degrees and converted to radians when `camera_projection` builds the perspective matrix.

## Projection convention

`camera_projection` returns an unflipped right-handed GL-clip projection through `Mat4::perspective_rh_gl`. The renderer and [picking](../picking/) negate `proj.y_axis.y` when they build a Vulkan-facing view-projection matrix. The editor gizmo consumes the unflipped projection, preventing a mirrored control.

This division keeps camera projection parameters in one place while each consumer applies the coordinate-system adaptation it requires.

## Source map

| What | File | Symbols |
|---|---|---|
| Authored and cached transform data | `engine/crates/scene/src/component.rs` | `Transform`, `WorldTransform`, `PoseOverride` |
| TRS and Euler conversion | `engine/crates/scene/src/hierarchy.rs` | `transform_matrix`, `quat_from_euler_xyz`, `quat_to_euler_zyx` |
| Hierarchy composition and reparenting | `engine/crates/scene/src/hierarchy.rs` | `Scene::update_world_transforms`, `Scene::set_parent`, `Scene::set_local_from_matrix` |
| Camera view and projection | `engine/crates/scene/src/hierarchy.rs` | `Scene::primary_camera`, `CameraView`, `camera_projection` |
| Vulkan Y flip | `engine/crates/assets/src/render_scene.rs` | `render_scene`, `viewport_ray` |
| Degree/radian editor conversion | `editor/src/components/fieldRenderer.tsx` | `Transform.rotation`, `RAD_TO_DEG`, `DEG_TO_RAD` |

## Related

- [Components](../built-in-components/)
- [Scene hierarchy](../scene-hierarchy/)
- [Picking](../picking/)
- [Inspector](../../ui-and-editor/inspector/)
