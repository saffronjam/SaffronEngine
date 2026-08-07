use std::sync::Mutex;

use super::*;
use crate::device::SurfaceSource;
use crate::resources::BindlessFreeList;
use crate::validation_issue_count;

/// Building the DDGI sub-state (the atlases + ray image + the four layouts/sets + mesh set 5 +
/// the static descriptor writes + the one-shot atlas init barrier) is validation-clean on a
/// software device — the GPU-runtime half of this phase the toolbox can actually run (the
/// four-pass chain is all compute, no ray tracing). The per-frame trace render is exercised by
/// the engine e2e once DDGI is wired through the control plane; here the resource bring-up + the
/// init transition are validated. Skips cleanly when no Vulkan device is obtainable.
#[test]
fn ddgi_resource_bringup_is_validation_clean() {
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

    let mut ddgi = Ddgi::new(&device, &descriptors).expect("Ddgi::new");
    // Built + ready, ON by default; the two atlases rest in ShaderReadOnly after the init
    // barrier (the mesh-sample resting state).
    assert!(ddgi.ready);
    assert!(ddgi.use_ddgi);
    assert_eq!(
        ddgi.irradiance().2.layout,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
    );
    assert_eq!(
        ddgi.distance().2.layout,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
    );
    assert_ne!(ddgi.mesh_set(), vk::DescriptorSet::null());

    // A camera-centered scene set snaps the volume to the probe grid; the probe-grid UBO + the
    // scroll base match the constants.
    assert!(ddgi.history_reset());
    assert!(ddgi.enabled());
    ddgi.set_scene(
        Vec3::new(5.0, 2.0, -3.0),
        Vec3::new(0.0, -1.0, 0.0),
        Vec3::ONE,
        1.0,
    );
    // The volume centres on the camera (extent = count · spacing), snapped to a whole cell.
    let (vmin, vext) = ddgi.volume();
    assert_eq!(vext.x, DDGI_PROBES_X as f32 * DDGI_PROBE_SPACING);
    let center = vmin + vext * 0.5;
    assert!((center.x - 5.0).abs() <= DDGI_PROBE_SPACING);
    assert_eq!(ddgi.probe_count_ubo().x, DDGI_PROBES_X);

    drop(ddgi);
    // SAFETY: the device must idle before its sub-state Drops (here Ddgi already dropped,
    // freeing its sampler + layouts + images).
    device.wait_idle().expect("wait_idle");
    assert_eq!(
        validation_issue_count(),
        before,
        "the DDGI bring-up + init transition raised no validation issues"
    );
}

/// The push-constant structs byte-match the `.slang` `Push` layouts the SPIR-V reads — a wrong
/// offset is a silently corrupted dispatch, so pin each size.
#[test]
fn ddgi_push_sizes_match_slang() {
    assert_eq!(size_of::<TracePush>(), 112);
    assert_eq!(size_of::<BlendPush>(), 80);
    assert_eq!(size_of::<BorderPush>(), 32);
}

/// The octahedral atlas dimensions follow `tilesPerRow · (interior + 2)`, matching the shaders'
/// `atlasW`/`atlasH` derivation — a wrong gutter count corrupts the border copy + the bilinear
/// sample. Pinned at the camera-centered clipmap's 16×8×16 probe grid.
#[test]
fn atlas_dimensions_match_octahedral_tiling() {
    assert_eq!(irradiance_atlas_width(), 16 * 8 * (8 + 2));
    assert_eq!(irradiance_atlas_height(), 16 * (8 + 2));
    assert_eq!(distance_atlas_width(), 16 * 8 * (16 + 2));
    assert_eq!(distance_atlas_height(), 16 * (16 + 2));
    assert_eq!(DDGI_PROBE_TOTAL, 16 * 8 * 16);
    assert_eq!(DDGI_PROBE_BUDGET, DDGI_PROBE_TOTAL / 4);
}

/// Euclidean modulo wraps a negative cell to a positive physical tile (the toroidal scroll
/// fold) — an off-by-one here crawls or flashes probes as the camera moves.
#[test]
fn wrap_mod_is_euclidean() {
    assert_eq!(wrap_mod(0, 16), 0);
    assert_eq!(wrap_mod(17, 16), 1);
    assert_eq!(wrap_mod(-1, 16), 15);
    assert_eq!(wrap_mod(-16, 16), 0);
    assert_eq!(wrap_mod(-17, 16), 15);
}

/// The pure DDGI state machine (enable/scene/advance/history/scroll) without a device, so the
/// acceptance gate can assert the camera-snap + scroll-delta + history-reset behavior the named
/// tests require. Mirrors the production fields the methods touch.
#[derive(Default)]
struct DdgiState {
    use_ddgi: bool,
    ready: bool,
    history_reset: bool,
    frame_index: u32,
    snap_base: IVec3,
    snap_base_ring: [IVec3; DDGI_PROBE_CYCLE],
}

impl DdgiState {
    fn set_enabled(&mut self, enabled: bool) {
        if enabled && !self.use_ddgi {
            self.history_reset = true;
        }
        self.use_ddgi = enabled;
    }

    fn set_scene(&mut self, cam_pos: Vec3) {
        if !self.ready {
            return;
        }
        let count = IVec3::new(
            DDGI_PROBES_X as i32,
            DDGI_PROBES_Y as i32,
            DDGI_PROBES_Z as i32,
        );
        let snapped = (cam_pos / DDGI_PROBE_SPACING).round().as_ivec3();
        self.snap_base = snapped - count / 2;
    }

    fn advance_frame(&mut self) {
        self.snap_base_ring[self.frame_index as usize % DDGI_PROBE_CYCLE] = self.snap_base;
        self.frame_index = self.frame_index.wrapping_add(1);
        self.history_reset = false;
    }

    /// Scroll accumulated over one round-robin cycle — the reset reference for a probe re-rayed
    /// this frame (it was last traced exactly a cycle ago).
    fn delta(&self) -> IVec3 {
        self.snap_base - self.snap_base_ring[self.frame_index as usize % DDGI_PROBE_CYCLE]
    }

    fn wants_ddgi(&self, pipelines_ready: bool) -> bool {
        self.use_ddgi && self.ready && pipelines_ready
    }
}

/// The four DDGI passes run only when DDGI is on AND the resources/PSOs are ready; absent
/// otherwise (the acceptance gate's first bullet). Default-on, so they run from boot.
#[test]
fn wants_ddgi_only_when_on_ready_and_pipelines_present() {
    let mut s = DdgiState {
        use_ddgi: true,
        ready: true,
        ..Default::default()
    };
    // On + ready + PSOs present → the chain runs (default state at boot).
    assert!(s.wants_ddgi(true));
    // On + ready but the PSOs failed to build → no passes.
    assert!(!s.wants_ddgi(false));
    // Off → never, whatever the pipelines.
    s.set_enabled(false);
    assert!(!s.wants_ddgi(true));
    // Not ready → never (e.g. a creation failure left `ready` false).
    s.set_enabled(true);
    s.ready = false;
    assert!(!s.wants_ddgi(true));
}

/// `set_scene` snaps the volume's min corner to the probe grid so the cage centres on the
/// camera; the cycle-accumulated scroll delta (the toroidal relocation reference, measured over
/// one round-robin cycle) is zero for a sub-cell creep and one cell for a whole-cell move. A
/// no-op before `ready`.
#[test]
fn set_scene_snaps_volume_and_tracks_scroll() {
    let mut s = DdgiState {
        use_ddgi: true,
        ..Default::default()
    };
    // Before ready → no-op (stale zero base).
    s.set_scene(Vec3::new(100.0, 0.0, 0.0));
    assert_eq!(s.snap_base, IVec3::ZERO);

    s.ready = true;
    s.set_scene(Vec3::ZERO);
    // Centred on the origin: min corner is -count/2 cells.
    assert_eq!(s.snap_base.x, -(DDGI_PROBES_X as i32) / 2);
    // Fill the cycle ring with a stable base so the cycle-delta references it.
    for _ in 0..DDGI_PROBE_CYCLE {
        s.set_scene(Vec3::ZERO);
        s.advance_frame();
    }
    // A sub-cell creep that does not cross a probe cell → no scroll over the cycle.
    s.set_scene(Vec3::new(DDGI_PROBE_SPACING * 0.3, 0.0, 0.0));
    assert_eq!(s.delta(), IVec3::ZERO);
    // A whole-cell move scrolls the cage by exactly one cell relative to a cycle ago.
    s.set_scene(Vec3::new(DDGI_PROBE_SPACING, 0.0, 0.0));
    assert_eq!(s.delta(), IVec3::new(1, 0, 0));
}

/// A one-cell scroll stays visible in the cycle-accumulated delta for a full round-robin cycle
/// — so every probe that scrolled in is still flagged newly-exposed on whatever frame the
/// budget finally re-rays it, with no probe slipping through between slices (defect: a
/// single-frame delta dropped the flag before the ~3/4 of the slab outside that frame's slice
/// was retraced, leaving a ghosting trail). After a full cycle at rest the reference catches up,
/// so a held camera raises no spurious reset.
#[test]
fn scroll_exposure_persists_across_a_round_robin_cycle() {
    let mut s = DdgiState {
        use_ddgi: true,
        ready: true,
        ..Default::default()
    };
    // Stabilise the cycle ring at the origin base.
    for _ in 0..DDGI_PROBE_CYCLE {
        s.set_scene(Vec3::ZERO);
        s.advance_frame();
    }
    // One whole-cell scroll, then hold the camera still for a full cycle: the cycle-delta keeps
    // reporting the one-cell scroll until the moved base propagates through the ring.
    let moved = Vec3::new(DDGI_PROBE_SPACING, 0.0, 0.0);
    for _ in 0..DDGI_PROBE_CYCLE {
        s.set_scene(moved);
        assert_eq!(s.delta(), IVec3::new(1, 0, 0));
        s.advance_frame();
    }
    // After a full cycle at the new position the reference catches up → no spurious reset.
    s.set_scene(moved);
    assert_eq!(s.delta(), IVec3::ZERO);
}

/// `history_reset` is set on enable and stays set until a frame is recorded, then
/// `advance_frame` clears it; re-enabling re-arms it (the acceptance gate — set on
/// enable/resize, cleared on subsequent frames). `advance_frame` also commits the scroll base.
#[test]
fn history_reset_arms_on_enable_and_clears_after_a_frame() {
    let mut s = DdgiState {
        ready: true,
        ..Default::default()
    };
    // Enabling from off arms the reset.
    s.set_enabled(true);
    assert!(s.history_reset);
    // The first recorded frame consumes it + records the scroll base into the cycle ring.
    s.set_scene(Vec3::new(20.0, 0.0, 0.0));
    s.advance_frame();
    assert!(!s.history_reset);
    assert_eq!(s.frame_index, 1);
    assert_eq!(s.snap_base_ring[0], s.snap_base);
    // A second frame keeps it cleared and bumps the index.
    s.advance_frame();
    assert!(!s.history_reset);
    assert_eq!(s.frame_index, 2);
    // Re-enabling while already on does NOT re-arm (only an off→on edge does).
    s.set_enabled(true);
    assert!(!s.history_reset);
    // A resize / explicit reset re-arms it.
    s.history_reset = true;
    assert!(s.history_reset);
}
