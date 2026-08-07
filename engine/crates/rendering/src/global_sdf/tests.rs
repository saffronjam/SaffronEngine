/// The GI gate is a reach test, not a visibility test.
///
/// This is the distinction the whole cut turns on: an occluder behind the camera still
/// shadows what the camera sees, so it must survive, while one beyond the coarsest cascade
/// cannot influence any march and must not. A frustum test would invert exactly this.
#[test]
fn gi_bounds_keep_occluders_behind_the_eye_and_drop_unreachable_ones() {
    let eye = Vec3::new(10.0, 2.0, -30.0);
    let (min, max) = super::gi_occluder_bounds(eye);

    // The eye is inside its own window, and the window reaches at least the coarsest
    // cascade's half extent in every direction.
    assert!(min.cmple(eye).all() && max.cmpge(eye).all());
    let reach = super::cascade_half_extent(super::GDF_CASCADES - 1);
    assert!(
        max.x - eye.x >= reach,
        "the window must reach the coarsest cascade"
    );

    let inside = |p: Vec3| min.cmple(p).all() && max.cmpge(p).all();
    // Directly behind the eye, well within reach: a frustum cull would drop this, and it
    // is precisely the occluder that darkens what is in front.
    assert!(inside(eye + Vec3::new(0.0, 0.0, -8.0)));
    assert!(inside(eye + Vec3::new(0.0, 0.0, 8.0)));
    // Far beyond any cascade: unreachable, so excluding it changes nothing.
    assert!(!inside(eye + Vec3::splat(4000.0)));
}

/// The window travels with the eye rather than being anchored at the origin — otherwise a
/// scene far from the origin would cull everything.
#[test]
fn gi_bounds_follow_the_eye() {
    let (near_min, _) = super::gi_occluder_bounds(Vec3::ZERO);
    let (far_min, _) = super::gi_occluder_bounds(Vec3::new(5000.0, 0.0, 0.0));
    assert!(far_min.x > near_min.x + 4000.0);
}

use std::sync::Mutex;

use super::*;
use crate::device::SurfaceSource;
use crate::resources::BindlessFreeList;
use crate::validation_issue_count;

/// The push + UBO structs byte-match the `.slang` layouts the SPIR-V reads — a wrong offset is
/// a silently corrupted dispatch, so pin each size.
#[test]
fn gdf_struct_sizes_match_slang() {
    assert_eq!(size_of::<GdfCullPush>(), 64);
    assert_eq!(size_of::<GdfCompositePush>(), 48);
    assert_eq!(size_of::<GdfCascadeUbo>(), 32);
    assert_eq!(size_of::<GdfParamsUbo>(), 112);
}

/// The cascade geometry doubles per cascade (the clipmap exponent): finest extent 32 m, voxel
/// 0.25 m; each coarser cascade covers twice the world at twice the voxel size.
#[test]
fn cascade_geometry_follows_the_clipmap_exponent() {
    assert_eq!(cascade_world_extent(0), 32.0);
    assert_eq!(cascade_world_extent(1), 64.0);
    assert_eq!(cascade_world_extent(2), 128.0);
    assert!((cascade_voxel_size(0) - 32.0 / 128.0).abs() < 1e-6);
    assert!((cascade_voxel_size(2) - 1.0).abs() < 1e-6);
    assert_eq!(cascade_half_extent(0), 16.0);
    assert_eq!(cascade_max_encode(0), 8.0);
}

/// `wants_gdf` runs only when on AND ready AND the PSOs are present (the acceptance gate).
#[test]
fn wants_gdf_gates_on_on_ready_and_pipelines() {
    // A device-free shadow of the gate (the real `GlobalSdf::new` needs a Vulkan device).
    struct Gate {
        use_gdf: bool,
        ready: bool,
    }
    impl Gate {
        fn wants_gdf(&self, pipelines: bool) -> bool {
            self.use_gdf && self.ready && pipelines
        }
    }
    let mut g = Gate {
        use_gdf: false,
        ready: true,
    };
    assert!(!g.wants_gdf(true));
    g.use_gdf = true;
    assert!(!g.wants_gdf(false));
    assert!(g.wants_gdf(true));
    g.ready = false;
    assert!(!g.wants_gdf(true));
}

/// First-fill / scroll-out / round-robin all force a single full-window region; a small camera
/// delta yields only the scrolled-in axis slabs (the toroidal incremental update) — an
/// off-by-one here smears the field as the camera moves.
#[test]
fn dirty_regions_full_on_first_fill_else_axis_slabs() {
    let res = 128;
    let c = IVec3::new(10, 0, -5);

    // No history -> one full window region covering [c - res/2, c + res/2).
    let full = cascade_dirty_regions(IVec3::ZERO, c, false, false, res);
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].size, UVec3::splat(res as u32));
    assert_eq!(full[0].base, c - IVec3::splat(res / 2));

    // Round-robin full refresh -> full window even with history + no motion.
    let rr = cascade_dirty_regions(c, c, true, true, res);
    assert_eq!(rr.len(), 1);
    assert_eq!(rr[0].size, UVec3::splat(res as u32));

    // No motion, has history, not round-robin -> nothing to update.
    assert!(cascade_dirty_regions(c, c, true, false, res).is_empty());

    // +3 along x -> one slab, 3 voxels thick, at the high-x leading edge, full on y/z.
    let prev = c;
    let cur = c + IVec3::new(3, 0, 0);
    let slabs = cascade_dirty_regions(prev, cur, true, false, res);
    assert_eq!(slabs.len(), 1);
    let s = slabs[0];
    assert_eq!(s.size, UVec3::new(3, res as u32, res as u32));
    // The slab's leading edge sits at cur.x + res/2 - 3, the newly-entered high-x cells.
    assert_eq!(s.base.x, cur.x + res / 2 - 3);
    assert_eq!(s.base.y, cur.y - res / 2);
    assert_eq!(s.base.z, cur.z - res / 2);

    // -2 along z -> one slab at the low-z edge.
    let slabs_z = cascade_dirty_regions(c, c + IVec3::new(0, 0, -2), true, false, res);
    assert_eq!(slabs_z.len(), 1);
    assert_eq!(slabs_z[0].size, UVec3::new(res as u32, res as u32, 2));
    assert_eq!(slabs_z[0].base.z, (c.z - 2) - res / 2);

    // Diagonal motion -> one slab per moving axis (their union covers all new cells).
    let diag = cascade_dirty_regions(c, c + IVec3::new(1, 0, 1), true, false, res);
    assert_eq!(diag.len(), 2);

    // A scroll past a full window -> a single full refresh, never a giant slab.
    let jump = cascade_dirty_regions(c, c + IVec3::new(res, 0, 0), true, false, res);
    assert_eq!(jump.len(), 1);
    assert_eq!(jump[0].size, UVec3::splat(res as u32));
}

/// Region merge drops empties, and collapses an over-cap set into one covering window.
#[test]
fn merge_regions_dedups_and_caps() {
    let unit = GdfRegion {
        base: IVec3::ZERO,
        size: UVec3::splat(4),
    };
    // Under the cap: kept (dedup only removes exact consecutive repeats).
    let kept = merge_regions(vec![unit, unit]);
    assert!(kept.len() <= 2 && !kept.is_empty());
    // Over the cap: collapsed to a single covering region.
    let many: Vec<_> = (0..GDF_MAX_DIRTY_INSTANCES as i32 + 2)
        .map(|i| GdfRegion {
            base: IVec3::new(i * 8, 0, 0),
            size: UVec3::splat(4),
        })
        .collect();
    let merged = merge_regions(many);
    assert_eq!(merged.len(), 1);
}

/// The composite's empty-voxel + encode contract (mirrored from `gdf_composite.slang`): an
/// empty voxel (no brick covers it) stores `+1` normalized (= +max-encode world), never zero,
/// so the sphere-march never stalls; an occupied voxel stores the clamped signed ratio.
#[test]
fn composite_encode_empty_voxel_is_positive_not_zero() {
    let encode = |best_world: f32, max_encode: f32| -> f32 {
        if best_world >= 1e29 {
            1.0
        } else {
            (best_world / max_encode).clamp(-1.0, 1.0)
        }
    };
    let max_encode = cascade_max_encode(0);
    // Empty voxel -> +1 (full open distance), strictly positive.
    assert_eq!(encode(1e30, max_encode), 1.0);
    // A surface 2 m away in an 8 m encode -> 0.25.
    assert!((encode(2.0, max_encode) - 0.25).abs() < 1e-6);
    // Inside a surface (negative) -> negative, clamped at -1.
    assert_eq!(encode(-100.0, max_encode), -1.0);
}

/// Building the GDF sub-state (cascade volumes, cull SSBO, params UBO, the two layouts/sets, the
/// static descriptor writes, and the one-shot cascade init barrier) is validation-clean on a
/// software device — the resource bring-up half of this phase the toolbox can run. Skips cleanly
/// when no Vulkan device is obtainable.
#[test]
fn gdf_resource_bringup_is_validation_clean() {
    let device = match crate::Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = crate::Descriptors::new(&device, &free_list).expect("Descriptors");
    let before = validation_issue_count();

    let mut gdf = GlobalSdf::new(&device, &descriptors).expect("GlobalSdf::new");
    // Built + ready, on by default; every cascade rests ShaderReadOnly after the init barrier.
    assert!(gdf.ready);
    assert!(gdf.use_gdf);
    for c in 0..GDF_CASCADES {
        assert_eq!(gdf.cascade(c).2, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
    }
    for frame in 0..MAX_FRAMES_IN_FLIGHT {
        assert_ne!(gdf.cull_set(frame), vk::DescriptorSet::null());
        assert_ne!(gdf.composite_set(frame), vk::DescriptorSet::null());
    }

    // On by default with no history yet → the first frame is a full refresh; a camera recenter
    // snaps the cascade centers + rewrites the current frame slot's params UBO; the cull push
    // carries the per-cascade bounds.
    assert!(gdf.enabled());
    gdf.set_camera(Vec3::new(5.0, 2.0, -3.0), 0);
    let push = gdf.cull_push(7);
    assert_eq!(push.counts.x, 7);
    assert_eq!(push.counts.y, GDF_MAX_CULLED);
    // The finest cascade's first frame (no history) -> one full region.
    let regions = gdf.dirty_regions(0);
    assert!(!regions.is_empty());

    drop(gdf);
    // SAFETY: the device must idle before its sub-state Drops.
    device.wait_idle().expect("wait_idle");
    assert_eq!(
        validation_issue_count(),
        before,
        "the GDF bring-up + init transition raised no validation issues"
    );
}
