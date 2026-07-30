//! The clustered-cull geometry mirrored from `light_cull.slang`: the froxel AABB, the
//! light-intersection test, and the whole CPU cull, so the shader's math is testable with no device.

use super::*;

/// The 6 cube-face world→clip transforms for an omnidirectional point shadow at `pos`
/// with `far_plane`. A 90° perspective per face with the Vulkan Y-flip, in the +X, −X,
/// +Y, −Y, +Z, −Z face order.
pub fn point_shadow_face_matrices(pos: Vec3, far_plane: f32) -> [Mat4; 6] {
    // Cube faces render in the fixed cube-map sampling convention (GL-standard look-at directions +
    // up vectors), which is origin-agnostic — so, unlike the screen and 2D shadow-map projections,
    // the point-shadow projection takes no window Y-flip. Adding one vertically mirrors every face.
    let proj = Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, 1.0, 0.05, far_plane.max(0.1));
    let fwd = [
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(-1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(0.0, -1.0, 0.0),
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(0.0, 0.0, -1.0),
    ];
    let up = [
        Vec3::new(0.0, -1.0, 0.0),
        Vec3::new(0.0, -1.0, 0.0),
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(0.0, 0.0, -1.0),
        Vec3::new(0.0, -1.0, 0.0),
        Vec3::new(0.0, -1.0, 0.0),
    ];
    let mut result = [Mat4::IDENTITY; 6];
    for i in 0..6 {
        result[i] = proj * Mat4::look_at_rh(pos, pos + fwd[i], up[i]);
    }
    result
}

/// Maps a screen pixel + Vulkan NDC depth (0 = near) to a view-space point through the
/// clip→view inverse projection — the CPU mirror of `screenToView` in `light_cull.slang`.
/// `inverse_projection` is `proj.inverse()`; `screen` is the offscreen pixel dims.
pub fn screen_to_view(inverse_projection: Mat4, screen: [u32; 2], px: Vec3) -> Vec3 {
    let tex_x = px.x / screen[0] as f32;
    let tex_y = px.y / screen[1] as f32;
    let ndc = Vec4::new(tex_x * 2.0 - 1.0, tex_y * 2.0 - 1.0, px.z, 1.0);
    let view = inverse_projection * ndc;
    view.truncate() / view.w
}

/// Intersects the eye ray through `p` (eye at the view-space origin) with the plane
/// `z = z_dist` — the CPU mirror of `rayToZ` in `light_cull.slang`.
pub fn ray_to_z(p: Vec3, z_dist: f32) -> Vec3 {
    p * (z_dist / p.z)
}

/// Builds the view-space AABB of cluster `(gx, gy, gz)` for `params`, exactly as the
/// cull shader does: the screen tile's near-plane extent, the slice's exponential
/// view-space Z planes, and the four corner rays clamped to each Z. Returns
/// `(aabb_min, aabb_max)`. The CPU mirror of the AABB build in `light_cull.slang`.
pub fn cluster_aabb(params: &ClusterParams, gx: u32, gy: u32, gz: u32) -> (Vec3, Vec3) {
    let screen = [params.screen_size.x, params.screen_size.y];
    let grid = [
        params.grid_size.x as f32,
        params.grid_size.y as f32,
        params.grid_size.z as f32,
    ];
    let tile_w = screen[0] as f32 / grid[0];
    let tile_h = screen[1] as f32 / grid[1];
    let min_ss = (gx as f32 * tile_w, gy as f32 * tile_h);
    let max_ss = ((gx + 1) as f32 * tile_w, (gy + 1) as f32 * tile_h);

    let inv = params.inverse_projection;
    let min_near = screen_to_view(inv, screen, Vec3::new(min_ss.0, min_ss.1, 0.0));
    let max_near = screen_to_view(inv, screen, Vec3::new(max_ss.0, max_ss.1, 0.0));

    let near = params.z_planes.x;
    let far = params.z_planes.y;
    let tile_near = -near * (far / near).powf(gz as f32 / grid[2]);
    let tile_far = -near * (far / near).powf((gz + 1) as f32 / grid[2]);

    let a = ray_to_z(min_near, tile_near);
    let b = ray_to_z(max_near, tile_near);
    let c = ray_to_z(min_near, tile_far);
    let d = ray_to_z(max_near, tile_far);

    let aabb_min = a.min(b).min(c.min(d));
    let aabb_max = a.max(b).max(c.max(d));
    (aabb_min, aabb_max)
}

/// Whether a punctual light at world `light_pos` with `radius` intersects the cluster
/// AABB `[aabb_min, aabb_max]` (both view-space). The light is transformed to view space
/// by `params.view`, then a sphere-vs-AABB test — the CPU mirror of the cull loop's
/// per-light test in `light_cull.slang`.
pub fn light_intersects_cluster(
    params: &ClusterParams,
    light_pos: Vec3,
    radius: f32,
    aabb_min: Vec3,
    aabb_max: Vec3,
) -> bool {
    let pos_view = (params.view * light_pos.extend(1.0)).truncate();
    let closest = pos_view.clamp(aabb_min, aabb_max);
    let delta = pos_view - closest;
    delta.dot(delta) <= radius * radius
}

/// Runs the full clustered cull on the CPU for `params` + `lights`, returning each
/// cluster's light-index list (capped at [`MAX_LIGHTS_PER_CLUSTER`]). This is the exact
/// logic the `light_cull.slang` compute dispatch runs per froxel, extracted as pure CPU
/// code so the cull is testable with no device. Cluster index encoding:
/// `x + y*gridX + z*gridX*gridY`.
pub fn cull_clusters_cpu(params: &ClusterParams, lights: &[GpuLight]) -> Vec<Vec<u32>> {
    let gx = params.grid_size.x;
    let gy = params.grid_size.y;
    let gz = params.grid_size.z;
    let total = (gx * gy * gz) as usize;
    let mut out = vec![Vec::new(); total];
    let light_count = params.grid_size.w.min(lights.len() as u32);
    for cluster_index in 0..total as u32 {
        let cx = cluster_index % gx;
        let cy = (cluster_index / gx) % gy;
        let cz = cluster_index / (gx * gy);
        let (aabb_min, aabb_max) = cluster_aabb(params, cx, cy, cz);
        let list = &mut out[cluster_index as usize];
        for i in 0..light_count {
            let light = lights[i as usize];
            let pos = light.position_range.truncate();
            let radius = light.position_range.w;
            if light_intersects_cluster(params, pos, radius, aabb_min, aabb_max)
                && list.len() < MAX_LIGHTS_PER_CLUSTER as usize
            {
                list.push(i);
            }
        }
    }
    out
}
