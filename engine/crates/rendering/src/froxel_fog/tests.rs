use super::*;

/// The baked erosion noise tiles: opposite faces of the volume sample near-identically (the
/// periodic-lattice octaves wrap), and the fBm stays in range.
#[test]
fn tiling_noise_wraps_and_is_bounded() {
    let lo = tiling_fbm(0.0, 0.3, 0.7);
    let hi = tiling_fbm(1.0 - 1e-6, 0.3, 0.7);
    assert!((lo - hi).abs() < 0.05, "noise wraps across the tiling seam");
    for &p in &[0.0_f32, 0.25, 0.5, 0.75, 0.9] {
        let n = tiling_fbm(p, p * 0.5, 1.0 - p);
        assert!((0.0..=1.0).contains(&n), "fBm stays in [0,1]");
    }
}

fn camera_params() -> FogGridParams {
    // A 90° vertical FOV perspective; near 0.1, far 100 (the camera far the cull uses), the
    // 1600×900 the 16×9 cull grid's native pixel ratio, and the full 160×90×128 froxel grid.
    let proj = Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, 1600.0 / 900.0, 0.1, 100.0);
    FogGridParams {
        inverse_projection: proj.inverse(),
        inverse_view: Mat4::IDENTITY,
        prev_view_proj: Mat4::IDENTITY,
        grid_size: UVec4::new(FROXEL_GRID_X, FROXEL_GRID_Y, FROXEL_GRID_Z, 0),
        screen_size: Vec4::new(1600.0, 900.0, 0.0, 0.0),
        z_planes: Vec4::new(0.1, 100.0, FROXEL_FAR, 0.0),
        temporal: Vec4::new(0.05, 0.0, 0.0, 0.0),
        jitter: Vec4::ZERO,
    }
}

/// The froxel constants, format, and CPU depth mapping match the cull's exponential-Z partition
/// — a drift here silently mis-aligns a froxel from the cull cluster whose light list it reads.
/// The analog of `lighting::cluster_grid_matches_shader`.
#[test]
fn froxel_grid_matches_shader() {
    // 1. The froxel grid is finer than the cull grid in XY and Z; the tiers are as specified.
    const {
        assert!(FROXEL_GRID_X > CLUSTER_GRID_X && FROXEL_GRID_Y > CLUSTER_GRID_Y);
        assert!(FROXEL_GRID_Z > CLUSTER_GRID_Z);
    }
    assert_eq!(FroxelQuality::High.grid(), (160, 90, 128));
    assert_eq!(FroxelQuality::Medium.grid(), (160, 90, 64));
    assert_eq!(FroxelQuality::Low.grid(), (128, 72, 64));

    // 2. One rgba16f volume convention shared with the GDF albedo cache.
    assert_eq!(FROXEL_FORMAT, GDF_ALBEDO_FORMAT);

    // 3. The exponential Z curve pins slice 0 to -near and the last slice edge to -far, the
    //    identical curve `cluster_aabb` derives at the cull's slice count.
    let (near, far) = (0.1_f32, 100.0_f32);
    assert!((froxel_slice_view_z(near, far, 0, FROXEL_GRID_Z) - -near).abs() < 1e-4);
    assert!((froxel_slice_view_z(near, far, FROXEL_GRID_Z, FROXEL_GRID_Z) - -far).abs() < 1e-3);
    // The froxel curve at a matching fraction equals the cull curve (same distribution, finer
    // count): froxel slice at k/Nz == cull slice at the same fraction.
    let cull_mid = froxel_slice_view_z(near, far, CLUSTER_GRID_Z / 2, CLUSTER_GRID_Z);
    let froxel_mid = froxel_slice_view_z(near, far, FROXEL_GRID_Z / 2, FROXEL_GRID_Z);
    assert!(
        (cull_mid - froxel_mid).abs() < 1e-3,
        "same curve at the same fraction"
    );

    // 4. froxel_to_cluster lands each froxel in a valid cull cluster, is monotonic in Z, and
    //    spans the full cull Z range at the froxel-column endpoints — for every quality tier, so
    //    a resized grid keeps the CPU↔GPU exponential-Z mapping locked to the cull partition.
    let cull_count = CLUSTER_GRID_X * CLUSTER_GRID_Y * CLUSTER_GRID_Z;
    for quality in [
        FroxelQuality::Low,
        FroxelQuality::Medium,
        FroxelQuality::High,
    ] {
        let (gx, gy, gz) = quality.grid();
        let mut params = camera_params();
        params.grid_size = UVec4::new(gx, gy, gz, 0);
        let cx = gx / 2;
        let cy = gy / 2;

        let mut prev_z = 0u32;
        for fz in 0..gz {
            let cluster = froxel_to_cluster(&params, cx, cy, fz);
            assert!(
                cluster < cull_count,
                "{quality:?}: froxel maps into a valid cull cluster"
            );
            let z_slice = cluster / (CLUSTER_GRID_X * CLUSTER_GRID_Y);
            assert!(
                z_slice >= prev_z,
                "{quality:?}: cull Z slice is non-decreasing in froxel fz"
            );
            prev_z = z_slice;
        }
        // Near-most froxel → cull slice 0; far-most → the last cull slice.
        assert_eq!(
            froxel_to_cluster(&params, cx, cy, 0) / (CLUSTER_GRID_X * CLUSTER_GRID_Y),
            0,
            "{quality:?}: near froxel → cull slice 0"
        );
        assert_eq!(
            froxel_to_cluster(&params, cx, cy, gz - 1) / (CLUSTER_GRID_X * CLUSTER_GRID_Y),
            CLUSTER_GRID_Z - 1,
            "{quality:?}: far froxel → last cull slice"
        );
        // XY tiling maps the center froxel column into the center cull tile.
        let center = froxel_to_cluster(&params, cx, cy, 0);
        assert_eq!(center % CLUSTER_GRID_X, CLUSTER_GRID_X / 2);
        assert_eq!(
            (center / CLUSTER_GRID_X) % CLUSTER_GRID_Y,
            CLUSTER_GRID_Y / 2
        );
    }
}
