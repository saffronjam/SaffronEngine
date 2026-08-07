//! Pending smoothed transform edits and the per-frame convergence stepper.
//!
//! A `smooth:1` edit merges its per-field targets into an entry here instead of writing
//! the component directly; [`SceneEditContext::step_edit_smoothing`] converges the entity's
//! component toward the target each rendered frame (the same `tau = 0.025` exponential the
//! gizmo pointer drag and the preview orbit share), snapping exactly and dropping the entry
//! once converged.

use glam::Vec3;

use saffron_scene::{Entity, Transform};

use crate::context::SceneEditContext;

/// The exponential smoothing time constant (seconds), shared by the gizmo pointer drag, the
/// preview orbit's camera ease, and the edit-smoothing stepper.
///
/// At ~25 ms a 60 Hz control sample is reached in roughly two frames' worth of lag while
/// the sample staircase becomes continuous motion. The single named source; the orbit
/// camera's ease ([`crate::camera`]) and the gizmo drag both step
/// `alpha = 1 - exp(-dt/TAU)` against it.
pub(crate) const SMOOTH_TAU: f32 = 0.025;

/// The convergence epsilon: an edit snaps exactly and its entry drops once every smoothed
/// field is within this of its target.
const SMOOTH_EPSILON: f32 = 1e-4;

/// A pending smoothed transform edit (`set-transform smooth:1`): Inspector scrubs converge
/// like gizmo drags instead of stepping at the send rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformSmoothTarget {
    /// The entity whose `Transform` is converging.
    pub entity: Entity,
    /// The target local translation, if set.
    pub translation: Option<Vec3>,
    /// The target local rotation (Euler XYZ radians), if set.
    pub rotation: Option<Vec3>,
    /// The target local scale, if set.
    pub scale: Option<Vec3>,
}

impl TransformSmoothTarget {
    /// An empty entry targeting `entity` with no fields set.
    #[must_use]
    pub fn new(entity: Entity) -> Self {
        Self {
            entity,
            translation: None,
            rotation: None,
            scale: None,
        }
    }
}

/// An exponentially-smoothable component field: steps toward a target by `alpha`, snapping
/// exactly and reporting convergence once within [`SMOOTH_EPSILON`].
trait BlendToward: Copy {
    /// Steps `*self` toward `target` by `alpha`; returns `true` once within epsilon, having
    /// snapped `*self` to `target` exactly.
    fn blend_toward(&mut self, target: Self, alpha: f32) -> bool;
}

impl BlendToward for f32 {
    fn blend_toward(&mut self, target: Self, alpha: f32) -> bool {
        *self += (target - *self) * alpha;
        if (target - *self).abs() <= SMOOTH_EPSILON {
            *self = target;
            return true;
        }
        false
    }
}

impl BlendToward for Vec3 {
    fn blend_toward(&mut self, target: Self, alpha: f32) -> bool {
        *self += (target - *self) * alpha;
        if (target - *self)
            .abs()
            .cmple(Vec3::splat(SMOOTH_EPSILON))
            .all()
        {
            *self = target;
            return true;
        }
        false
    }
}

impl SceneEditContext {
    /// The pending smoothed-transform entry for `entity`, appended if absent.
    pub fn transform_smooth_entry_for(&mut self, entity: Entity) -> &mut TransformSmoothTarget {
        if let Some(index) = self
            .transform_smoothing
            .iter()
            .position(|entry| entry.entity == entity)
        {
            return &mut self.transform_smoothing[index];
        }
        self.transform_smoothing
            .push(TransformSmoothTarget::new(entity));
        self.transform_smoothing
            .last_mut()
            .expect("just pushed an entry")
    }

    /// Drops `entity`'s smoothed-transform entry.
    pub fn cancel_transform_smoothing(&mut self, entity: Entity) {
        self.transform_smoothing
            .retain(|entry| entry.entity != entity);
    }

    /// Converges every smoothed transform edit toward its targets one rendered frame (the
    /// `tau = 0.025` exponential), snapping exactly and dropping each entry once converged.
    ///
    /// A smooth edit issued during play converges in — and is discarded with — the play
    /// scene; in Edit it is the authored scene. A live gizmo drag owns its target's
    /// transform, so a stale transform entry on the dragged entity is dropped (the drag's
    /// own smoothing wins). Bumps `scene_version` on any applied frame so the control poll
    /// tracks the convergence live.
    pub fn step_edit_smoothing(&mut self, dt: f32) {
        if self.transform_smoothing.is_empty() {
            return;
        }
        let alpha = 1.0 - (-dt.max(0.0) / SMOOTH_TAU).exp();
        let dragging = self.native_gizmo.dragging;
        let drag_target = self.native_gizmo.target;
        let mut applied = false;

        let mut transform = std::mem::take(&mut self.transform_smoothing);
        let scene = self.active_scene();
        transform.retain_mut(|entry| {
            if !scene.valid(entry.entity) || !scene.has_component::<Transform>(entry.entity) {
                return false;
            }
            if dragging && drag_target == entry.entity {
                return false;
            }
            let converged = scene
                .with_component_mut::<Transform, _>(entry.entity, |t| {
                    let mut converged = true;
                    if let Some(target) = entry.translation {
                        converged &= t.translation.blend_toward(target, alpha);
                    }
                    if let Some(target) = entry.rotation {
                        converged &= t.rotation.blend_toward(target, alpha);
                    }
                    if let Some(target) = entry.scale {
                        converged &= t.scale.blend_toward(target, alpha);
                    }
                    converged
                })
                .unwrap_or(true);
            applied = true;
            !converged
        });
        self.transform_smoothing = transform;

        if applied {
            self.scene_version += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context with a transformable entity selected.
    fn smoothing_context() -> (SceneEditContext, Entity) {
        let mut ctx = SceneEditContext::default();
        let entity = ctx.scene.create_entity("Target");
        ctx.set_selection(entity);
        (ctx, entity)
    }

    #[test]
    fn entry_for_appends_then_finds_in_place() {
        let (mut ctx, entity) = smoothing_context();
        ctx.transform_smooth_entry_for(entity).translation = Some(Vec3::new(1.0, 2.0, 3.0));
        // A second call finds the same entry — it does not append a duplicate.
        ctx.transform_smooth_entry_for(entity).scale = Some(Vec3::splat(2.0));
        assert_eq!(ctx.transform_smoothing.len(), 1, "one entry per entity");
        let entry = &ctx.transform_smoothing[0];
        assert_eq!(entry.translation, Some(Vec3::new(1.0, 2.0, 3.0)));
        assert_eq!(entry.scale, Some(Vec3::splat(2.0)));
    }

    #[test]
    fn cancel_drops_the_entry() {
        let (mut ctx, entity) = smoothing_context();
        ctx.transform_smooth_entry_for(entity).scale = Some(Vec3::splat(3.0));
        assert_eq!(ctx.transform_smoothing.len(), 1);
        ctx.cancel_transform_smoothing(entity);
        assert!(
            ctx.transform_smoothing.is_empty(),
            "transform entry dropped"
        );
    }

    #[test]
    fn step_converges_monotonically_then_drops_the_entry() {
        let (mut ctx, entity) = smoothing_context();
        let target = Vec3::new(5.0, 0.0, 0.0);
        ctx.transform_smooth_entry_for(entity).translation = Some(target);
        let before_version = ctx.scene_version;

        let dt = 1.0 / 60.0;
        let mut prev = ctx
            .scene
            .component::<Transform>(entity)
            .unwrap()
            .translation
            .x;
        let mut converged = false;
        for _ in 0..600 {
            ctx.step_edit_smoothing(dt);
            let x = ctx
                .scene
                .component::<Transform>(entity)
                .unwrap()
                .translation
                .x;
            assert!(x >= prev - 1e-6, "translation advances monotonically");
            assert!(x <= target.x + 1e-3, "no overshoot past the target");
            prev = x;
            if ctx.transform_smoothing.is_empty() {
                converged = true;
                break;
            }
        }
        assert!(converged, "the entry drops once converged");
        let t = ctx.scene.component::<Transform>(entity).unwrap();
        assert_eq!(t.translation, target, "it snaps exactly on convergence");
        assert!(
            ctx.scene_version > before_version,
            "applied frames bump scene_version"
        );
    }

    #[test]
    fn step_drops_an_entry_whose_entity_lost_its_component() {
        let (mut ctx, entity) = smoothing_context();
        ctx.transform_smooth_entry_for(entity).translation = Some(Vec3::ONE);
        // Destroy the entity: the next step drops the stale entry without panicking.
        ctx.scene.destroy_entity(entity);
        ctx.step_edit_smoothing(1.0 / 60.0);
        assert!(
            ctx.transform_smoothing.is_empty(),
            "a stale entry is dropped"
        );
    }

    #[test]
    fn step_drops_a_transform_entry_owned_by_a_live_gizmo_drag() {
        let (mut ctx, entity) = smoothing_context();
        ctx.transform_smooth_entry_for(entity).translation = Some(Vec3::new(9.0, 0.0, 0.0));
        // A live drag on the same entity owns its transform; the smooth target yields.
        ctx.native_gizmo.dragging = true;
        ctx.native_gizmo.target = entity;
        ctx.step_edit_smoothing(1.0 / 60.0);
        assert!(
            ctx.transform_smoothing.is_empty(),
            "the drag wins and the stale target drops"
        );
        // The transform was not stepped toward the smooth target.
        let t = ctx.scene.component::<Transform>(entity).unwrap();
        assert_eq!(t.translation, Vec3::ZERO, "the drag owns the transform");
    }

    #[test]
    fn step_is_a_noop_with_no_pending_edits() {
        let (mut ctx, _entity) = smoothing_context();
        let before = ctx.scene_version;
        ctx.step_edit_smoothing(1.0 / 60.0);
        assert_eq!(ctx.scene_version, before, "no entries → no version bump");
    }
}
