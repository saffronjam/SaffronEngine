use super::*;
use crate::device::SurfaceSource;
use crate::resources::BindlessFreeList;
use crate::validation_issue_count;
use std::sync::{Arc, Mutex};

/// Builds two views, each with its own screen-space targets + per-view sets, and
/// asserts no cross-view aliasing: each view's sets are distinct handles, each view
/// owns distinct images, and building the second view does not disturb the first
/// view's sets — switching the active view binds *this* view's images, never the
/// other's. Also asserts the build + teardown is validation-clean. Skips when no
/// Vulkan device is present.
#[test]
fn per_view_screen_space_sets_never_alias_across_views() {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let before = validation_issue_count();

    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
    let ssao = Ssao::new(&device).expect("Ssao");

    let mut view_a = ViewTarget::new(&device, 32, 32).expect("view a");
    view_a
        .allocate_screen_space_sets(&descriptors, &ssao)
        .expect("alloc a");
    view_a
        .build_screen_space(&device, &descriptors, &ssao)
        .expect("build a");
    let mut view_b = ViewTarget::new(&device, 48, 24).expect("view b");
    view_b
        .allocate_screen_space_sets(&descriptors, &ssao)
        .expect("alloc b");
    view_b
        .build_screen_space(&device, &descriptors, &ssao)
        .expect("build b");

    // The two views' per-view sets are distinct handles — a switch never binds the
    // other view's set.
    assert_ne!(view_a.gtao_set, view_b.gtao_set);
    assert_ne!(view_a.mesh_set, view_b.mesh_set);
    assert_ne!(view_a.ssgi_set, view_b.ssgi_set);
    // Each view owns distinct screen-space images — its sets bind its own targets.
    let a_g = view_a.g_normal.as_ref().unwrap().handle();
    let b_g = view_b.g_normal.as_ref().unwrap().handle();
    assert_ne!(a_g, b_g, "each view has its own G-buffer image");
    let a_ssgi = view_a.ssgi_denoised.as_ref().unwrap().handle();
    let b_ssgi = view_b.ssgi_denoised.as_ref().unwrap().handle();
    assert_ne!(a_ssgi, b_ssgi);

    drop(view_a);
    drop(view_b);
    drop(ssao);
    drop(descriptors);
    device.wait_idle().expect("idle before teardown");
    drop(device);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the per-view screen-space build + teardown must be validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}

/// The SSGI history validity resets on a view resize: a fresh build leaves it false
/// (no temporal history yet), and rebuilding after a resize re-resets it even if a
/// frame had set it true (the reprojection is stale). Skips when no Vulkan device is
/// present.
#[test]
fn ssgi_history_validity_resets_on_resize() {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
    let ssao = Ssao::new(&device).expect("Ssao");

    let mut view = ViewTarget::new(&device, 32, 32).expect("view");
    view.allocate_screen_space_sets(&descriptors, &ssao)
        .expect("alloc");
    view.build_screen_space(&device, &descriptors, &ssao)
        .expect("build");
    assert!(!view.history_valid, "fresh build has no temporal history");

    // A frame validates the history; a resize must invalidate it again.
    view.history_valid = true;
    view.history_index = 1;
    view.desired_width = 64;
    view.desired_height = 48;
    let ext = vk::Extent2D {
        width: 64,
        height: 48,
    };
    view.resize(&device, ext, ext).expect("resize");
    view.build_screen_space(&device, &descriptors, &ssao)
        .expect("rebuild");
    assert!(
        !view.history_valid,
        "a resize invalidates the SSGI reprojection history"
    );
    assert_eq!(view.history_index, 0, "the ping-pong parity resets too");

    drop(view);
    drop(ssao);
    drop(descriptors);
    device.wait_idle().expect("idle before teardown");
    drop(device);
}

/// `build_aa_targets` creates the per-mode AA targets: the motion target + its depth ride
/// with SSGI; the INPUT-extent scene scratch and the DISPLAY-extent overlay depth are built
/// in every mode (the scene always rasterises into scratch, the resolve reconstructs to the
/// display offscreen); TAA adds the two DISPLAY-extent history images; MSAA adds the
/// multisampled scene color + depth. The build + descriptor set writes + teardown are
/// validation-clean across every mode (a GPU gate the toolbox can run — no ray tracing, no
/// present). Skips when no Vulkan device is present.
#[test]
fn build_aa_targets_per_mode_is_validation_clean() {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let before = validation_issue_count();

    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
    let ssao = Ssao::new(&device).expect("Ssao");
    let supported = device.supported_sample_counts(OFFSCREEN_COLOR_FORMAT, DEPTH_FORMAT);

    let mut view = ViewTarget::new(&device, 32, 32).expect("view");
    view.allocate_screen_space_sets(&descriptors, &ssao)
        .expect("alloc");
    view.build_screen_space(&device, &descriptors, &ssao)
        .expect("build screen-space");

    // Off: motion (SSGI feeds it), the input scratch + display overlay depth (always built),
    // but no TAA history / MSAA.
    let mut aa = crate::Aa::new(supported);
    view.build_aa_targets(&device, &descriptors, aa)
        .expect("build off");
    assert!(view.motion.is_some(), "the motion target rides with SSGI");
    assert!(view.motion_depth.is_some());
    assert!(
        view.scratch.is_some(),
        "the scene always renders into the input scratch"
    );
    assert!(
        view.depth_display.is_some(),
        "the display-extent overlay depth is always built"
    );
    assert!(view.history[0].is_none() && view.history[1].is_none());
    assert!(view.msaa_color.is_none());

    // TAA: motion + its depth + two history + scratch.
    aa.set(0, false, true);
    view.build_aa_targets(&device, &descriptors, aa)
        .expect("build taa");
    assert!(view.motion.is_some(), "TAA builds the motion target");
    assert!(view.motion_depth.is_some());
    assert!(
        view.history[0].is_some() && view.history[1].is_some(),
        "TAA builds the two ping-pong history images"
    );
    assert!(view.scratch.is_some(), "TAA renders the scene into scratch");
    assert!(view.msaa_color.is_none(), "TAA is not MSAA");
    assert!(!view.history_valid, "a fresh build has no temporal history");

    // FXAA: scratch + motion (SSGI feeds it), but no TAA history.
    aa.set(0, true, false);
    view.build_aa_targets(&device, &descriptors, aa)
        .expect("build fxaa");
    assert!(
        view.scratch.is_some(),
        "FXAA renders the scene into scratch"
    );
    assert!(
        view.history[0].is_none() && view.history[1].is_none(),
        "FXAA has no TAA history"
    );
    assert!(view.msaa_color.is_none());

    // MSAA (only when the device supports a count > 1; llvmpipe does).
    if supported.contains(vk::SampleCountFlags::TYPE_4)
        || supported.contains(vk::SampleCountFlags::TYPE_2)
    {
        aa.set(4, false, false);
        view.build_aa_targets(&device, &descriptors, aa)
            .expect("build msaa");
        assert!(
            view.msaa_color.is_some(),
            "MSAA builds the multisampled color"
        );
        assert!(view.msaa_depth.is_some());
        assert!(
            view.scratch.is_some(),
            "the scene always renders into the input scratch (MSAA resolves into it)"
        );
        assert!(view.history[0].is_none() && view.history[1].is_none());
    }

    drop(view);
    drop(ssao);
    drop(descriptors);
    device.wait_idle().expect("idle before teardown");
    drop(device);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the per-mode AA target build must be validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}

/// The temporal bookkeeping: a fresh AA build has no history and no prev-viewProj; a
/// frame's `store_prev_view_proj` + `flip_history` mark them valid and toggle the parity;
/// a rebuild (resize / AA change) re-invalidates both (the reprojection is stale).
/// Skips when no device.
#[test]
fn taa_history_and_prev_view_proj_invalidate_on_rebuild() {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
    let ssao = Ssao::new(&device).expect("Ssao");
    let supported = device.supported_sample_counts(OFFSCREEN_COLOR_FORMAT, DEPTH_FORMAT);
    let mut aa = crate::Aa::new(supported);
    aa.set(0, false, true);

    let mut view = ViewTarget::new(&device, 32, 32).expect("view");
    view.allocate_screen_space_sets(&descriptors, &ssao)
        .expect("alloc");
    view.build_screen_space(&device, &descriptors, &ssao)
        .expect("build screen-space");
    view.build_aa_targets(&device, &descriptors, aa)
        .expect("build taa");
    assert!(!view.history_valid, "fresh build: no temporal history");
    assert!(!view.prev_view_proj_valid, "fresh build: no prev viewProj");
    assert_eq!(view.history_index, 0);

    // A frame consumes the parity + records its viewProj.
    view.store_prev_view_proj(saffron_geometry::glam::Mat4::IDENTITY);
    view.flip_history();
    assert!(view.history_valid, "a frame validates the history");
    assert!(view.prev_view_proj_valid);
    assert_eq!(view.history_index, 1, "the ping-pong parity flipped");

    // A resize rebuilds the screen-space + AA targets and re-invalidates everything.
    view.desired_width = 64;
    view.desired_height = 48;
    let ext = vk::Extent2D {
        width: 64,
        height: 48,
    };
    view.resize(&device, ext, ext).expect("resize");
    view.build_screen_space(&device, &descriptors, &ssao)
        .expect("rebuild screen-space");
    view.build_aa_targets(&device, &descriptors, aa)
        .expect("rebuild taa");
    assert!(
        !view.history_valid,
        "a resize invalidates the TAA reprojection history"
    );
    assert!(
        !view.prev_view_proj_valid,
        "a resize invalidates the prev viewProj"
    );
    assert_eq!(view.history_index, 0, "the parity resets too");

    drop(view);
    drop(ssao);
    drop(descriptors);
    device.wait_idle().expect("idle before teardown");
    drop(device);
}
