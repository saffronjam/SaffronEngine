//! The gizmo op/space, the overlay gizmo's hover/drag state, and the pure-math hit-test,
//! projection, and drag it runs on.
//!
//! [`GizmoOp`] / [`GizmoSpace`] are the canonical op and reference space, shared by the control TU
//! and the native overlay. [`NativeGizmoState`]'s `mode`/`space` are a per-frame *mirror* of those:
//! never set directly, only synced by [`SceneEditContext::sync_native_gizmo`].

use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};

use saffron_scene::{
    CameraView, Entity, Relationship, Transform, camera_projection, quat_from_euler_xyz,
    quat_to_euler_zyx, transform_matrix,
};

use crate::context::SceneEditContext;

/// The gizmo operation: which transform channel a drag edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GizmoOp {
    /// Translate the selection.
    #[default]
    Translate,
    /// Rotate the selection.
    Rotate,
    /// Scale the selection.
    Scale,
}

impl GizmoOp {
    /// The control-plane name (`"translate"` / `"rotate"` / `"scale"`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            GizmoOp::Translate => "translate",
            GizmoOp::Rotate => "rotate",
            GizmoOp::Scale => "scale",
        }
    }

    /// The op for a control-plane name, defaulting to [`GizmoOp::Translate`] on any
    /// unknown spelling.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "rotate" => GizmoOp::Rotate,
            "scale" => GizmoOp::Scale,
            _ => GizmoOp::Translate,
        }
    }
}

/// The gizmo reference space: world axes or the selection's local axes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GizmoSpace {
    /// World-aligned axes.
    #[default]
    World,
    /// The selection's local-rotated axes.
    Local,
}

impl GizmoSpace {
    /// The control-plane name (`"world"` / `"local"`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            GizmoSpace::World => "world",
            GizmoSpace::Local => "local",
        }
    }

    /// The space for a control-plane name, defaulting to [`GizmoSpace::World`] on any
    /// unknown spelling.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "local" => GizmoSpace::Local,
            _ => GizmoSpace::World,
        }
    }
}

/// The overlay gizmo's operation mirror, driven from [`GizmoOp`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NativeGizmoMode {
    /// Translate handles.
    #[default]
    Translate,
    /// Rotate rings.
    Rotate,
    /// Scale handles.
    Scale,
}

/// The overlay gizmo's space mirror, driven from [`GizmoSpace`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NativeGizmoSpace {
    /// World-aligned handles.
    #[default]
    World,
    /// Local-rotated handles.
    Local,
}

/// A gizmo handle the pointer can hover or drag.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NativeGizmoHandle {
    /// No handle under the pointer.
    #[default]
    None,
    /// The X axis.
    X,
    /// The Y axis.
    Y,
    /// The Z axis.
    Z,
    /// The XY plane.
    Xy,
    /// The YZ plane.
    Yz,
    /// The XZ plane.
    Xz,
    /// The screen-space plane.
    Screen,
    /// The uniform-scale handle.
    Uniform,
}

/// The engine-rendered (overlay) gizmo's hover/drag interaction state.
///
/// `mode`/`space` are mirrored from the backend-neutral [`GizmoOp`]/[`GizmoSpace`] each
/// frame (the single source of truth); the remaining fields are the overlay's own
/// hover/drag interaction state, written by the gizmo math.
#[derive(Clone, Debug)]
pub struct NativeGizmoState {
    /// The operation mirror (synced from [`GizmoOp`]).
    pub mode: NativeGizmoMode,
    /// The space mirror (synced from [`GizmoSpace`]).
    pub space: NativeGizmoSpace,
    /// The handle currently under the pointer.
    pub hovered: NativeGizmoHandle,
    /// The handle being dragged.
    pub active: NativeGizmoHandle,
    /// Whether a drag is in progress.
    pub dragging: bool,
    /// The pointer position at drag begin (viewport pixels).
    pub start_mouse: Vec2,
    /// The latest raw pointer sample (viewport pixels).
    pub drag_target: Vec2,
    /// The per-frame smoothed pointer the drag math consumes.
    pub drag_smoothed: Vec2,
    /// Whether a command-driven drag is pending (smooth + apply each frame).
    pub drag_pending: bool,
    /// The world translation at drag begin.
    pub start_translation: Vec3,
    /// The world rotation (Euler) at drag begin.
    pub start_rotation: Vec3,
    /// The local scale at drag begin (scale never rebases).
    pub start_scale: Vec3,
    /// The frozen parent world for the whole drag.
    pub start_parent_world: Mat4,
    /// The dragged entity.
    pub target: Entity,
    /// Direct-child worlds frozen at drag begin (filled only with `preserve_children`);
    /// each applied drag frame rebases these locals so the children hold their pose.
    pub start_child_worlds: Vec<(Entity, Mat4)>,
}

impl Default for NativeGizmoState {
    fn default() -> Self {
        Self {
            mode: NativeGizmoMode::default(),
            space: NativeGizmoSpace::default(),
            hovered: NativeGizmoHandle::default(),
            active: NativeGizmoHandle::default(),
            dragging: false,
            start_mouse: Vec2::ZERO,
            drag_target: Vec2::ZERO,
            drag_smoothed: Vec2::ZERO,
            drag_pending: false,
            start_translation: Vec3::ZERO,
            start_rotation: Vec3::ZERO,
            start_scale: Vec3::ONE,
            start_parent_world: Mat4::IDENTITY,
            target: Entity::NULL,
            start_child_worlds: Vec::new(),
        }
    }
}

/// A world point projected to the viewport: pixel + NDC + whether it is on-screen.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GizmoProjection {
    /// The viewport pixel (top-left origin).
    pub pixel: Vec2,
    /// The clip-space NDC (`x`/`y` in `[-1, 1]`).
    pub ndc: Vec2,
    /// Whether the point is on-screen (in front of the camera, inside the depth range).
    pub visible: bool,
}

/// The drag-begin world-up axis, shared by the rotation-ring basis fallback.
const WORLD_UP: Vec3 = Vec3::Y;

/// Projects a world point through the camera to viewport pixels (top-left origin) + NDC.
///
/// Returns an invisible projection for a zero-area viewport, a point on or behind the near
/// plane (`|w|` near zero), or a point outside the `[0, 1]` depth range.
#[must_use]
pub fn viewport_project(cam: &CameraView, width: u32, height: u32, world: Vec3) -> GizmoProjection {
    if width == 0 || height == 0 {
        return GizmoProjection::default();
    }
    let proj = camera_projection(cam, width as f32 / height as f32);
    let clip = proj * cam.view * world.extend(1.0);
    if clip.w.abs() < 0.0001 {
        return GizmoProjection::default();
    }
    let ndc3 = clip.truncate() / clip.w;
    if ndc3.z < 0.0 || ndc3.z > 1.0 {
        return GizmoProjection::default();
    }
    GizmoProjection {
        pixel: Vec2::new(
            (ndc3.x * 0.5 + 0.5) * width as f32,
            (1.0 - (ndc3.y * 0.5 + 0.5)) * height as f32,
        ),
        ndc: Vec2::new(ndc3.x, ndc3.y),
        visible: true,
    }
}

/// A viewport pixel (top-left origin) to clip-space NDC.
#[must_use]
pub fn pixel_to_ndc(p: Vec2, width: u32, height: u32) -> Vec2 {
    Vec2::new(
        p.x / width as f32 * 2.0 - 1.0,
        p.y / height as f32 * 2.0 - 1.0,
    )
}

/// The camera's world position from its view matrix.
#[must_use]
pub fn camera_position(cam: &CameraView) -> Vec3 {
    cam.view.inverse().w_axis.truncate()
}

/// Distance from `p` to the segment `[a, b]`, all in pixels.
#[must_use]
pub fn point_segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let denom = ab.dot(ab);
    if denom < 0.0001 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / denom).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Whether `p` lies inside the convex quad `quad`.
///
/// An off-screen corner fails the test outright (a partially clipped plane handle is not
/// hit-tested), matching the overlay draw which only fills a fully-visible quad.
#[must_use]
fn point_in_convex_quad(p: Vec2, quad: &[GizmoProjection; 4]) -> bool {
    if quad.iter().any(|q| !q.visible) {
        return false;
    }
    let edge = |a: Vec2, b: Vec2, c: Vec2| -> f32 {
        let ab = b - a;
        let ac = c - a;
        ab.x * ac.y - ab.y * ac.x
    };
    let mut has_neg = false;
    let mut has_pos = false;
    for i in 0..4 {
        let e = edge(quad[i].pixel, quad[(i + 1) % 4].pixel, p);
        has_neg = has_neg || e < 0.0;
        has_pos = has_pos || e > 0.0;
    }
    !(has_neg && has_pos)
}

/// An orthonormal basis spanning the plane perpendicular to `n` (the rotation-ring plane),
/// NaN-safe for any axis including world up.
///
/// The raw `cross` is tested *before* normalizing: normalizing a near-zero vector first
/// would yield NaN, and `NaN < epsilon` is false, so the world-up fallback would never
/// trigger — the load-bearing numeric edge case.
#[must_use]
pub fn ring_basis(n: Vec3) -> (Vec3, Vec3) {
    let mut a = n.cross(WORLD_UP);
    if a.dot(a) < 0.0001 {
        a = n.cross(Vec3::X);
    }
    let a = a.normalize();
    (a, n.cross(a).normalize())
}

/// Walks the rotation ring of each axis at `radius`, returning the nearest handle within
/// the pixel threshold.
fn hit_rotate_ring(
    cam: &CameraView,
    width: u32,
    height: u32,
    mouse: Vec2,
    origin: Vec3,
    axes: &[Vec3; 3],
    radius: f32,
) -> NativeGizmoHandle {
    const SEGMENTS: u32 = 96;
    let handles = [
        NativeGizmoHandle::X,
        NativeGizmoHandle::Y,
        NativeGizmoHandle::Z,
    ];
    let mut hit = NativeGizmoHandle::None;
    let mut best = 9.0_f32;
    for axis in 0..3 {
        let (a, b) = ring_basis(axes[axis]);
        let mut prev = GizmoProjection::default();
        for i in 0..=SEGMENTS {
            let t = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let cur = viewport_project(
                cam,
                width,
                height,
                origin + (a * t.cos() + b * t.sin()) * radius,
            );
            if i > 0 && prev.visible && cur.visible {
                let d = point_segment_distance(mouse, prev.pixel, cur.pixel);
                if d < best {
                    best = d;
                    hit = handles[axis];
                }
            }
            prev = cur;
        }
    }
    hit
}

/// The display color for a gizmo handle: axis-tinted, highlighted gold when hovered or
/// active. Consumed by the overlay draw in the host.
#[must_use]
pub fn axis_color(handle: NativeGizmoHandle, gizmo: &NativeGizmoState) -> Vec4 {
    if gizmo.active == handle
        || (gizmo.active == NativeGizmoHandle::None && gizmo.hovered == handle)
    {
        return Vec4::new(1.0, 0.82, 0.18, 1.0);
    }
    match handle {
        NativeGizmoHandle::X => Vec4::new(0.93, 0.18, 0.20, 1.0),
        NativeGizmoHandle::Y => Vec4::new(0.20, 0.82, 0.25, 1.0),
        NativeGizmoHandle::Z => Vec4::new(0.22, 0.42, 0.98, 1.0),
        _ => Vec4::new(0.93, 0.93, 0.95, 0.75),
    }
}

/// The gizmo's X/Y/Z basis: world identity in World space, or the entity's world-rotated
/// basis in Local space.
#[must_use]
pub fn gizmo_axes(world_rotation: Quat, space: NativeGizmoSpace) -> [Vec3; 3] {
    if space == NativeGizmoSpace::World {
        return [Vec3::X, Vec3::Y, Vec3::Z];
    }
    [
        world_rotation * Vec3::X,
        world_rotation * Vec3::Y,
        world_rotation * Vec3::Z,
    ]
}

/// The world-space axis for a single-axis handle, zero for plane / screen / uniform handles.
#[must_use]
pub fn handle_axis(handle: NativeGizmoHandle, axes: &[Vec3; 3]) -> Vec3 {
    match handle {
        NativeGizmoHandle::X => axes[0],
        NativeGizmoHandle::Y => axes[1],
        NativeGizmoHandle::Z => axes[2],
        _ => Vec3::ZERO,
    }
}

/// The projected corners of a two-axis translate plane handle (the `axes` pair
/// `(first, second)`), shared by the overlay drawing and the hit-test so they always agree.
#[must_use]
pub fn gizmo_plane_corners(
    cam: &CameraView,
    width: u32,
    height: u32,
    position: Vec3,
    axes: &[Vec3; 3],
    axis_len: f32,
    pair: (usize, usize),
) -> [GizmoProjection; 4] {
    const QUAD_MIN: f32 = 0.545;
    const QUAD_MAX: f32 = 0.755;
    let (first, second) = pair;
    [
        viewport_project(
            cam,
            width,
            height,
            position + axes[first] * axis_len * QUAD_MIN + axes[second] * axis_len * QUAD_MIN,
        ),
        viewport_project(
            cam,
            width,
            height,
            position + axes[first] * axis_len * QUAD_MIN + axes[second] * axis_len * QUAD_MAX,
        ),
        viewport_project(
            cam,
            width,
            height,
            position + axes[first] * axis_len * QUAD_MAX + axes[second] * axis_len * QUAD_MAX,
        ),
        viewport_project(
            cam,
            width,
            height,
            position + axes[first] * axis_len * QUAD_MAX + axes[second] * axis_len * QUAD_MIN,
        ),
    ]
}

/// The rotation with its scale divided out of a world matrix.
///
/// Used to peel a non-root's frozen parent rotation off the world drag result.
fn rotation_of(m: Mat4) -> Quat {
    let mut scale = Vec3::new(
        m.x_axis.truncate().length(),
        m.y_axis.truncate().length(),
        m.z_axis.truncate().length(),
    );
    scale = scale.max(Vec3::splat(1e-8));
    let rotation = Mat3::from_cols(
        m.x_axis.truncate() / scale.x,
        m.y_axis.truncate() / scale.y,
        m.z_axis.truncate() / scale.z,
    );
    Quat::from_mat3(&rotation)
}

impl SceneEditContext {
    /// Mirrors the backend-neutral [`GizmoOp`] / [`GizmoSpace`] (the single source) onto the
    /// overlay's `native_gizmo.mode` / `.space`.
    ///
    /// Nothing else writes the mirror; call this each frame so the overlay tracks the source.
    pub fn sync_native_gizmo(&mut self) {
        self.native_gizmo.mode = match self.gizmo_op {
            GizmoOp::Rotate => NativeGizmoMode::Rotate,
            GizmoOp::Scale => NativeGizmoMode::Scale,
            GizmoOp::Translate => NativeGizmoMode::Translate,
        };
        self.native_gizmo.space = if self.gizmo_space == GizmoSpace::Local {
            NativeGizmoSpace::Local
        } else {
            NativeGizmoSpace::World
        };
    }

    /// Hit-tests the selected entity's gizmo at `mouse` (viewport pixels) for the active
    /// mode/space, returning the handle under the pointer.
    ///
    /// Returns [`NativeGizmoHandle::None`] when nothing is selected, the selection has no
    /// [`Transform`], or its origin is off-screen.
    #[must_use]
    pub fn hit_native_gizmo(
        &self,
        cam: &CameraView,
        width: u32,
        height: u32,
        mouse: Vec2,
    ) -> NativeGizmoHandle {
        if self.selected == Entity::NULL || !self.scene.has_component::<Transform>(self.selected) {
            return NativeGizmoHandle::None;
        }
        let position = self.scene.world_translation(self.selected);
        let origin = viewport_project(cam, width, height, position);
        if !origin.visible {
            return NativeGizmoHandle::None;
        }
        let axes = gizmo_axes(
            self.scene.world_rotation(self.selected),
            self.native_gizmo.space,
        );
        let distance = (camera_position(cam) - position).length();
        let axis_len = (distance * 0.22).max(0.75);
        let handles = [
            NativeGizmoHandle::X,
            NativeGizmoHandle::Y,
            NativeGizmoHandle::Z,
        ];

        if self.native_gizmo.mode == NativeGizmoMode::Rotate {
            return hit_rotate_ring(cam, width, height, mouse, position, &axes, axis_len * 0.72);
        }

        if self.native_gizmo.mode == NativeGizmoMode::Scale
            && (mouse - origin.pixel).length() < 12.0
        {
            return NativeGizmoHandle::Uniform;
        }

        for i in 0..3 {
            let end = viewport_project(cam, width, height, position + axes[i] * axis_len);
            if !end.visible {
                continue;
            }
            if point_segment_distance(mouse, origin.pixel, end.pixel) < 9.0 {
                return handles[i];
            }
        }

        if self.native_gizmo.mode == NativeGizmoMode::Translate {
            let planes = [
                (NativeGizmoHandle::Yz, (1, 2)),
                (NativeGizmoHandle::Xz, (0, 2)),
                (NativeGizmoHandle::Xy, (0, 1)),
            ];
            for (handle, pair) in planes {
                let corners =
                    gizmo_plane_corners(cam, width, height, position, &axes, axis_len, pair);
                if point_in_convex_quad(mouse, &corners) {
                    return handle;
                }
            }
        }
        NativeGizmoHandle::None
    }

    /// The resolved parent handle of `entity`, or [`None`] for a root / an entity with no
    /// [`Relationship`].
    fn parent_of(&self, entity: Entity) -> Option<Entity> {
        self.scene
            .with_component::<Relationship, _>(entity, |rel| rel.parent_handle)
            .unwrap_or(None)
    }

    /// With `preserve_children`, holds each direct child at its drag-begin world pose by
    /// rebasing its local against the target's freshly written transform.
    fn rebase_preserved_children(&mut self) {
        if self.native_gizmo.start_child_worlds.is_empty() {
            return;
        }
        let target = self.native_gizmo.target;
        let local = self
            .scene
            .with_component::<Transform, _>(target, transform_matrix)
            .unwrap_or(Mat4::IDENTITY);
        let target_world = self.native_gizmo.start_parent_world * local;
        let inv_target_world = target_world.inverse();
        let children = std::mem::take(&mut self.native_gizmo.start_child_worlds);
        for &(child, world) in &children {
            if !self.scene.valid(child) || !self.scene.has_component::<Transform>(child) {
                continue;
            }
            self.scene
                .set_local_from_matrix(child, inv_target_world * world);
        }
        self.native_gizmo.start_child_worlds = children;
    }

    /// Captures the drag-begin state of `target` (world translation/rotation, local scale,
    /// frozen parent world, and — with `preserve_children` — direct-child worlds), the one
    /// snapshot both the SDL and control gizmo-pointer paths share.
    ///
    /// A root keeps the raw authored Euler so rotate-drag continuity survives angles a
    /// matrix extraction would wrap; a non-root stores the world rotation as a ZYX Euler.
    pub fn snapshot_native_gizmo_start(&mut self, target: Entity) {
        let transform = match self.scene.component::<Transform>(target) {
            Ok(t) => t,
            Err(_) => return,
        };
        self.native_gizmo.start_scale = transform.scale;
        self.native_gizmo.start_parent_world = Mat4::IDENTITY;
        self.native_gizmo.start_child_worlds.clear();
        if self.preserve_children && self.scene.has_component::<Relationship>(target) {
            let children = self
                .scene
                .with_component::<Relationship, _>(target, |rel| rel.children.clone())
                .unwrap_or_default();
            for child in children {
                if self.scene.has_component::<Transform>(child) {
                    let world = self.scene.compose_world_matrix(child);
                    self.native_gizmo.start_child_worlds.push((child, world));
                }
            }
        }
        match self.parent_of(target) {
            None => {
                self.native_gizmo.start_translation = transform.translation;
                self.native_gizmo.start_rotation = transform.rotation;
            }
            Some(parent) => {
                self.native_gizmo.start_parent_world = self.scene.compose_world_matrix(parent);
                self.native_gizmo.start_translation = self.scene.world_translation(target);
                self.native_gizmo.start_rotation =
                    quat_to_euler_zyx(self.scene.world_rotation(target));
            }
        }
    }

    /// Applies an in-progress gizmo drag at `mouse` (viewport pixels), writing the dragged
    /// entity's [`Transform`].
    ///
    /// The drag math runs in world space then rebases the result into the parent frame
    /// (identity for a root). It bumps `scene_version` past the guard so the control poll
    /// re-inspects the drag live. A no-op unless a drag is active on a transformable target.
    pub fn apply_native_gizmo_drag(
        &mut self,
        cam: &CameraView,
        width: u32,
        height: u32,
        mouse: Vec2,
    ) {
        let target = self.native_gizmo.target;
        if !self.native_gizmo.dragging
            || target == Entity::NULL
            || !self.scene.has_component::<Transform>(target)
        {
            return;
        }
        self.scene_version += 1;

        let axes = gizmo_axes(self.scene.world_rotation(target), self.native_gizmo.space);
        let delta = mouse - self.native_gizmo.start_mouse;
        let start_translation = self.native_gizmo.start_translation;
        let distance = (camera_position(cam) - start_translation).length();
        let units_per_pixel = (2.0 * distance * (cam.fov.to_radians() * 0.5).tan()
            / (height as f32).max(1.0))
        .max(0.001);

        let projected_axis = |axis: Vec3| -> Vec2 {
            let a = viewport_project(cam, width, height, start_translation);
            let b = viewport_project(cam, width, height, start_translation + axis);
            let d = b.pixel - a.pixel;
            let len = d.length();
            if len < 0.001 {
                return Vec2::new(1.0, 0.0);
            }
            d / len
        };

        let mode = self.native_gizmo.mode;
        let active = self.native_gizmo.active;

        if mode == NativeGizmoMode::Translate {
            let mut move_world = Vec3::ZERO;
            if active == NativeGizmoHandle::Xy
                || active == NativeGizmoHandle::Xz
                || active == NativeGizmoHandle::Yz
            {
                let use_x = active != NativeGizmoHandle::Yz;
                let use_y = active != NativeGizmoHandle::Xz;
                let use_z = active != NativeGizmoHandle::Xy;
                if use_x {
                    move_world += axes[0] * delta.dot(projected_axis(axes[0])) * units_per_pixel;
                }
                if use_y {
                    move_world += axes[1] * delta.dot(projected_axis(axes[1])) * units_per_pixel;
                }
                if use_z {
                    move_world += axes[2] * delta.dot(projected_axis(axes[2])) * units_per_pixel;
                }
            } else {
                let axis = handle_axis(active, &axes);
                move_world = axis * delta.dot(projected_axis(axis)) * units_per_pixel;
            }
            let local = (self.native_gizmo.start_parent_world.inverse()
                * (start_translation + move_world).extend(1.0))
            .truncate();
            let _ = self
                .scene
                .with_component_mut::<Transform, _>(target, |t| t.translation = local);
            self.rebase_preserved_children();
            return;
        }

        if mode == NativeGizmoMode::Rotate {
            let radians = (delta.x + delta.y) * 0.01;
            let mut world_euler = self.native_gizmo.start_rotation;
            if active == NativeGizmoHandle::X {
                world_euler += Vec3::new(radians, 0.0, 0.0);
            }
            if active == NativeGizmoHandle::Y {
                world_euler += Vec3::new(0.0, radians, 0.0);
            }
            if active == NativeGizmoHandle::Z {
                world_euler += Vec3::new(0.0, 0.0, radians);
            }
            match self.parent_of(target) {
                None => {
                    let _ = self
                        .scene
                        .with_component_mut::<Transform, _>(target, |t| t.rotation = world_euler);
                }
                Some(_) => {
                    let local_rot = rotation_of(self.native_gizmo.start_parent_world).inverse()
                        * quat_from_euler_xyz(world_euler);
                    let euler = quat_to_euler_zyx(local_rot);
                    let _ = self
                        .scene
                        .with_component_mut::<Transform, _>(target, |t| t.rotation = euler);
                }
            }
            self.rebase_preserved_children();
            return;
        }

        let scale_delta = delta.dot(projected_axis(handle_axis(active, &axes))) * 0.01;
        let factor = (1.0 + scale_delta).max(0.05);
        let start_scale = self.native_gizmo.start_scale;
        let new_scale = match active {
            NativeGizmoHandle::Uniform => {
                let uniform = (1.0 + (delta.x - delta.y) * 0.01).max(0.05);
                Some(start_scale * uniform)
            }
            NativeGizmoHandle::X => Some(start_scale * Vec3::new(factor, 1.0, 1.0)),
            NativeGizmoHandle::Y => Some(start_scale * Vec3::new(1.0, factor, 1.0)),
            NativeGizmoHandle::Z => Some(start_scale * Vec3::new(1.0, 1.0, factor)),
            _ => None,
        };
        if let Some(scale) = new_scale {
            let _ = self
                .scene
                .with_component_mut::<Transform, _>(target, |t| t.scale = scale);
        }
        self.rebase_preserved_children();
    }

    /// Advances a command-driven drag each rendered frame: exponentially smooths the raw
    /// pointer toward `drag_target` (`tau = 0.025`) and applies the drag, so ~60 Hz control
    /// samples render fluidly.
    ///
    /// A no-op unless a gizmo-pointer drag sample is pending.
    pub fn step_native_gizmo_drag(&mut self, cam: &CameraView, width: u32, height: u32, dt: f32) {
        if !self.native_gizmo.dragging || !self.native_gizmo.drag_pending {
            return;
        }
        let alpha = 1.0 - (-dt.max(0.0) / crate::smoothing::SMOOTH_TAU).exp();
        let target = self.native_gizmo.drag_target;
        self.native_gizmo.drag_smoothed += (target - self.native_gizmo.drag_smoothed) * alpha;
        let smoothed = self.native_gizmo.drag_smoothed;
        self.apply_native_gizmo_drag(cam, width, height, smoothed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gizmo_op_name_round_trips() {
        for op in [GizmoOp::Translate, GizmoOp::Rotate, GizmoOp::Scale] {
            assert_eq!(GizmoOp::from_name(op.name()), op);
        }
        // Unknown spellings fall through to translate.
        assert_eq!(GizmoOp::from_name("nonsense"), GizmoOp::Translate);
        assert_eq!(GizmoOp::from_name(""), GizmoOp::Translate);
    }

    #[test]
    fn gizmo_space_name_round_trips() {
        for space in [GizmoSpace::World, GizmoSpace::Local] {
            assert_eq!(GizmoSpace::from_name(space.name()), space);
        }
        assert_eq!(GizmoSpace::from_name("nonsense"), GizmoSpace::World);
    }

    /// A camera at `eye` looking at the origin, the framing the gizmo tests project against.
    fn test_camera(eye: Vec3) -> CameraView {
        CameraView {
            view: Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y),
            fov: 45.0,
            near_plane: 0.1,
            far_plane: 100.0,
        }
    }

    #[test]
    fn viewport_project_and_pixel_to_ndc_round_trip() {
        let cam = test_camera(Vec3::new(0.0, 0.0, 5.0));
        let (width, height) = (1280, 720);
        // A point in front of the camera projects to a visible pixel...
        let world = Vec3::new(0.5, -0.3, 0.0);
        let proj = viewport_project(&cam, width, height, world);
        assert!(proj.visible, "a point in front of the camera is visible");
        // ...and pixel_to_ndc recovers the projected NDC from the pixel.
        let ndc = pixel_to_ndc(proj.pixel, width, height);
        // pixel_to_ndc gives a top-left-origin NDC: x matches, y is the flip of clip-space y.
        assert!((ndc.x - proj.ndc.x).abs() < 1e-4, "x round-trips: {ndc:?}");
        assert!(
            (ndc.y - (-proj.ndc.y)).abs() < 1e-4,
            "y round-trips through the viewport flip: {ndc:?}"
        );
    }

    #[test]
    fn viewport_project_rejects_offscreen_and_zero_viewport() {
        let cam = test_camera(Vec3::new(0.0, 0.0, 5.0));
        // Behind the camera (past it) is not visible.
        let behind = viewport_project(&cam, 1280, 720, Vec3::new(0.0, 0.0, 100.0));
        assert!(!behind.visible, "a point behind the camera is invisible");
        // A zero-area viewport yields the default (invisible) projection.
        let zero = viewport_project(&cam, 0, 720, Vec3::ZERO);
        assert!(!zero.visible);
        assert_eq!(zero, GizmoProjection::default());
    }

    #[test]
    fn camera_position_recovers_the_eye() {
        let eye = Vec3::new(3.0, 2.5, 4.0);
        let cam = test_camera(eye);
        assert!(
            camera_position(&cam).distance(eye) < 1e-4,
            "the eye is the inverse-view translation"
        );
    }

    #[test]
    fn ring_basis_is_orthonormal_and_nan_free_for_arbitrary_normals() {
        let normals = [
            Vec3::Y,     // world up: the cross-with-up degeneracy the fallback handles
            Vec3::NEG_Y, // and its negation
            Vec3::X,
            Vec3::Z,
            Vec3::new(1.0, 2.0, 3.0).normalize(),
            Vec3::new(-0.3, 0.9, -0.2).normalize(),
        ];
        for n in normals {
            let (a, b) = ring_basis(n);
            assert!(a.is_finite() && b.is_finite(), "no NaNs for {n:?}");
            // Unit length.
            assert!((a.length() - 1.0).abs() < 1e-5, "a is unit for {n:?}");
            assert!((b.length() - 1.0).abs() < 1e-5, "b is unit for {n:?}");
            // Mutually orthogonal and both perpendicular to n.
            assert!(a.dot(b).abs() < 1e-5, "a⊥b for {n:?}");
            assert!(a.dot(n).abs() < 1e-5, "a⊥n for {n:?}");
            assert!(b.dot(n).abs() < 1e-5, "b⊥n for {n:?}");
        }
    }

    #[test]
    fn gizmo_axes_world_is_identity_local_is_rotated() {
        // World space ignores the rotation.
        let world = gizmo_axes(Quat::from_rotation_z(1.0), NativeGizmoSpace::World);
        assert_eq!(world, [Vec3::X, Vec3::Y, Vec3::Z]);
        // Local space rotates the identity basis.
        let q = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let local = gizmo_axes(q, NativeGizmoSpace::Local);
        assert!(local[0].distance(Vec3::Y) < 1e-5, "X→Y under +90° Z");
        assert!(local[1].distance(Vec3::NEG_X) < 1e-5, "Y→-X under +90° Z");
        assert!(local[2].distance(Vec3::Z) < 1e-5, "Z unchanged");
    }

    #[test]
    fn handle_axis_picks_the_single_axis_and_zeroes_the_rest() {
        let axes = [Vec3::X, Vec3::Y, Vec3::Z];
        assert_eq!(handle_axis(NativeGizmoHandle::X, &axes), Vec3::X);
        assert_eq!(handle_axis(NativeGizmoHandle::Y, &axes), Vec3::Y);
        assert_eq!(handle_axis(NativeGizmoHandle::Z, &axes), Vec3::Z);
        assert_eq!(handle_axis(NativeGizmoHandle::Xy, &axes), Vec3::ZERO);
        assert_eq!(handle_axis(NativeGizmoHandle::Uniform, &axes), Vec3::ZERO);
        assert_eq!(handle_axis(NativeGizmoHandle::None, &axes), Vec3::ZERO);
    }

    #[test]
    fn point_segment_distance_matches_geometry() {
        // Perpendicular foot inside the segment.
        let d = point_segment_distance(Vec2::new(1.0, 2.0), Vec2::ZERO, Vec2::new(4.0, 0.0));
        assert!((d - 2.0).abs() < 1e-5);
        // Past the end clamps to the endpoint distance.
        let d = point_segment_distance(Vec2::new(6.0, 0.0), Vec2::ZERO, Vec2::new(4.0, 0.0));
        assert!((d - 2.0).abs() < 1e-5);
        // A degenerate segment is just the point distance.
        let d = point_segment_distance(Vec2::new(3.0, 4.0), Vec2::ZERO, Vec2::ZERO);
        assert!((d - 5.0).abs() < 1e-5);
    }

    #[test]
    fn sync_native_gizmo_mirrors_op_and_space() {
        let mut ctx = SceneEditContext::new();
        for (op, mode) in [
            (GizmoOp::Translate, NativeGizmoMode::Translate),
            (GizmoOp::Rotate, NativeGizmoMode::Rotate),
            (GizmoOp::Scale, NativeGizmoMode::Scale),
        ] {
            ctx.gizmo_op = op;
            ctx.sync_native_gizmo();
            assert_eq!(ctx.native_gizmo.mode, mode);
        }
        ctx.gizmo_space = GizmoSpace::Local;
        ctx.sync_native_gizmo();
        assert_eq!(ctx.native_gizmo.space, NativeGizmoSpace::Local);
        ctx.gizmo_space = GizmoSpace::World;
        ctx.sync_native_gizmo();
        assert_eq!(ctx.native_gizmo.space, NativeGizmoSpace::World);
    }

    /// A context with a single transformable root at the origin selected, dragging the X
    /// handle from screen-center, ready for a drag-apply test.
    fn drag_context() -> (SceneEditContext, CameraView, u32, u32, Entity) {
        let mut ctx = SceneEditContext::default();
        let entity = ctx.scene.create_entity("Target");
        ctx.scene.relink_hierarchy();
        ctx.set_selection(entity);
        let (width, height) = (1280, 720);
        let cam = test_camera(Vec3::new(2.0, 1.5, 6.0));

        ctx.gizmo_op = GizmoOp::Translate;
        ctx.gizmo_space = GizmoSpace::World;
        ctx.sync_native_gizmo();
        ctx.native_gizmo.target = entity;
        ctx.native_gizmo.active = NativeGizmoHandle::X;
        ctx.native_gizmo.dragging = true;
        let origin = viewport_project(&cam, width, height, Vec3::ZERO);
        ctx.native_gizmo.start_mouse = origin.pixel;
        ctx.snapshot_native_gizmo_start(entity);
        (ctx, cam, width, height, entity)
    }

    #[test]
    fn translate_drag_on_root_moves_along_the_projected_axis_and_bumps_version() {
        let (mut ctx, cam, width, height, entity) = drag_context();
        let before = ctx.scene_version;
        // Drag the X handle: push the mouse along the +X screen direction.
        let x_screen = {
            let a = viewport_project(&cam, width, height, Vec3::ZERO).pixel;
            let b = viewport_project(&cam, width, height, Vec3::X).pixel;
            (b - a).normalize()
        };
        let mouse = ctx.native_gizmo.start_mouse + x_screen * 80.0;
        ctx.apply_native_gizmo_drag(&cam, width, height, mouse);

        let t = ctx.scene.component::<Transform>(entity).unwrap();
        assert!(
            t.translation.x > 0.05,
            "moved along +X: {:?}",
            t.translation
        );
        assert!(t.translation.y.abs() < 1e-3, "no Y drift");
        assert!(t.translation.z.abs() < 1e-3, "no Z drift");
        assert_eq!(
            ctx.scene_version,
            before + 1,
            "the drag bumps scene_version"
        );
    }

    #[test]
    fn rotate_drag_on_root_keeps_the_raw_euler() {
        let (mut ctx, cam, width, height, entity) = drag_context();
        ctx.gizmo_op = GizmoOp::Rotate;
        ctx.sync_native_gizmo();
        // Seed a large authored Z rotation past what a matrix extraction would wrap.
        let _ = ctx
            .scene
            .with_component_mut::<Transform, _>(entity, |t| t.rotation = Vec3::new(0.0, 0.0, 6.0));
        ctx.native_gizmo.active = NativeGizmoHandle::Z;
        ctx.snapshot_native_gizmo_start(entity);

        // A small positive delta adds (delta.x + delta.y)*0.01 to the Z Euler, raw.
        let mouse = ctx.native_gizmo.start_mouse + Vec2::new(50.0, 0.0);
        ctx.apply_native_gizmo_drag(&cam, width, height, mouse);

        let t = ctx.scene.component::<Transform>(entity).unwrap();
        // 6.0 + 50*0.01 = 6.5, kept raw (a matrix extraction would have wrapped past 2π).
        assert!(
            (t.rotation.z - 6.5).abs() < 1e-4,
            "root keeps the raw Euler: {}",
            t.rotation.z
        );
        assert!(t.rotation.z > std::f32::consts::PI, "past the wrap point");
    }

    #[test]
    fn rotate_drag_on_parented_entity_peels_the_parent_rotation() {
        let mut ctx = SceneEditContext::default();
        let parent = ctx.scene.create_entity("Parent");
        let child = ctx.scene.create_entity("Child");
        // Rotate the parent +90° about Z in world.
        let _ = ctx.scene.with_component_mut::<Transform, _>(parent, |t| {
            t.rotation = Vec3::new(0.0, 0.0, std::f32::consts::FRAC_PI_2);
        });
        ctx.scene.set_parent(child, Some(parent), false).unwrap();
        ctx.scene.update_world_transforms();
        ctx.set_selection(child);

        let (width, height) = (1280, 720);
        let cam = test_camera(Vec3::new(2.0, 1.5, 6.0));
        ctx.gizmo_op = GizmoOp::Rotate;
        ctx.gizmo_space = GizmoSpace::World;
        ctx.sync_native_gizmo();
        ctx.native_gizmo.target = child;
        ctx.native_gizmo.active = NativeGizmoHandle::Z;
        ctx.native_gizmo.dragging = true;
        let origin = viewport_project(&cam, width, height, ctx.scene.world_translation(child));
        ctx.native_gizmo.start_mouse = origin.pixel;
        ctx.snapshot_native_gizmo_start(child);

        // The snapshot freezes the child's *world* rotation Euler (parent included, ≈ +90°Z).
        let start_world_euler = ctx.native_gizmo.start_rotation;
        assert!(
            (start_world_euler.z - std::f32::consts::FRAC_PI_2).abs() < 1e-4,
            "the snapshot freezes the world rotation (parent's +90° Z)"
        );

        // Drag the Z handle: world Euler += 30*0.01 = 0.3 rad about Z.
        let mouse = ctx.native_gizmo.start_mouse + Vec2::new(30.0, 0.0);
        ctx.apply_native_gizmo_drag(&cam, width, height, mouse);

        // The child *local* rotation holds the parent peeled out: inverse(parent world rot)
        // composed with the new world Euler — it must NOT carry the parent's +90°.
        let child_local =
            quat_from_euler_xyz(ctx.scene.component::<Transform>(child).unwrap().rotation);
        let parent_world_rot = ctx.scene.world_rotation(parent);
        let world_euler = start_world_euler + Vec3::new(0.0, 0.0, 0.3);
        let expected_local = parent_world_rot.inverse() * quat_from_euler_xyz(world_euler);
        assert!(
            child_local.dot(expected_local).abs() > 1.0 - 1e-4,
            "the child local peels the parent rotation: {child_local:?} vs {expected_local:?}"
        );

        // And recomposed, the child's world rotation is the dragged world Euler (start + delta).
        ctx.scene.update_world_transforms();
        let world_rot = ctx.scene.world_rotation(child);
        let expected_world = quat_from_euler_xyz(world_euler);
        assert!(
            world_rot.dot(expected_world).abs() > 1.0 - 1e-4,
            "child world rotation is the dragged world Euler (dot {})",
            world_rot.dot(expected_world).abs()
        );
    }

    #[test]
    fn scale_drag_multiplies_start_scale_and_floors_at_005() {
        let (mut ctx, cam, width, height, entity) = drag_context();
        ctx.gizmo_op = GizmoOp::Scale;
        ctx.sync_native_gizmo();
        let _ = ctx
            .scene
            .with_component_mut::<Transform, _>(entity, |t| t.scale = Vec3::new(2.0, 2.0, 2.0));
        ctx.native_gizmo.active = NativeGizmoHandle::X;
        ctx.snapshot_native_gizmo_start(entity);
        let before = ctx.scene_version;

        // A strong negative-X drag drives the factor below 0.05, where it floors.
        let x_screen = {
            let a = viewport_project(&cam, width, height, Vec3::ZERO).pixel;
            let b = viewport_project(&cam, width, height, Vec3::X).pixel;
            (b - a).normalize()
        };
        let mouse = ctx.native_gizmo.start_mouse - x_screen * 100000.0;
        ctx.apply_native_gizmo_drag(&cam, width, height, mouse);

        let t = ctx.scene.component::<Transform>(entity).unwrap();
        // factor floors at 0.05; start_scale.x = 2.0 → 0.1.
        assert!(
            (t.scale.x - 0.1).abs() < 1e-4,
            "floored X scale: {}",
            t.scale.x
        );
        assert!((t.scale.y - 2.0).abs() < 1e-4, "Y scale untouched");
        assert!((t.scale.z - 2.0).abs() < 1e-4, "Z scale untouched");
        assert!(ctx.scene_version > before, "scale drag bumps scene_version");
    }

    #[test]
    fn preserve_children_holds_a_child_world_through_a_parent_translate() {
        let mut ctx = SceneEditContext {
            preserve_children: true,
            ..SceneEditContext::default()
        };
        let parent = ctx.scene.create_entity("Parent");
        let child = ctx.scene.create_entity("Child");
        let _ = ctx.scene.with_component_mut::<Transform, _>(child, |t| {
            t.translation = Vec3::new(1.0, 0.0, 0.0)
        });
        ctx.scene.set_parent(child, Some(parent), true).unwrap();
        ctx.scene.update_world_transforms();
        let child_world_before = ctx.scene.compose_world_matrix(child);

        ctx.set_selection(parent);
        let (width, height) = (1280, 720);
        let cam = test_camera(Vec3::new(2.0, 1.5, 6.0));
        ctx.gizmo_op = GizmoOp::Translate;
        ctx.gizmo_space = GizmoSpace::World;
        ctx.sync_native_gizmo();
        ctx.native_gizmo.target = parent;
        ctx.native_gizmo.active = NativeGizmoHandle::Y;
        ctx.native_gizmo.dragging = true;
        let origin = viewport_project(&cam, width, height, Vec3::ZERO);
        ctx.native_gizmo.start_mouse = origin.pixel;
        ctx.snapshot_native_gizmo_start(parent);

        // Drag the parent up along the +Y screen direction.
        let y_screen = {
            let a = viewport_project(&cam, width, height, Vec3::ZERO).pixel;
            let b = viewport_project(&cam, width, height, Vec3::Y).pixel;
            (b - a).normalize()
        };
        let mouse = ctx.native_gizmo.start_mouse + y_screen * 90.0;
        ctx.apply_native_gizmo_drag(&cam, width, height, mouse);

        // The parent moved...
        let parent_t = ctx.scene.component::<Transform>(parent).unwrap();
        assert!(parent_t.translation.length() > 0.05, "the parent moved");
        // ...but the child's world transform is held (its local rebased).
        let child_world_after = ctx.scene.compose_world_matrix(child);
        let cols_before = child_world_before.to_cols_array();
        let cols_after = child_world_after.to_cols_array();
        for (a, b) in cols_before.iter().zip(cols_after.iter()) {
            assert!(
                (a - b).abs() < 1e-4,
                "the child world is unchanged ({a} vs {b})"
            );
        }
    }

    #[test]
    fn step_native_gizmo_drag_converges_toward_the_target() {
        let (mut ctx, cam, width, height, entity) = drag_context();
        // A pending command-driven drag sample 200px to the right.
        let target = ctx.native_gizmo.start_mouse + Vec2::new(200.0, 0.0);
        ctx.native_gizmo.drag_target = target;
        ctx.native_gizmo.drag_smoothed = ctx.native_gizmo.start_mouse;
        ctx.native_gizmo.drag_pending = true;

        let dt = 1.0 / 60.0;
        let mut prev = ctx.native_gizmo.drag_smoothed.x;
        let mut prev_x = ctx
            .scene
            .component::<Transform>(entity)
            .unwrap()
            .translation
            .x;
        for _ in 0..240 {
            ctx.step_native_gizmo_drag(&cam, width, height, dt);
            let s = ctx.native_gizmo.drag_smoothed.x;
            assert!(
                s >= prev - 1e-4,
                "the smoothed pointer advances monotonically"
            );
            assert!(s <= target.x + 1e-3, "no overshoot past the target sample");
            prev = s;
            let x = ctx
                .scene
                .component::<Transform>(entity)
                .unwrap()
                .translation
                .x;
            assert!(
                x >= prev_x - 1e-4,
                "the applied translation advances with it"
            );
            prev_x = x;
        }
        assert!(
            (ctx.native_gizmo.drag_smoothed.x - target.x).abs() < 1.0,
            "the smoothed pointer converges to the target sample"
        );
    }

    #[test]
    fn step_native_gizmo_drag_is_a_noop_without_a_pending_sample() {
        let (mut ctx, cam, width, height, _entity) = drag_context();
        ctx.native_gizmo.drag_pending = false;
        let before = ctx.scene_version;
        ctx.step_native_gizmo_drag(&cam, width, height, 1.0 / 60.0);
        assert_eq!(ctx.scene_version, before, "no pending sample → no apply");
    }
}
