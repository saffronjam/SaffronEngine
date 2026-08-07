use super::*;
use crate::descriptors::Descriptors;
use crate::device::SurfaceSource;
use crate::resources::BindlessFreeList;
use crate::validation_issue_count;
use std::sync::Mutex;

fn drain_refresh(ibl: &mut Ibl, device: &Device) {
    for _ in 0..8 {
        device.wait_idle().expect("refresh queue idle");
        ibl.update_refresh(device).expect("advance refresh");
        if ibl.refresh.is_none() && !ibl.rebake_pending && ibl.blend_frames_remaining == 0 {
            return;
        }
    }
    panic!("IBL refresh did not converge");
}

/// Builds a headless device + descriptors or skips (no Vulkan ICD in this toolbox).
fn device_or_skip() -> Option<(Device, Descriptors)> {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return None;
        }
    };
    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = match Descriptors::new(&device, &free_list) {
        Ok(descriptors) => descriptors,
        Err(err) => {
            eprintln!("skipping: descriptors unbuildable ({err})");
            return None;
        }
    };
    Some((device, descriptors))
}

/// `SkygenParams` equality gates the re-bake: identical params (and the same source)
/// arm nothing; a changed sun direction arms a re-bake. The pure gate, no device.
#[test]
fn skygen_params_equality_gates_rebake() {
    let baked = SkygenParams::default();

    // Identical params + same source → no re-bake.
    assert!(
        !should_rebake(
            EnvSource::Procedural,
            EnvSource::Procedural,
            &baked,
            &baked,
            false,
        ),
        "identical inputs must not arm a re-bake"
    );

    let mut below_threshold = baked;
    below_threshold.sun_dir =
        saffron_geometry::glam::Quat::from_rotation_z(0.1_f32.to_radians()) * baked.sun_dir;
    assert!(
        !should_rebake(
            EnvSource::Atmosphere,
            EnvSource::Atmosphere,
            &below_threshold,
            &baked,
            false,
        ),
        "sub-threshold solar motion must not arm a refresh"
    );

    let mut above_threshold = baked;
    above_threshold.sun_dir =
        saffron_geometry::glam::Quat::from_rotation_z(0.3_f32.to_radians()) * baked.sun_dir;
    assert!(should_rebake(
        EnvSource::Atmosphere,
        EnvSource::Atmosphere,
        &above_threshold,
        &baked,
        false,
    ));

    // A changed sun direction → re-bake armed (Procedural reads the sun).
    let mut moved = baked;
    moved.sun_dir = Vec3::new(-0.2, 0.8, 0.5);
    assert!(
        should_rebake(
            EnvSource::Procedural,
            EnvSource::Procedural,
            &moved,
            &baked,
            false,
        ),
        "a changed sun direction must arm a re-bake"
    );

    // A changed sun intensity / color → re-bake armed.
    let mut brighter = baked;
    brighter.sun_intensity = 2.0;
    assert!(should_rebake(
        EnvSource::Procedural,
        EnvSource::Procedural,
        &brighter,
        &baked,
        false,
    ));

    // A source switch alone → re-bake armed even with identical sky params.
    assert!(should_rebake(
        EnvSource::Atmosphere,
        EnvSource::Procedural,
        &baked,
        &baked,
        false,
    ));

    // An atmosphere-param change matters only for the Atmosphere source.
    let mut atmos = baked;
    atmos.atmosphere.mie_anisotropy = 0.5;
    assert!(
        should_rebake(
            EnvSource::Atmosphere,
            EnvSource::Atmosphere,
            &atmos,
            &baked,
            false,
        ),
        "an atmosphere-param change must arm a re-bake under the Atmosphere source"
    );
    assert!(
        !should_rebake(
            EnvSource::Procedural,
            EnvSource::Procedural,
            &atmos,
            &baked,
            false,
        ),
        "an atmosphere-param change must NOT arm a re-bake under the Procedural source"
    );

    let mut cadence = baked;
    cadence.atmosphere.sky_capture_cadence = 1.0;
    assert!(
        !should_rebake(
            EnvSource::Atmosphere,
            EnvSource::Atmosphere,
            &cadence,
            &baked,
            false,
        ),
        "capture cadence changes scheduling without rebuilding the atmosphere"
    );
}

#[test]
fn prefilter_cadence_distributes_every_slice_once() {
    for cadence in [1_u32, 9, 60] {
        let schedule: Vec<Vec<u32>> = (0..cadence)
            .map(|frame| {
                (0..PREFILTER_BASE_SLICES)
                    .filter(|slice| slice * cadence / PREFILTER_BASE_SLICES == frame)
                    .collect()
            })
            .collect();
        let slices: Vec<u32> = schedule.iter().flatten().copied().collect();
        assert_eq!(slices, (0..PREFILTER_BASE_SLICES).collect::<Vec<_>>());
        if cadence == 1 {
            assert_eq!(schedule[0].len(), PREFILTER_BASE_SLICES as usize);
        }
        if cadence == PREFILTER_BASE_SLICES {
            assert!(schedule.iter().all(|frame| frame.len() == 1));
        }
    }
}

/// `request_env_bake` arms `rebake_pending` only on a real change, matching the gate.
/// Device-backed (the full `Ibl` owns GPU handles); skipped without an ICD.
#[test]
fn request_env_bake_arms_only_on_change() {
    let Some((device, descriptors)) = device_or_skip() else {
        return;
    };
    let mut ibl = Ibl::new(&device, &descriptors).expect("ibl init");
    // The renderer's construction runs the first (procedural) bake right after `new`,
    // committing the default params as `baked`.
    ibl.bake(&device, true).expect("first bake");
    assert!(ibl.ready, "first bake must mark IBL ready");

    // Re-requesting the identical params arms nothing.
    ibl.request_env_bake(EnvSource::Procedural, None, SkygenParams::default());
    assert!(
        !ibl.rebake_pending,
        "identical request must not arm a re-bake"
    );

    // A changed sun arms the re-bake.
    let moved = SkygenParams {
        sun_dir: Vec3::new(0.1, 0.9, -0.4),
        ..SkygenParams::default()
    };
    ibl.request_env_bake(EnvSource::Procedural, None, moved);
    assert!(ibl.rebake_pending, "a changed sun must arm the re-bake");
}

/// The probe array seeds all 8 slots with the global IBL cubes at init: a validation-
/// clean `seed` over the full `MAX_REFLECTION_PROBES` array proves every slot binds a
/// valid cube before any capture. Device-backed; skipped without an ICD.
#[test]
fn probe_array_seeds_all_slots_validation_clean() {
    let Some((device, descriptors)) = device_or_skip() else {
        return;
    };
    let before = validation_issue_count();
    let ibl = Ibl::new(&device, &descriptors).expect("ibl init");
    let reflection = ReflectionProbes::new(&device).expect("probes init");
    // Seeds bindings 3 (prefiltered ×8) + 4 (irradiance ×8) + 5 (meta SSBO); a bad
    // slot or array element would trip a validation error.
    reflection.seed(&ibl);
    device.wait_idle().expect("idle");
    let after = validation_issue_count();
    assert_eq!(
        before, after,
        "seeding all {MAX_REFLECTION_PROBES} probe slots must be validation-clean"
    );
    assert_eq!(reflection.count(), 0, "no probes submitted yet");
}

/// `EnvSource` round-trips through the bake dispatch for all three variants: each baked
/// source completes a validation-clean convolution chain (Procedural skygen, Equirect
/// fallback-to-procedural with no panorama, Atmosphere LUT chain). Device-backed;
/// skipped without an ICD.
#[test]
fn env_source_round_trips_through_bake() {
    let Some((device, descriptors)) = device_or_skip() else {
        return;
    };
    let before = validation_issue_count();
    let mut ibl = Ibl::new(&device, &descriptors).expect("ibl init (procedural)");
    ibl.bake(&device, true).expect("startup bake");
    assert!(
        ibl.atmosphere_base_ready,
        "startup initializes the persistently bound atmosphere LUTs"
    );

    // Procedural: a re-bake exercises the retained front/back environment path.
    ibl.request_env_bake(
        EnvSource::Procedural,
        None,
        SkygenParams {
            sun_dir: Vec3::new(0.3, 0.7, 0.6),
            ..SkygenParams::default()
        },
    );
    assert!(ibl.rebake_pending);
    ibl.fire_rebake(&device).expect("procedural re-bake");
    drain_refresh(&mut ibl, &device);

    // Equirect with no panorama degrades to procedural — the bake must still succeed.
    ibl.request_env_bake(
        EnvSource::Equirect,
        None,
        SkygenParams {
            sun_dir: Vec3::new(-0.3, 0.7, 0.6),
            ..SkygenParams::default()
        },
    );
    ibl.fire_rebake(&device)
        .expect("equirect (fallback) re-bake");
    drain_refresh(&mut ibl, &device);

    // Atmosphere: the Hillaire LUT chain (transmittance → multiscatter → skyview →
    // skygen) feeding the env cube.
    let mut atmos = SkygenParams {
        sun_dir: Vec3::new(0.0, 0.2, 1.0),
        ..SkygenParams::default()
    };
    atmos.atmosphere.enabled = true;
    ibl.request_env_bake(EnvSource::Atmosphere, None, atmos);
    ibl.fire_rebake(&device).expect("atmosphere re-bake");
    drain_refresh(&mut ibl, &device);

    device.wait_idle().expect("idle");
    let after = validation_issue_count();
    assert_eq!(
        before, after,
        "all three EnvSource bakes must be validation-clean"
    );
}

#[test]
fn analytic_transmittance_blocks_the_planet_and_reddens_the_horizon() {
    let atmosphere = AtmosphereParams {
        enabled: true,
        ..AtmosphereParams::default()
    };
    assert_eq!(sun_transmittance(&atmosphere, Vec3::NEG_Y), Vec3::ZERO);
    let zenith = sun_transmittance(&atmosphere, Vec3::Y);
    let horizon = sun_transmittance(&atmosphere, Vec3::X);
    assert!(zenith.min_element() > 0.0);
    assert!(horizon.x / horizon.z > zenith.x / zenith.z);
}

/// `ProbeMetaGpu` matches the std430 layout the mesh fragment reads (48 bytes, three
/// 16-byte blocks). A wrong offset corrupts the probe sampling, not a compile error.
#[test]
fn probe_meta_std430_layout() {
    assert_eq!(size_of::<ProbeMetaGpu>(), 48);
    assert_eq!(std::mem::offset_of!(ProbeMetaGpu, origin_radius), 0);
    assert_eq!(std::mem::offset_of!(ProbeMetaGpu, extent_intensity), 16);
    assert_eq!(std::mem::offset_of!(ProbeMetaGpu, flags), 32);
}

/// `submit` arms a capture on a dirty/new probe and drops removed slots; `prepare_frame`
/// writes the selected frame slot's metadata SSBO. Device-backed; skipped without an ICD.
#[test]
fn submit_reflection_probes_tracks_dirty_and_count() {
    let Some((device, _descriptors)) = device_or_skip() else {
        return;
    };
    let mut reflection = ReflectionProbes::new(&device).expect("probes init");

    let probe = ReflectionProbeUpload {
        entity: 7,
        origin: Vec3::new(1.0, 2.0, 3.0),
        ..ReflectionProbeUpload::default()
    };
    reflection.submit(&[probe]);
    reflection.prepare_frame(0);
    assert_eq!(reflection.count(), 1);
    assert!(reflection.capture_pending, "a new probe must arm a capture");
    assert_eq!(reflection.frame_probe_count(), 1);

    // Disabling probes zeroes the sampled count even with an active slot.
    reflection.use_probes = false;
    reflection.submit(&[probe]);
    reflection.prepare_frame(0);
    assert_eq!(
        reflection.frame_probe_count(),
        0,
        "disabled probes contribute zero samples"
    );

    // Overflow past the cap is clamped (logged once).
    reflection.use_probes = true;
    let many = vec![ReflectionProbeUpload::default(); (MAX_REFLECTION_PROBES + 4) as usize];
    reflection.submit(&many);
    assert_eq!(reflection.count(), MAX_REFLECTION_PROBES);
}
