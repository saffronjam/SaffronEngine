//! The native gizmo overlay geometry: the CPU builders the editor's in-viewport chrome is drawn from.
//! The hit-test, projection, and drag math lives in `saffron-sceneedit`; these builders only consume
//! it, pushing pure geometry into a `&mut Vec<OverlayVertex>`.
//!
//! [`build_scene_edit_overlay`] builds a `depth_tested` range (occluded by scene geometry) and an
//! `on_top` range, handing both to `Renderer::submit_overlay` so the overlay pass draws each with its
//! own pipeline from one buffer. `edit_chrome` gates the Edit-only chrome; colliders and the skeleton
//! sit outside that gate with their own preview guards.

mod chrome;
mod colliders;
mod debug;
mod primitives;

use glam::Vec3;

use saffron_assets::{AssetServer, GpuUploader};
use saffron_rendering::OverlayVertex;
use saffron_scene::CameraView;
use saffron_sceneedit::SceneEditContext;

use chrome::{
    build_native_gizmo, build_scene_edit_billboards, build_scene_edit_camera_frustums,
    build_skeleton_overlay,
};
use colliders::build_collider_overlays;
use debug::{
    build_debug_overlays, build_heatmap_overlay, build_navigation_overlay,
    build_plant_proxy_overlays, build_rejection_overlay, build_vegetation_overlays,
    build_wind_overlay,
};

/// The per-frame inputs of one overlay build: the camera framing, the viewport extent, whether the
/// edit chrome draws, and the live vegetation world when a map is bound.
pub struct OverlayFrame<'a> {
    pub cam: &'a CameraView,
    pub width: u32,
    pub height: u32,
    pub edit_chrome: bool,
    pub vegetation: Option<&'a saffron_runtime::VegetationWorld>,
    /// Reason-coded rejected-candidate markers (world metres; empty when the
    /// rejection overlay is off).
    pub rejections: &'a [(Vec3, u8)],
    /// Surface-cast micro-density texels (world metres + density 0..1; empty when
    /// the heatmap overlay is off).
    pub heatmap: &'a [(Vec3, f32)],
    /// Sampled wind vectors (world-metre base + velocity m/s; empty when the wind
    /// overlay is off).
    pub wind: &'a [(Vec3, Vec3)],
    /// Published navigation contributions and the regions awaiting a rebuild (empty when the
    /// navigation overlay is off).
    pub navigation: Option<&'a saffron_runtime::VegetationNavigationSeam>,
}

/// Builds both overlay ranges for one frame.
///
/// Returns `(depth_tested, on_top)` so the host's `render_ui` owns the renderer borrow when it
/// submits, and unit tests assert the ranges without a GPU.
#[must_use]
pub fn build_scene_edit_overlay(
    editor: &mut SceneEditContext,
    assets: &mut AssetServer,
    gpu: &dyn GpuUploader,
    frame: &OverlayFrame<'_>,
) -> (Vec<OverlayVertex>, Vec<OverlayVertex>) {
    let OverlayFrame {
        cam,
        width,
        height,
        edit_chrome,
        vegetation,
        rejections,
        heatmap,
        wind,
        navigation,
    } = *frame;
    let mut depth_tested: Vec<OverlayVertex> = Vec::new();
    let mut on_top: Vec<OverlayVertex> = Vec::new();
    if edit_chrome {
        build_scene_edit_camera_frustums(editor, cam, width, height, &mut depth_tested);
        build_debug_overlays(editor, assets, gpu, cam, width, height, &mut depth_tested);
        build_vegetation_overlays(editor, vegetation, cam, width, height, &mut depth_tested);
        build_rejection_overlay(rejections, cam, width, height, &mut depth_tested);
        build_heatmap_overlay(heatmap, cam, width, height, &mut depth_tested);
        build_wind_overlay(wind, cam, width, height, &mut depth_tested);
        build_navigation_overlay(editor, navigation, cam, width, height, &mut depth_tested);
        build_scene_edit_billboards(editor, cam, width, height, &mut on_top);
        build_native_gizmo(editor, cam, width, height, &mut on_top);
    }
    // Colliders draw in Edit AND Play (they read the authored Collider), so they sit outside
    // edit_chrome like the skeleton overlay, with their own preview guard (inside the call).
    build_collider_overlays(editor, assets, gpu, cam, width, height, &mut depth_tested);
    build_plant_proxy_overlays(editor, assets, cam, width, height, &mut depth_tested);
    build_skeleton_overlay(editor, cam, width, height, &mut on_top);
    (depth_tested, on_top)
}

#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use glam::Mat4;
    use saffron_sceneedit::GizmoOp;

    /// A camera at `eye` looking at the origin, the framing the overlay tests project against.
    pub(crate) fn test_camera(eye: Vec3) -> CameraView {
        CameraView {
            view: Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y),
            fov: 45.0,
            near_plane: 0.1,
            far_plane: 100.0,
        }
    }

    /// A null GPU uploader: the overlay's mesh-resolving paths negative-cache to `None` against an
    /// empty catalog before reaching it.
    pub(crate) struct NoGpu;
    impl GpuUploader for NoGpu {
        fn upload_mesh(
            &self,
            _mesh: &saffron_geometry::Mesh,
            _hierarchy: &saffron_geometry::PortableVirtualHierarchy,
            _skin: &[saffron_geometry::VertexSkin],
            _morph: Option<&saffron_geometry::MorphData>,
            _sdf: saffron_rendering::SdfSource<'_>,
        ) -> saffron_rendering::Result<std::sync::Arc<saffron_rendering::GpuMesh>> {
            unreachable!("an empty catalog never reaches the uploader")
        }
        fn upload_texture(
            &self,
            _rgba: &[u8],
            _width: u32,
            _height: u32,
            _srgb: bool,
        ) -> saffron_rendering::Result<std::sync::Arc<saffron_rendering::GpuTexture>> {
            unreachable!()
        }
        fn upload_texture_float(
            &self,
            _rgba: &[f32],
            _width: u32,
            _height: u32,
        ) -> saffron_rendering::Result<std::sync::Arc<saffron_rendering::GpuTexture>> {
            unreachable!()
        }
        fn skinning_enabled(&self) -> bool {
            false
        }
    }

    /// A context with a transformable selection at the origin, a spot light, and a camera, enough
    /// to exercise both overlay ranges.
    pub(crate) fn overlay_context() -> SceneEditContext {
        let mut ctx = SceneEditContext::new();
        let target = ctx.scene.create_entity("Target");
        ctx.scene.relink_hierarchy();
        ctx.set_selection(target);
        ctx.gizmo_op = GizmoOp::Translate;
        ctx.sync_native_gizmo();
        let spot = ctx.scene.create_entity("Spot");
        let _ = ctx
            .scene
            .add_component(spot, saffron_scene::SpotLight::default());
        ctx.scene.relink_hierarchy();
        ctx.scene.update_world_transforms();
        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::{NoGpu, overlay_context, test_camera};
    use super::*;
    use saffron_scene::{Collider, Scene, Shape};

    #[test]
    fn submit_scene_edit_overlay_ranges() {
        let mut ctx = overlay_context();
        let mut assets = AssetServer::new(std::env::temp_dir().join("saffron-overlay-test-edit"));
        let gpu = NoGpu;
        let cam = test_camera(Vec3::new(3.0, 2.5, 6.0));
        let (w, h) = (1280u32, 720u32);

        let (depth, on_top) = build_scene_edit_overlay(
            &mut ctx,
            &mut assets,
            &gpu,
            &OverlayFrame {
                cam: &cam,
                width: w,
                height: h,
                edit_chrome: true,
                vegetation: None,
                rejections: &[],
                heatmap: &[],
                wind: &[],
                navigation: None,
            },
        );
        assert!(
            !on_top.is_empty(),
            "edit chrome populates the on-top range (gizmo + billboards)"
        );
        assert!(
            !depth.is_empty(),
            "the seeded camera's frustum populates the depth-tested range"
        );

        let (depth_play, on_top_play) = build_scene_edit_overlay(
            &mut ctx,
            &mut assets,
            &gpu,
            &OverlayFrame {
                cam: &cam,
                width: w,
                height: h,
                edit_chrome: false,
                vegetation: None,
                rejections: &[],
                heatmap: &[],
                wind: &[],
                navigation: None,
            },
        );
        assert!(
            depth_play.is_empty() && on_top_play.is_empty(),
            "no edit chrome and no colliders/skeleton → both ranges empty"
        );
    }

    #[test]
    fn colliders_and_skeleton_ignore_edit_chrome() {
        let mut ctx = SceneEditContext::new();
        let body = ctx.scene.create_entity("Body");
        let _ = ctx.scene.add_component(
            body,
            Collider {
                shape: Shape::Capsule,
                half_extents: Vec3::new(0.4, 0.8, 0.4),
                ..Collider::default()
            },
        );
        ctx.scene.relink_hierarchy();
        ctx.scene.update_world_transforms();
        ctx.debug_overlays.colliders = true;

        let mut assets =
            AssetServer::new(std::env::temp_dir().join("saffron-overlay-test-collider"));
        let gpu = NoGpu;
        let cam = test_camera(Vec3::new(3.0, 2.5, 6.0));
        let (w, h) = (1280u32, 720u32);

        let (depth, _on_top) = build_scene_edit_overlay(
            &mut ctx,
            &mut assets,
            &gpu,
            &OverlayFrame {
                cam: &cam,
                width: w,
                height: h,
                edit_chrome: false,
                vegetation: None,
                rejections: &[],
                heatmap: &[],
                wind: &[],
                navigation: None,
            },
        );
        assert!(
            !depth.is_empty(),
            "colliders draw in Play (outside edit_chrome)"
        );

        ctx.preview_scene = Some(Scene::new());
        ctx.preview_active_view = true;
        let (depth_preview, _) = build_scene_edit_overlay(
            &mut ctx,
            &mut assets,
            &gpu,
            &OverlayFrame {
                cam: &cam,
                width: w,
                height: h,
                edit_chrome: false,
                vegetation: None,
                rejections: &[],
                heatmap: &[],
                wind: &[],
                navigation: None,
            },
        );
        assert!(
            depth_preview.is_empty(),
            "the collider preview guard suppresses them while previewing"
        );
    }
}
