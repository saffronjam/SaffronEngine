//! GPU **adaptive tessellation** subsystem — the amplifying displacement path. Compute passes *amplify*
//! each base triangle of a displacement-enabled mesh into a watertight grid of displaced micro-geometry
//! written to a per-frame transient VB/IB, read by both the raster passes (indirect draw) and the RT
//! BLAS. This module owns the descriptor infrastructure + push layouts for the Phase-3 preparation
//! passes; the Phase-4 dice/emit kernel joins the same subsystem.
//!
//! Phase-3 passes, per displaced instance unless noted:
//! - **factor** (`tess_factor.slang`): one fractional tessellation factor per unique base edge, from
//!   the welded endpoints only — shared edges index one slot, so they cannot crack.
//! - **scan** (`tess_scan.slang`): predict each triangle's exact dice output counts, atomic-carry
//!   prefix-sum them to packed offsets inside the instance's worst-case-reserved slice.
//! - **finalize** (`tess_finalize.slang`): write the instance's indirect `VkDrawIndexedIndirectCommand`
//!   seed + its RT primitive count.
//! - **args** (`tess_args.slang`, once): the global `VkDispatchIndirectCommand` sizing the emit dispatch.
//!
//! - **emit** (`tessellate.slang`, Phase 4): one workgroup per base triangle dices it into the
//!   barycentric micro-grid at level `L`, Phong-smooths + displaces + welds each micro-vertex, and
//!   writes the amplified micro-vertices + generated index stream into the transient VB/IB. It also
//!   writes each micro-vertex's previous-frame position (the Phase-5 double-buffered per-edge factors on
//!   the same grid) into a parallel prev-VB, so the motion prepass reprojects the geomorph slide for TAA.
//!
//! The dice contract (Phase 3 defines, Phase 4 emits): the driving factor `m = max(f0,f1,f2)` (capped) is
//! resolved by the **split pass** ([`dice_plan`]) into `4^levels` barycentric subpatches, each diced at
//! `leaf_level = ceil(m / 2^levels)` clamped to `[1, TESS_MAX_DICE_FACTOR]`, giving `4^levels·(L+1)(L+2)/2`
//! vertices and `4^levels·L²` triangles. A triangle within the dice cap needs no split (`levels = 0`, one
//! leaf at `L = ceil(m)`). Sibling subpatches share their interior split edges bit-identically (parent-
//! relative dyadic midpoints); the parent's OUTER edges still weld with the neighbouring base triangle
//! because each boundary micro-vertex is snapped onto the SHARED per-edge factor's floor→ceil segment grid
//! (Phase-5 geomorph), a pure function of that edge's two endpoints regardless of either side's split depth.

use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::{Mat4, Vec3};

use crate::checked;
use crate::device::Device;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::resources::{Buffer, DeviceResources, GpuMesh};

/// Per-frame cap on tessellated instances (matching the displacement/skinning budgets). The global
/// micro-triangle ceiling (Phase-3 budget) bounds emitted geometry within this instance count.
pub const TESS_MAX_INSTANCES: u32 = 64;

/// Default hard per-edge dice cap until a control command tunes it. Generous by design: the split pass
/// expresses factors far beyond the CLAS leaf (`dice_plan` resolves 256 → 1024 subpatches × leaf 8), the
/// screen-space factor only *reaches* the cap where detail warrants it, and the hard budget
/// ([`budget_scaled_caps`]) coarsens instances whenever the summed worst case would exceed
/// [`TESS_MICRO_VERTEX_BUDGET`] — the budget, not this constant, is the real bound (the
/// vk_tessellated_clusters / Nanite pattern: a dice cap + unbounded-ish split + a pool clamp). A low cap
/// here was the "8-peak plane" defect: it strangled the split before the budget ever mattered.
pub const TESS_DEFAULT_FACTOR_CAP: f32 = 256.0;
/// Default lower factor clamp — never coarser than the base triangle.
pub const TESS_DEFAULT_MIN_FACTOR: f32 = 1.0;
/// Default LOD target: desired pixels per micro-edge. Nanite dices at ~2 px (its micropoly rasterizer
/// eats that); on our forward+ hardware-raster path 4 px is the perf-sane default — rich relief without
/// quad-occupancy collapse. Tunable live via `set-tessellation-quality`.
pub const TESS_DEFAULT_EDGE_LENGTH_TARGET: f32 = 4.0;

/// RT secondary-ray tessellation **coarsening** factor (Phase 10, Q2). Shadow / GI / reflection rays do
/// not need the primary view's ~1-triangle-per-pixel density, so the per-frame RT BLAS is built from a
/// **separate, coarser** run of the factor→scan→emit chain: the per-edge LOD target is multiplied by this
/// factor (fewer micro-edges) and the dice cap divided by it (a smaller worst-case reservation → far fewer
/// primitives in the portable per-frame BUILD). The coarse geometry is still Phong-smoothed, displaced, and
/// watertight (the same shared-edge factor snap), only lower-density — trading bit-exact raster/RT parity
/// for "displaced, close enough" on secondary rays. `2.0` cuts the RT reservation to ~`1/4` (verts grow
/// quadratically in the cap) while keeping the coarse surface faithful; raise toward `4.0` for a cheaper
/// build at coarser relief. Applied per instance in [`rt_coarsen_target`] / [`rt_coarsen_cap`].
pub const TESS_RT_COARSEN: f32 = 2.0;

/// The frame-global ceiling on emitted micro-vertices across all instances (the Phase-3 budget). The
/// transient VB/IB are reserved to this size regardless of per-instance worst cases summing higher; the
/// scan's global totals are read back to detect (and log) an overflow.
/// ~one micro-vertex per 1080p pixel. VRAM anchor: the budget bounds the transient VB (48 B/vert) + the
/// prev-position VB + the coarse RT chain, so 2 Mi ⇒ ~220 MiB worst-case grow across the pools — sane on
/// an 8 GiB card (8 Mi would have been ~0.9 GiB+).
pub const TESS_MICRO_VERTEX_BUDGET: u64 = 2 * 1024 * 1024;

/// Initial per-edge factor-buffer capacity (in `f32` edges), doubling grow-only from here.
const INITIAL_FACTOR_CAPACITY: u32 = 4096;

/// Max triangles per cluster for the NVIDIA CLAS fast path (`VK_NV_cluster_acceleration_structure`,
/// Phase 8). Aligning the dice/split leaf size to this hardware cap now lets the CLAS backend bolt on
/// later with no re-clustering; the portable BLAS floor is unaffected by it.
pub const TESS_CLAS_MAX_TRIS: u32 = 128;
/// Max vertices per cluster for the CLAS fast path (the paired hardware cap).
pub const TESS_CLAS_MAX_VERTS: u32 = 256;

/// The largest integer dice level whose diced leaf fits the CLAS caps: `L² ≤ 128 tris` and
/// `(L+1)(L+2)/2 ≤ 256 verts` ⇒ `L = 11` (121 tris, 78 verts). A base triangle whose desired factor
/// exceeds this is split (see [`split_recursion`]) so every emitted leaf is a valid CLAS cluster.
pub const TESS_MAX_DICE_FACTOR: u32 = 11;

/// Split recursion for a base triangle whose desired tessellation `factor` exceeds `max_dice_factor`:
/// subdivide (4 sub-triangles via edge midpoints per level) until each leaf's factor ≤ the cap, then the
/// existing barycentric micro-grid dices the leaf. Returns `(split_levels, leaf_factor)` — `4^levels`
/// leaves, each diced at `leaf_factor`. Karis's guidance: prefer a large dice factor over deep splitting
/// (fewer, better-shaped leaves), so this splits only as far as the cap requires. A factor already within
/// the cap needs no split (`(0, factor)`). Pure + testable; the shared contract for the GPU split pass.
pub fn split_recursion(factor: f32, max_dice_factor: u32) -> (u32, f32) {
    let cap = max_dice_factor.max(1) as f32;
    if factor <= cap {
        return (0, factor);
    }
    let levels = (factor / cap).log2().ceil().max(0.0) as u32;
    let leaf = factor / (1u32 << levels) as f32;
    (levels, leaf)
}

/// The largest split depth the GPU split pass enumerates. `4^levels` subpatches per base triangle, so the
/// dispatch + reservation growth is bounded and the `1 << (2 * levels)` subpatch count never overflows a
/// `u32`. The budget ([`budget_scaled_caps`]) keeps `factor_cap` far below the depth that would reach this;
/// it is purely a numerical-safety clamp (a factor of `11 * 2^15 ≈ 360k` px/edge would be needed to hit it).
pub const TESS_MAX_SPLIT_LEVELS: u32 = 15;

/// The GPU **dice plan** for a base triangle driven at `factor`, capped at [`TESS_MAX_DICE_FACTOR`]: how
/// many `4^levels` barycentric subpatches the split pass enumerates and the integer leaf dice level each is
/// diced at. Consumes [`split_recursion`] and rounds the leaf up to an integer grid level (`ceil`, matching
/// the `tess_scan` dice contract), clamped to `[1, TESS_MAX_DICE_FACTOR]` so every emitted leaf is a valid
/// CLAS-sized cluster (`leaf² ≤ 128` tris, `(leaf+1)(leaf+2)/2 ≤ 256` verts). Returns `(subpatch_count,
/// leaf_level)`. This is the exact contract the Slang `dicePlan` in `tess_scan.slang` / `tessellate.slang`
/// mirrors, so the CPU reservation and the GPU scan/emit counts cannot diverge. Pure + testable.
pub fn dice_plan(factor: f32) -> (u32, u32) {
    let (levels, leaf) = split_recursion(factor, TESS_MAX_DICE_FACTOR);
    let levels = levels.min(TESS_MAX_SPLIT_LEVELS);
    let leaf_level = (leaf.ceil() as u32).clamp(1, TESS_MAX_DICE_FACTOR);
    (1u32 << (2 * levels), leaf_level)
}

/// Worst-case output of dicing one base triangle whose driving factor is capped at `factor_cap`. The split
/// pass ([`dice_plan`]) resolves the cap into `subpatches = 4^levels` barycentric subpatches each diced at
/// `leaf_level ≤ TESS_MAX_DICE_FACTOR`; a grid at level `L` yields `(L+1)(L+2)/2` vertices and `L²`
/// triangles, so the per-base-triangle worst case is `subpatches ×` that. Used to reserve the transient
/// VB/IB up front (the exact prefix-sum only packs *inside* this reservation). The count is monotone
/// non-decreasing in `factor_cap`, so reserving at the cap bounds any per-triangle factor `≤ cap`. Returned
/// as `(vertices, indices)`. A `factor_cap ≤ TESS_MAX_DICE_FACTOR` needs no split (one leaf), so the
/// no-split reservation is unchanged.
pub fn tess_worst_case(base_prims: u32, factor_cap: u32) -> (u64, u64) {
    let (subpatches, leaf_level) = dice_plan(factor_cap.max(1) as f32);
    let l = leaf_level as u64;
    let sub = subpatches as u64;
    let verts_per = sub * (l + 1) * (l + 2) / 2;
    let tris_per = sub * l * l;
    let prims = base_prims as u64;
    (prims * verts_per, prims * tris_per * 3)
}

/// Hermite smoothstep on `[0,1]` (`3t² − 2t³`) — monotone, C1 at both ends. The shared morph curve for
/// the geomorph blend; kept as a named helper so the emit kernel and the CPU test evaluate the same ramp.
pub fn smoothstep01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Geomorph blend weight (Phase 5) for a diced micro-vertex, from the Phase-3 fractional factor. The
/// continuous tessellation factor is `f = level + remainder` (`remainder ∈ [0,1)`); `levels_since_birth`
/// is `current_level − birth_level`, the number of integer subdivision levels a vertex has existed.
///
/// A vertex born at the current top level (`levels_since_birth == 0`) is an "odd" vertex still morphing
/// in: it blends from its **coarse-parent** position (`w = 0` — the linear midpoint of its two even
/// neighbours, which lies on the lower-level surface, so a newborn appears with no pop) to its **fine**
/// diced + displaced position (`w = 1`) as `remainder` sweeps `0 → 1`. A vertex present at a lower level
/// (`levels_since_birth ≥ 1`) is fully resolved (`w = 1`).
///
/// C0 across an integer boundary: a just-born vertex at `remainder → 1⁻` reads `w → 1`, and the same
/// sample one level up (`levels_since_birth == 1`) reads `w = 1` — no discontinuity, so a factor
/// transition is a smooth motion, not a topology pop. The smoothstep ramp additionally matches velocity
/// (C1) at the boundary, keeping the motion vector continuous for TAA. The emit kernel evaluates this
/// same weight on both the current and the previous streams so a geomorph is a small cur/prev delta.
pub fn geomorph_weight(remainder: f32, levels_since_birth: u32) -> f32 {
    if levels_since_birth >= 1 {
        1.0
    } else {
        smoothstep01(remainder)
    }
}

/// The **interior-geomorph coarse-parent lookup** (Phase 10) — CPU mirror of the emit kernel's
/// `coarseParentPosition`. A triangle dices at `L = ceil(maxFactor)`; an interior micro-vertex morphs from
/// the coarser `L-1` approximation toward the fine `L` surface as the fractional factor sweeps `0 → 1`
/// (weighted by [`smoothstep01`]), so a diced row appearing as the camera dollies in is a smooth morph,
/// not a facet pop. The coarse-parent position is the linear interpolation across the `L-1` micro-triangle
/// that contains the vertex's barycentric point: this function locates that micro-triangle and returns its
/// three corner grid vertices (as integer `(i, j)` coords on the `level = L-1` grid) each paired with the
/// point's local barycentric weight there. The kernel evaluates the displaced surface at the three corners
/// and blends by these weights.
///
/// `a`, `b` are the corner-1 / corner-2 barycentric weights of the point over the base triangle
/// (`w = (1-a-b, a, b)`), with `a, b ≥ 0` and `a + b ≤ 1`; `level ≥ 1`. The returned weights sum to 1 and
/// reconstruct the point in coarse-grid coords (`Σ wᵢ·(iᵢ, jᵢ) = (a·level, b·level)`). Interior samples
/// have `a + b < 1`, so `I + J < level` and every returned corner (including the down-triangle apex) is a
/// valid grid vertex. Pure + testable: the barycentric location the shader and this test must agree on.
///
/// Exactness note: successive integer dice levels are *distinct* barycentric grids, not nested
/// refinements, so the morph is continuous only up to the sub-facet error of a fine micro-triangle
/// straddling a coarse facet crease (bounded by per-facet curvature, vanishing as `L` grows) — visually
/// continuous, not bit-exact. This is inherent to integer-`L` dicing, not a defect of the lookup.
pub fn coarse_parent_bary(a: f32, b: f32, level: u32) -> [(u32, u32, f32); 3] {
    let level_f = level.max(1) as f32;
    let bi = a * level_f; // corner-1 axis, coarse-grid coords
    let bj = b * level_f; // corner-2 axis, coarse-grid coords
    let i0 = bi.floor();
    let j0 = bj.floor();
    let fi = bi - i0;
    let fj = bj - j0;
    let (i0, j0) = (i0 as u32, j0 as u32);
    if fi + fj <= 1.0 {
        // Up-pointing micro-triangle: corners (i0,j0), (i0+1,j0), (i0,j0+1).
        [(i0, j0, 1.0 - fi - fj), (i0 + 1, j0, fi), (i0, j0 + 1, fj)]
    } else {
        // Down-pointing micro-triangle: corners (i0+1,j0), (i0,j0+1), (i0+1,j0+1).
        [
            (i0 + 1, j0, 1.0 - fj),
            (i0, j0 + 1, 1.0 - fi),
            (i0 + 1, j0 + 1, fi + fj - 1.0),
        ]
    }
}

/// Screen-pixel extent of a world-space length seen at `dist` view-space depth — the small-angle form of
/// the factor kernel's projection (`pixels = (worldLen / dist) / tan(½fovY) · viewportH / 2`). The base
/// edge uses the exact subtended arc; the displacement amplitude added by [`displacement_aware_factor`]
/// is small enough that this small-angle projection matches it. Shared with the factor kernel so the CPU
/// test covers the shipped metric. Distance falls out here — "distance LOD" is not a separate path.
pub fn project_world_to_pixels(
    world_len: f32,
    dist: f32,
    tan_half_fov_y: f32,
    viewport_h: f32,
) -> f32 {
    let dist = dist.max(1e-4);
    let tan = tan_half_fov_y.max(1e-4);
    (world_len / dist) / tan * viewport_h * 0.5
}

/// The **displacement-aware** per-edge tessellation factor (Phase 10). The base metric measures the flat
/// base edge only, so a flat surface under a tall/high-frequency height field undersamples the *displaced*
/// surface (the "spiky plane"). The displaced patch spans the base edge **plus** the local displacement
/// range along the normal, so the tessellation must resolve whichever is larger: the factor is driven by
/// `max(base-edge px, displacement-range px)`. `disp_amp_px` is the projected local displacement range
/// (from the material `height_scale` in the pyramid-free v1; from the min-max pyramid's local range once
/// the pyramid is GPU-resident). Clamped to `[min_factor, factor_cap]`. Both terms depend only on the two
/// shared endpoints (+ the shared local range), so a shared edge stays bit-identical → crack-free.
pub fn displacement_aware_factor(
    base_edge_px: f32,
    disp_amp_px: f32,
    edge_length_target: f32,
    min_factor: f32,
    factor_cap: f32,
) -> f32 {
    let target = edge_length_target.max(1e-4);
    let driving_px = base_edge_px.max(disp_amp_px);
    (driving_px / target).clamp(min_factor, factor_cap)
}

/// The **hard triangle budget** (Phase 10): scale each instance's factor cap down so the *summed*
/// worst-case micro-vertex reservation fits `vert_budget`, coarsening under pressure. Worst-case vertices
/// grow ~quadratically in the cap (`≈ tri·L²/2`), so a proportional fit scales every cap by
/// `√(budget/total)`; a couple of iterations tighten the +L linear terms. Each cap is floored at its
/// `min_factor` (never coarser than requested) — so a scene that cannot fit even at the floors is
/// reported by the caller, never silently overrun. The reservation, scan, and emit all consume these
/// adjusted caps, so the GPU can never write past the reserved arena. Pure + testable; input is one
/// `(tri_count, factor_cap, min_factor)` per instance, output the adjusted caps in the same order.
pub fn budget_scaled_caps(instances: &[(u32, f32, f32)], vert_budget: u64) -> Vec<f32> {
    let total = |caps: &[f32]| -> u64 {
        instances
            .iter()
            .zip(caps)
            .map(|(&(tris, _, _), &cap)| tess_worst_case(tris, cap.max(1.0) as u32).0)
            .sum()
    };
    let mut caps: Vec<f32> = instances.iter().map(|&(_, cap, _)| cap).collect();
    if vert_budget == 0 || total(&caps) <= vert_budget {
        return caps;
    }
    // A few proportional shrink passes converge the fit (the closed form is exact only for the L² term).
    for _ in 0..4 {
        let t = total(&caps);
        if t <= vert_budget {
            break;
        }
        let scale = (vert_budget as f64 / t as f64).sqrt() as f32;
        for (cap, &(_, _, min_factor)) in caps.iter_mut().zip(instances) {
            *cap = (*cap * scale).max(min_factor.max(1.0)).floor().max(1.0);
        }
    }
    caps
}

/// The RT-coarsened per-edge LOD target (Phase 10, Q2): the raster `edge_length_target` scaled up by
/// [`TESS_RT_COARSEN`], so each RT micro-edge is allowed to span more pixels and the factor kernel emits a
/// coarser dice for the secondary-ray BLAS. Pure; the shared CPU/GPU contract fed into the RT factor push.
pub fn rt_coarsen_target(edge_length_target: f32) -> f32 {
    edge_length_target * TESS_RT_COARSEN.max(1.0)
}

/// The RT-coarsened dice cap (Phase 10, Q2): the (budget-scaled) raster `factor_cap` divided by
/// [`TESS_RT_COARSEN`] and rounded up, floored at `min_factor` (never coarser than requested) and at `1`
/// (always at least the base triangle — displaced, never flat). Because the coarse factors are already
/// ~`1/COARSEN` of the raster ones, this cap only re-clips what the raster cap would, scaled; it bounds the
/// RT worst-case reservation ([`tess_worst_case`]) to ~`1/COARSEN²` of the raster arena, so the per-frame
/// RT BUILD reads far fewer primitives. Pure + testable; consumed by the RT scan clamp + reservation.
pub fn rt_coarsen_cap(factor_cap: f32, min_factor: f32) -> f32 {
    (factor_cap / TESS_RT_COARSEN.max(1.0))
        .ceil()
        .max(min_factor.max(1.0))
}

/// One displaced instance to tessellate: its base mesh (carrying the Phase-2 conditioning buffers), its
/// world transform, the material displacement params, and the per-instance budget. Gathered from each
/// displacement-enabled draw item; the amplified transient geometry it emits is what every raster + RT
/// consumer reads for that mesh.
pub struct TessBucket {
    /// The base mesh; [`GpuMesh::conditioning`] supplies the welded/edges/tri-edges buffers.
    pub mesh: Arc<GpuMesh>,
    /// The base offset into the frame's instance buffer for this instance's submesh-major block —
    /// the `firstInstance` the tessellated indirect draw must seed, and the key that links this
    /// bucket to its [`crate::TessSceneDraw`] when `record_tess_prep` resolves the draw handles.
    pub base_instance: u32,
    /// The entity id, keying this bucket to its [`crate::DeformedRtInstance`] so `record_tess_prep`
    /// can fill the RT tessellated slice (and the per-entity `TessellatedBlas`). `0` when RT is unarmed.
    pub entity: u64,
    /// This instance's world transform.
    pub model: Mat4,
    /// Bindless index of the height map (Phase-4 dice samples it).
    pub height_index: u32,
    /// Local-space displacement amplitude.
    pub height_scale: f32,
    /// `tiling.xy, offset.xy`.
    pub uv_transform: [f32; 4],
    /// Bindless index of the vector-displacement map (`0` = scalar-only).
    pub vector_index: u32,
    /// Hard per-edge factor cap (bounds the reservation).
    pub factor_cap: f32,
    /// Lower factor clamp (>= 1).
    pub min_factor: f32,
    /// Desired pixels per micro-edge (the LOD target).
    pub edge_length_target: f32,
}

/// The CPU-computed placement of one instance inside the shared transient buffers + descriptor rows.
#[derive(Clone, Copy, Debug, Default)]
pub struct TessInstanceLayout {
    /// Base offset (in edges) into the shared factor buffer.
    pub factor_base: u32,
    /// Base offset (in triangles) into the shared per-triangle record buffer.
    pub tri_base: u32,
    /// This instance's row (index of its 2-uint counter slot / draw seed / prim count).
    pub instance_row: u32,
    /// Base offset (in vertices) into the shared transient VB — the draw's `vertexOffset`.
    pub vertex_base: u32,
    /// Base offset (in indices) into the shared transient IB — the draw's `firstIndex`.
    pub index_base: u32,
}

/// The camera + viewport the factor kernel needs, gathered once per frame.
#[derive(Clone, Copy)]
pub struct TessCamera {
    /// The main-camera view-projection.
    pub view_proj: Mat4,
    /// World-space camera position.
    pub cam_pos: Vec3,
    /// Render-target extent in pixels.
    pub viewport: [f32; 2],
    /// `tan(0.5 * vertical FOV)`.
    pub tan_half_fov_y: f32,
    /// Camera near-plane distance.
    pub near: f32,
}

struct FrameTess {
    pool: vk::DescriptorPool,
}

/// The tessellation subsystem: the five compute set layouts (factor/scan/finalize/args prep + the
/// Phase-4 emit) + a per-frame descriptor pool. The transient VB/IB/args/count buffers are owned by
/// [`crate::transient::RenderGraphResources`]; this wires the dispatches that write them.
pub struct Tessellation {
    resources: Arc<DeviceResources>,
    factor_layout: vk::DescriptorSetLayout,
    scan_layout: vk::DescriptorSetLayout,
    finalize_layout: vk::DescriptorSetLayout,
    args_layout: vk::DescriptorSetLayout,
    emit_layout: vk::DescriptorSetLayout,
    frames: Vec<FrameTess>,
    /// The persistent double-buffered per-edge factor store (Phase-5 temporal): two grow-only slots,
    /// **not** the rewound transient pool, so last frame's factors survive to drive the prev geomorph.
    /// Frame `f` writes cur factors into `slot[f % 2]` and reads prev from `slot[(f + 1) % 2]` — the
    /// same 2-slot parity the TAA history ping-pong uses (in lockstep with the frame-in-flight index).
    factor_slots: [Option<Buffer>; 2],
    /// Grow-only capacity of each factor slot, in `f32` edges.
    factor_capacity: [u32; 2],
    /// The per-instance edge-count sequence last written into each slot — the prev slot's factors are
    /// aligned with this frame only when this sequence matches, else the prev binding falls back to cur
    /// (prev == cur ⇒ zero geomorph motion, no ghost) rather than reprojecting through a stale layout.
    factor_layout_sig: [Vec<u32>; 2],
    /// Factor slots superseded by a grow, held for [`MAX_FRAMES_IN_FLIGHT`] `begin_frame`s before free so
    /// an in-flight frame reading the old slot as its prev stream never sees a use-after-free.
    retired_factors: Vec<(Buffer, usize)>,
}

impl Tessellation {
    /// Creates the five set layouts (3/5/3/2/9 compute storage buffers) and one descriptor pool per
    /// frame-in-flight, sized for [`TESS_MAX_INSTANCES`] instances plus the global args set.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if a layout or pool cannot be created.
    pub fn new(device: &Device) -> crate::Result<Self> {
        let raw = device.resources().device();
        let factor_layout = storage_set_layout(raw, 3)?;
        let cleanup = |raw: &ash::Device, layouts: &[vk::DescriptorSetLayout]| {
            for &l in layouts {
                // SAFETY: the ash seam. Each layout was created above; freed once.
                unsafe { raw.destroy_descriptor_set_layout(l, None) };
            }
        };
        let scan_layout = match storage_set_layout(raw, 5) {
            Ok(l) => l,
            Err(err) => {
                cleanup(raw, &[factor_layout]);
                return Err(err);
            }
        };
        let finalize_layout = match storage_set_layout(raw, 3) {
            Ok(l) => l,
            Err(err) => {
                cleanup(raw, &[factor_layout, scan_layout]);
                return Err(err);
            }
        };
        let args_layout = match storage_set_layout(raw, 2) {
            Ok(l) => l,
            Err(err) => {
                cleanup(raw, &[factor_layout, scan_layout, finalize_layout]);
                return Err(err);
            }
        };
        // The emit set: base VB + base IB + perTri + triEdges + factors + out VB + out IB + prev factors +
        // prev out VB (the Phase-5 temporal prev-stream — same slice layout, previous frame's factors).
        let emit_layout = match storage_set_layout(raw, 9) {
            Ok(l) => l,
            Err(err) => {
                cleanup(
                    raw,
                    &[factor_layout, scan_layout, finalize_layout, args_layout],
                );
                return Err(err);
            }
        };
        let all = [
            factor_layout,
            scan_layout,
            finalize_layout,
            args_layout,
            emit_layout,
        ];
        let mut frames: Vec<FrameTess> = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            let pool = match create_tess_pool(raw) {
                Ok(pool) => pool,
                Err(err) => {
                    for frame in &frames {
                        // SAFETY: the ash seam. Each pool created above; freed once.
                        unsafe { raw.destroy_descriptor_pool(frame.pool, None) };
                    }
                    cleanup(raw, &all);
                    return Err(err);
                }
            };
            frames.push(FrameTess { pool });
        }
        Ok(Self {
            resources: Arc::clone(device.resources()),
            factor_layout,
            scan_layout,
            finalize_layout,
            args_layout,
            emit_layout,
            frames,
            factor_slots: [None, None],
            factor_capacity: [0, 0],
            factor_layout_sig: [Vec::new(), Vec::new()],
            retired_factors: Vec::new(),
        })
    }

    /// The factor set layout (3 storage buffers), for building the `tess_factor` PSO.
    pub fn factor_layout(&self) -> vk::DescriptorSetLayout {
        self.factor_layout
    }
    /// The scan set layout (5 storage buffers), for building the `tess_scan` PSO.
    pub fn scan_layout(&self) -> vk::DescriptorSetLayout {
        self.scan_layout
    }
    /// The finalize set layout (3 storage buffers), for building the `tess_finalize` PSO.
    pub fn finalize_layout(&self) -> vk::DescriptorSetLayout {
        self.finalize_layout
    }
    /// The args set layout (2 storage buffers), for building the `tess_args` PSO.
    pub fn args_layout(&self) -> vk::DescriptorSetLayout {
        self.args_layout
    }
    /// The emit set layout (9 storage buffers: base VB/IB + perTri + triEdges + factors + out VB/IB +
    /// prev factors + prev out VB), for building the `tessellate` PSO (paired with the bindless set 0).
    pub fn emit_layout(&self) -> vk::DescriptorSetLayout {
        self.emit_layout
    }

    /// Ensures factor `slot` (0 or 1) holds at least `edge_count` `f32`s (grow-only, doubling), and
    /// returns its buffer handle. A grow retires the old allocation for [`MAX_FRAMES_IN_FLIGHT`] frames
    /// (an in-flight frame may still be reading it as its prev stream) rather than freeing it inline.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if the buffer allocation fails.
    pub fn ensure_factor_slot(
        &mut self,
        slot: usize,
        edge_count: u32,
    ) -> crate::Result<vk::Buffer> {
        let needed = edge_count.max(1);
        if self.factor_slots[slot].is_some() && self.factor_capacity[slot] >= needed {
            return Ok(self.factor_slots[slot]
                .as_ref()
                .expect("slot present")
                .handle());
        }
        let capacity = grow_factor_capacity(self.factor_capacity[slot], needed);
        let size = u64::from(capacity) * size_of::<f32>() as u64;
        let usage = vk::BufferUsageFlags::STORAGE_BUFFER;
        let alloc = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let buffer = Buffer::new(&self.resources, size, usage, &alloc)?;
        if let Some(old) = self.factor_slots[slot].take() {
            // Held past the fences of every frame that could still read the old slot as its prev stream.
            self.retired_factors.push((old, MAX_FRAMES_IN_FLIGHT));
            // The old contents are gone, so the stored layout no longer describes this slot's buffer.
            self.factor_layout_sig[slot].clear();
        }
        let handle = buffer.handle();
        self.factor_slots[slot] = Some(buffer);
        self.factor_capacity[slot] = capacity;
        tracing::debug!(
            "tessellation: factor slot {slot} grew to {capacity} edges ({} KiB)",
            size / 1024
        );
        Ok(handle)
    }

    /// The buffer handle of factor `slot`, or `None` before its first grow.
    pub fn factor_slot(&self, slot: usize) -> Option<vk::Buffer> {
        self.factor_slots[slot].as_ref().map(Buffer::handle)
    }

    /// Whether factor `slot`'s last-written per-instance edge-count sequence equals `sig` — i.e. the
    /// slot's surviving factors are aligned with this frame's slice layout, so it can serve as the prev
    /// stream. A mismatch (or an empty signature: never written / just grown) means fall back to cur.
    pub fn factor_layout_matches(&self, slot: usize, sig: &[u32]) -> bool {
        !self.factor_layout_sig[slot].is_empty() && self.factor_layout_sig[slot] == sig
    }

    /// Records the per-instance edge-count sequence just written into factor `slot`, so next frame's
    /// prev-stream lookup can decide whether the slot is layout-aligned. Call only once the frame's
    /// factor pass is committed (its GPU write into the slot is scheduled).
    pub fn set_factor_layout(&mut self, slot: usize, sig: Vec<u32>) {
        self.factor_layout_sig[slot] = sig;
    }

    /// Resets this frame's descriptor pool at frame begin (after the slot's fence). Call before
    /// wiring the frame's dispatches. Also ages out the retired factor buffers: each grow-superseded
    /// slot is freed once [`MAX_FRAMES_IN_FLIGHT`] frames have elapsed, so every frame that could still
    /// read it as its prev stream has completed.
    pub fn begin_frame(&mut self, frame: usize) {
        self.retired_factors.retain_mut(|(_, remaining)| {
            *remaining = remaining.saturating_sub(1);
            *remaining > 0
        });
        let raw = self.resources.device();
        // SAFETY: the ash seam. This slot's prior GPU work was awaited, so its sets recycle.
        if let Err(result) = unsafe {
            raw.reset_descriptor_pool(
                self.frames[frame].pool,
                vk::DescriptorPoolResetFlags::empty(),
            )
        } {
            tracing::error!("tessellation: reset pool failed: {result:?}");
        }
    }
}

impl Drop for Tessellation {
    fn drop(&mut self) {
        let raw = self.resources.device();
        for frame in &self.frames {
            // SAFETY: the ash seam. Each pool created in `new`; freed once.
            unsafe { raw.destroy_descriptor_pool(frame.pool, None) };
        }
        for &l in &[
            self.factor_layout,
            self.scan_layout,
            self.finalize_layout,
            self.args_layout,
            self.emit_layout,
        ] {
            // SAFETY: the ash seam. Each layout created in `new`; freed once after the pools.
            unsafe { raw.destroy_descriptor_set_layout(l, None) };
        }
    }
}

/// The `tess_factor` push (128 B) — matches `tess_factor.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TessFactorPush {
    /// `view_proj * model`.
    pub mvp: [[f32; 4]; 4],
    /// World camera in this instance's local space (xyz); `w` carries the material's uv tiling.x (the
    /// pyramid span is texture-space — see `uv_tiling_y`).
    pub cam_pos_local: [f32; 4],
    /// Render-target extent in pixels.
    pub viewport: [f32; 2],
    /// `tan(0.5 * vertical FOV)`.
    pub tan_half_fov_y: f32,
    /// Camera near distance.
    pub near: f32,
    /// Desired pixels per micro-edge.
    pub edge_length_target: f32,
    /// Hard per-edge factor cap.
    pub factor_cap: f32,
    /// Lower factor clamp.
    pub min_factor: f32,
    /// Unique edges in this instance's mesh.
    pub edge_count: u32,
    /// This instance's base offset into the factor buffer.
    pub factor_base: u32,
    /// LOCAL-space displacement amplitude (`height_scale`, the material's object-space amplitude) — the
    /// extent along the normal for a *full* `[0,1]` height range. The kernel scales it by the per-edge
    /// local height range sampled from the min/max pyramid (Phase 10 per-region adaptivity), so a flat
    /// base under a busy — not merely tall — height field still refines.
    pub disp_amp_local: f32,
    /// Bindless slot of the height map, addressing its min/max pyramid (binding 4) so the kernel samples
    /// the local range over each edge's UV span.
    pub height_index: u32,
    /// Material uv tiling.y (`tiling.x` rides `cam_pos_local[3]`): a tiled material's edge sweeps
    /// `tiling×` more height texels, so the pyramid span/samples are taken in texture space.
    pub uv_tiling_y: f32,
}

/// The `tess_scan` push (32 B) — matches `tess_scan.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TessScanPush {
    /// Base triangles in this instance's mesh.
    pub tri_count: u32,
    /// This instance's base offset into the per-triangle record buffer.
    pub tri_base: u32,
    /// This instance's base index into the per-instance counters (= 2 * row).
    pub counter_base: u32,
    /// Hard clamp on the driving factor before the split pass resolves it into subpatches + leaf level.
    pub factor_cap: f32,
    pub _pad: [u32; 4],
}

/// The `tess_finalize` push (32 B) — matches `tess_finalize.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TessFinalizePush {
    /// = 2 * instance row (into the counters).
    pub counter_base: u32,
    /// = 5 * instance row (into the draw seeds).
    pub seed_base: u32,
    /// This instance's reserved vertex slice base.
    pub vertex_base: u32,
    /// This instance's reserved index slice base.
    pub index_base: u32,
    /// Prim-count slot index (the tess instance row).
    pub instance_row: u32,
    /// The draw's `firstInstance` — the batch's `base_instance` (submesh-major instance row).
    pub first_instance: u32,
    pub _pad: [u32; 2],
}

/// The `tessellate` emit push (64 B) — matches `tessellate.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TessEmitPush {
    /// This instance's base offset into perTri / triEdges.
    pub tri_base: u32,
    /// This instance's reserved vertex slice base (the draw's `vertexOffset`).
    pub vertex_base: u32,
    /// This instance's reserved index slice base (the draw's `firstIndex`).
    pub index_base: u32,
    /// Bindless index of the height map.
    pub height_index: u32,
    /// LOCAL-space displacement amplitude — the offset rides the instance's model matrix like every
    /// other vertex, so gizmo-scaling an object scales its relief proportionally (the Blender / Arnold /
    /// RenderMan / Nanite object-space convention).
    pub height_scale: f32,
    /// Hard clamp on the driving factor (matches Phase 3); the split pass resolves it into subpatches +
    /// leaf level. Also the cap `snapEdgeParam` clamps the shared per-edge factor to for the outer-edge
    /// weld, so a split triangle's boundary is diced at the full shared factor (not the leaf cap).
    pub factor_cap: f32,
    /// Bindless index of the vector-displacement map (`0` = scalar).
    pub vector_index: u32,
    pub _pad0: u32,
    /// `tiling.xy, offset.xy`.
    pub uv_transform: [f32; 4],
    /// This instance's base offset into the factor buffer (for boundary snapping).
    pub factor_base: u32,
    pub _pad: [u32; 3],
}

/// Builds the `tess_factor` push for an instance: `mvp`/`cam_pos_local` are derived from the camera +
/// the instance `model` on the CPU (so the kernel needs no camera UBO). The metric is exact for uniform
/// scale (the accepted v1 compromise for the local-space angular arc). `factor_base` is the instance's
/// slice base into the factor buffer; `edge_count` its unique-edge count.
#[allow(clippy::too_many_arguments)]
pub fn factor_push(
    cam: &TessCamera,
    model: Mat4,
    factor_cap: f32,
    min_factor: f32,
    edge_length_target: f32,
    factor_base: u32,
    edge_count: u32,
    disp_amp_local: f32,
    height_index: u32,
    uv_tiling: [f32; 2],
) -> TessFactorPush {
    let mvp = cam.view_proj * model;
    let cam_local = model.inverse().transform_point3(cam.cam_pos);
    TessFactorPush {
        mvp: mvp.to_cols_array_2d(),
        cam_pos_local: [cam_local.x, cam_local.y, cam_local.z, uv_tiling[0]],
        viewport: cam.viewport,
        tan_half_fov_y: cam.tan_half_fov_y,
        near: cam.near,
        edge_length_target,
        factor_cap,
        min_factor,
        edge_count,
        factor_base,
        disp_amp_local,
        height_index,
        uv_tiling_y: uv_tiling[1],
    }
}

/// Grow-only factor capacity (in `f32` edges): keep the current size if it already fits, else double
/// until it does (never shrink), starting from [`INITIAL_FACTOR_CAPACITY`].
fn grow_factor_capacity(current: u32, needed: u32) -> u32 {
    let mut capacity = if current == 0 {
        INITIAL_FACTOR_CAPACITY
    } else {
        current
    };
    while capacity < needed {
        capacity *= 2;
    }
    capacity
}

/// The compute set layout of `count` compute-stage storage buffers, matching the tess shaders' set 0.
fn storage_set_layout(raw: &ash::Device, count: u32) -> crate::Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..count)
        .map(|b| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "tessSetLayout",
    )
}

/// A per-frame tessellation pool: per instance the raster factor (3) + scan (5) + finalize (3) + emit (9)
/// sets **plus** the RT-coarsening factor (3) + scan (5) + emit (9) sets (Phase 10, Q2 — the coarse
/// secondary-ray chain reuses the same layouts, minus finalize/args), plus one global args (2) set, across
/// [`TESS_MAX_INSTANCES`].
fn create_tess_pool(raw: &ash::Device) -> crate::Result<vk::DescriptorPool> {
    // Raster: factor + scan + finalize + emit (4). RT: factor + scan + emit (3). = 7 sets per instance.
    let per_instance_sets = 7;
    let max_sets = TESS_MAX_INSTANCES * per_instance_sets + 1; // + the global args set
    // Buffers per set summed: raster factor 3 + scan 5 + finalize 3 + emit 9 = 20; RT factor 3 + scan 5 +
    // emit 9 = 17; = 37 per instance, + args 2.
    let buffers = TESS_MAX_INSTANCES * 37 + 2;
    let sizes = [vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::STORAGE_BUFFER)
        .descriptor_count(buffers)];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(max_sets)
        .pool_sizes(&sizes);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_pool(&info, None) },
        "tessPool",
    )
}

/// Allocates one set from `pool` for `layout` and writes `buffers` as consecutive storage-buffer
/// bindings (0..n). `None` on allocation failure (logged).
pub fn wire_storage_set(
    raw: &ash::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    buffers: &[vk::Buffer],
) -> Option<vk::DescriptorSet> {
    let layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    // SAFETY: the ash seam. The layout outlives the call; the set lives until the pool is reset.
    let set = match unsafe { raw.allocate_descriptor_sets(&info) } {
        Ok(sets) => sets[0],
        Err(result) => {
            tracing::error!("tessellation: allocate set failed: {result:?}");
            return None;
        }
    };
    let infos: Vec<vk::DescriptorBufferInfo> = buffers
        .iter()
        .map(|&buffer| vk::DescriptorBufferInfo {
            buffer,
            offset: 0,
            range: vk::WHOLE_SIZE,
        })
        .collect();
    let writes: Vec<vk::WriteDescriptorSet> = (0..buffers.len())
        .map(|b| {
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(b as u32)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(std::slice::from_ref(&infos[b]))
        })
        .collect();
    // SAFETY: the ash seam. The set + buffers outlive the call; each write targets one binding.
    unsafe { raw.update_descriptor_sets(&writes, &[]) };
    Some(set)
}

/// The pool handle for `frame`, for wiring this frame's sets.
impl Tessellation {
    /// This frame's descriptor pool (reset by [`Tessellation::begin_frame`]).
    pub fn pool(&self, frame: usize) -> vk::DescriptorPool {
        self.frames[frame].pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The push blocks must match the byte sizes passed to `build_compute_multi` (and the Slang
    /// `Push` structs) exactly — a mismatch silently truncates the push range.
    #[test]
    fn push_block_sizes_match_the_shader_contract() {
        assert_eq!(size_of::<TessFactorPush>(), 128);
        assert_eq!(size_of::<TessScanPush>(), 32);
        assert_eq!(size_of::<TessFinalizePush>(), 32);
        assert_eq!(size_of::<TessEmitPush>(), 64);
    }

    /// Worst-case reservation is the closed form of the dice contract at `L = factor_cap`.
    #[test]
    fn worst_case_matches_the_dice_closed_form() {
        // One triangle at cap 4: verts = 5*6/2 = 15, tris = 16 → indices = 48.
        assert_eq!(tess_worst_case(1, 4), (15, 48));
        // Scales linearly with the base primitive count.
        assert_eq!(tess_worst_case(10, 4), (150, 480));
        // A zero cap is clamped to 1 (verts = 3, indices = 3) so a buffer is always reservable.
        assert_eq!(tess_worst_case(1, 0), (3, 3));
    }

    /// The dice plan resolves a cap within [`TESS_MAX_DICE_FACTOR`] to a single leaf (no split) and a cap
    /// above it into `4^levels` subpatches whose leaf level stays `≤ TESS_MAX_DICE_FACTOR` (a valid CLAS
    /// cluster). The subpatch count is a power of four and the leaf reconstructs the factor per-edge halving.
    #[test]
    fn dice_plan_splits_only_above_the_dice_cap() {
        // Within the cap: one leaf, level = ceil(factor).
        assert_eq!(dice_plan(1.0), (1, 1));
        assert_eq!(dice_plan(8.0), (1, 8));
        assert_eq!(dice_plan(11.0), (1, 11));
        assert_eq!(dice_plan(10.5), (1, 11));
        // Just over the cap: one split level (4 subpatches), leaf halved and re-ceiled, ≤ cap.
        let (subs, leaf) = dice_plan(20.0);
        assert_eq!(subs, 4);
        assert_eq!(leaf, 10); // 20 / 2 = 10
        // The default factor cap (32): two levels ⇒ 16 subpatches, leaf 8.
        assert_eq!(dice_plan(32.0), (16, 8));
        // Every leaf level stays a valid CLAS-sized cluster across a wide factor sweep.
        for &f in &[12.0f32, 13.0, 22.0, 30.0, 44.0, 64.0, 100.0, 500.0, 1000.0] {
            let (subs, leaf) = dice_plan(f);
            assert!(
                (1..=TESS_MAX_DICE_FACTOR).contains(&leaf),
                "leaf {leaf} within cap at {f}"
            );
            assert!(
                leaf * leaf <= TESS_CLAS_MAX_TRIS,
                "leaf fits CLAS tris at {f}"
            );
            assert!(
                (leaf + 1) * (leaf + 2) / 2 <= TESS_CLAS_MAX_VERTS,
                "leaf fits CLAS verts at {f}"
            );
            assert!(
                subs.is_power_of_two(),
                "subpatch count {subs} is a power of four at {f}"
            );
        }
    }

    /// The worst-case reservation is split-aware above the dice cap (`4^levels` subpatch expansion) and is
    /// monotone non-decreasing in the cap — so reserving at the cap bounds any per-triangle factor ≤ cap.
    #[test]
    fn worst_case_is_split_aware_and_monotone() {
        // Default cap 32 ⇒ 16 subpatches at leaf 8: verts = 16·(9·10/2) = 720, tris = 16·64 = 1024.
        assert_eq!(tess_worst_case(1, 32), (720, 1024 * 3));
        assert_eq!(tess_worst_case(4, 32), (4 * 720, 4 * 1024 * 3));
        // Monotone non-decreasing in the cap (within-band leaf growth + the band jumps up).
        let mut prev = (0u64, 0u64);
        for cap in 1u32..=64 {
            let wc = tess_worst_case(1, cap);
            assert!(
                wc.0 >= prev.0,
                "verts monotone at cap {cap}: {} < {}",
                wc.0,
                prev.0
            );
            assert!(wc.1 >= prev.1, "indices monotone at cap {cap}");
            prev = wc;
        }
    }

    /// The factor push carries the per-instance factor base and a finite camera; `mvp`/`cam_pos_local`
    /// are derived on the CPU so the kernel needs no camera UBO.
    #[test]
    fn factor_push_carries_instance_base_and_derived_camera() {
        let cam = TessCamera {
            view_proj: Mat4::IDENTITY,
            cam_pos: Vec3::new(0.0, 0.0, 5.0),
            viewport: [1920.0, 1080.0],
            tan_half_fov_y: 0.5,
            near: 0.1,
        };
        let model = Mat4::from_translation(Vec3::new(2.0, 0.0, 0.0));
        let push = factor_push(&cam, model, 32.0, 1.0, 8.0, 42, 7, 0.5, 9, [2.0, 3.0]);
        assert_eq!(push.factor_base, 42);
        assert_eq!(push.edge_count, 7);
        assert_eq!(push.factor_cap, 32.0);
        assert_eq!(push.disp_amp_local, 0.5);
        // The uv tiling rides the packed slots: x in cam_pos_local.w, y in its own field.
        assert_eq!(push.cam_pos_local[3], 2.0);
        assert_eq!(push.uv_tiling_y, 3.0);
        assert_eq!(push.height_index, 9);
        // Camera in local space: world (0,0,5) minus the model translation (2,0,0).
        assert!((push.cam_pos_local[0] - (-2.0)).abs() < 1e-5);
        assert!((push.cam_pos_local[2] - 5.0).abs() < 1e-5);
    }

    /// The geomorph weight is a monotone `0 → 1` ramp over the fractional remainder for a just-born
    /// vertex, is identically `1` for any older vertex, and is C0 across an integer factor boundary —
    /// the contract the emit kernel shares so a factor transition is a smooth motion, not a pop.
    #[test]
    fn geomorph_weight_is_a_c0_monotone_birth_to_resolve_ramp() {
        // A newborn (levels_since_birth == 0) collapses onto its coarse parent, then resolves.
        assert_eq!(
            geomorph_weight(0.0, 0),
            0.0,
            "just born ⇒ on the coarse parent"
        );
        assert!(
            (geomorph_weight(1.0, 0) - 1.0).abs() < 1e-6,
            "fully swept ⇒ fine diced position"
        );
        // Monotone non-decreasing over the sweep, always in [0, 1].
        let mut prev = -1.0;
        for i in 0..=32 {
            let r = i as f32 / 32.0;
            let w = geomorph_weight(r, 0);
            assert!(w >= prev - 1e-6, "monotone in remainder");
            assert!((0.0..=1.0).contains(&w), "weight in [0, 1]");
            prev = w;
        }
        // Any vertex present at a lower level is fully resolved regardless of remainder.
        for lvl in 1..=8 {
            assert_eq!(geomorph_weight(0.0, lvl), 1.0);
            assert_eq!(geomorph_weight(0.5, lvl), 1.0);
            assert_eq!(geomorph_weight(0.999, lvl), 1.0);
        }
        // C0 across the integer boundary: newborn at remainder → 1⁻ equals the same sample one level up.
        let below = geomorph_weight(1.0 - 1e-4, 0);
        let above = geomorph_weight(0.0, 1);
        assert!(
            (below - above).abs() < 1e-3,
            "no discontinuity at the boundary"
        );
    }

    /// The interior-geomorph coarse-parent lookup locates the containing `L-1` micro-triangle: its three
    /// corner weights sum to 1, are non-negative, reconstruct the point in coarse-grid coords, and every
    /// corner is a valid grid vertex (`i + j ≤ level`) — the contract the emit kernel's `coarseParentPosition`
    /// must match so the interior morph is continuous without referencing an out-of-grid vertex.
    #[test]
    fn coarse_parent_bary_locates_a_valid_containing_micro_triangle() {
        // A spread of interior barycentric points across several coarse levels.
        let samples = [
            (0.30f32, 0.20f32),
            (0.10, 0.70),
            (0.45, 0.45),
            (0.05, 0.05),
            (0.60, 0.30),
            (0.333, 0.333),
        ];
        for level in [1u32, 2, 3, 5, 8, 11, 31] {
            for &(a, b) in &samples {
                // Only test genuine interior points (a + b < 1) — boundary verts never call this.
                if a + b >= 1.0 {
                    continue;
                }
                let corners = coarse_parent_bary(a, b, level);
                let sum: f32 = corners.iter().map(|&(_, _, w)| w).sum();
                assert!((sum - 1.0).abs() < 1e-5, "weights sum to 1 (level {level})");
                let level_f = level as f32;
                let mut ri = 0.0f32;
                let mut rj = 0.0f32;
                for &(ci, cj, w) in &corners {
                    assert!(w >= -1e-6, "non-negative weight (level {level})");
                    assert!(
                        ci + cj <= level,
                        "corner ({ci},{cj}) is inside the level-{level} grid"
                    );
                    ri += w * ci as f32;
                    rj += w * cj as f32;
                }
                // Reconstruction in coarse-grid coords: Σ wᵢ·(iᵢ, jᵢ) = (a·level, b·level).
                assert!(
                    (ri - a * level_f).abs() < 1e-4,
                    "reconstructs I (level {level})"
                );
                assert!(
                    (rj - b * level_f).abs() < 1e-4,
                    "reconstructs J (level {level})"
                );
            }
        }
    }

    /// `smoothstep01` is the clamped Hermite ramp both the CPU weight and the kernel evaluate.
    #[test]
    fn smoothstep01_is_a_clamped_hermite_ramp() {
        assert_eq!(smoothstep01(-1.0), 0.0);
        assert_eq!(smoothstep01(0.0), 0.0);
        assert!((smoothstep01(0.5) - 0.5).abs() < 1e-6);
        assert_eq!(smoothstep01(1.0), 1.0);
        assert_eq!(smoothstep01(2.0), 1.0);
    }

    /// The world→pixel projection is inverse-linear in distance (halving distance doubles pixels) and
    /// linear in world length, matching the factor kernel's small-angle form.
    #[test]
    fn project_world_to_pixels_scales_with_size_over_distance() {
        // 1 unit at distance 10, tan(½fov)=0.5, 1080 tall: (1/10)/0.5 * 540 = 108 px.
        let px = project_world_to_pixels(1.0, 10.0, 0.5, 1080.0);
        assert!((px - 108.0).abs() < 1e-3);
        // Twice as far → half the pixels.
        let far = project_world_to_pixels(1.0, 20.0, 0.5, 1080.0);
        assert!((far - 54.0).abs() < 1e-3);
        // Twice as long → twice the pixels.
        let big = project_world_to_pixels(2.0, 10.0, 0.5, 1080.0);
        assert!((big - 216.0).abs() < 1e-3);
        // Degenerate distance is guarded (no divide-by-zero / NaN).
        assert!(project_world_to_pixels(1.0, 0.0, 0.5, 1080.0).is_finite());
    }

    /// The displacement-aware factor is driven by max(base edge, displacement range): a flat base edge
    /// with a tall displacement still refines (fixing the spiky plane), and it clamps to [min, cap].
    #[test]
    fn displacement_aware_factor_refines_on_displacement_not_just_edge_length() {
        // Base edge alone at 8px / target 4px = factor 2.
        assert!((displacement_aware_factor(8.0, 0.0, 4.0, 1.0, 64.0) - 2.0).abs() < 1e-4);
        // A tall displacement (40px range) on the SAME short edge drives the factor up to 10 — the
        // flat-plane fix: detail, not just size, refines.
        assert!((displacement_aware_factor(8.0, 40.0, 4.0, 1.0, 64.0) - 10.0).abs() < 1e-4);
        // Clamped to the cap.
        assert_eq!(displacement_aware_factor(8.0, 4000.0, 4.0, 1.0, 64.0), 64.0);
        // Clamped to the floor when both are tiny.
        assert_eq!(displacement_aware_factor(0.1, 0.1, 4.0, 1.0, 64.0), 1.0);
        // The larger term wins regardless of order.
        assert_eq!(
            displacement_aware_factor(40.0, 8.0, 4.0, 1.0, 64.0),
            displacement_aware_factor(8.0, 40.0, 4.0, 1.0, 64.0),
        );
    }

    /// The hard budget shrinks caps to fit the summed worst-case reservation, leaves an in-budget scene
    /// untouched, and never coarsens below an instance's min factor.
    #[test]
    fn budget_scaled_caps_fits_the_worst_case_reservation() {
        let wc = |caps: &[f32], insts: &[(u32, f32, f32)]| -> u64 {
            insts
                .iter()
                .zip(caps)
                .map(|(&(t, _, _), &c)| tess_worst_case(t, c as u32).0)
                .sum()
        };
        // Already in budget → unchanged.
        let small = [(100u32, 16.0f32, 1.0f32), (100, 16.0, 1.0)];
        let budget_big = 10_000_000;
        assert_eq!(budget_scaled_caps(&small, budget_big), vec![16.0, 16.0]);

        // Over budget → shrunk so the summed reservation fits.
        let big = [(5000u32, 64.0f32, 1.0f32), (5000, 64.0, 1.0)];
        let budget = 2_000_000u64;
        let over = wc(&[64.0, 64.0], &big);
        assert!(over > budget, "precondition: worst case exceeds the budget");
        let fitted = budget_scaled_caps(&big, budget);
        assert!(
            wc(&fitted, &big) <= budget,
            "scaled reservation must fit the budget (got {})",
            wc(&fitted, &big)
        );
        assert!(fitted.iter().all(|&c| c < 64.0), "caps were coarsened");

        // The min-factor floor is never breached even under an impossible budget.
        let floored = [(100000u32, 64.0f32, 8.0f32)];
        let caps = budget_scaled_caps(&floored, 1);
        assert!(
            caps[0] >= 8.0,
            "never coarser than the requested min factor"
        );
    }

    /// RT secondary-ray coarsening (Phase 10, Q2): the target scales up by `TESS_RT_COARSEN` (a coarser
    /// dice), the cap scales down (a smaller reservation), the cap never drops below the min factor or 1
    /// (still displaced, never flat), and the worst-case reservation shrinks ~quadratically vs. the raster
    /// arena — the whole point of coarsening the RT BLAS build.
    #[test]
    fn rt_coarsening_scales_target_up_and_cap_and_reservation_down() {
        // Target scales up by the coarsening factor.
        assert!((rt_coarsen_target(12.0) - 12.0 * TESS_RT_COARSEN).abs() < 1e-4);
        // Cap scales down (rounded up), floored at the min factor.
        assert_eq!(rt_coarsen_cap(32.0, 1.0), (32.0 / TESS_RT_COARSEN).ceil());
        // Never coarser than the requested min factor.
        assert_eq!(rt_coarsen_cap(4.0, 8.0), 8.0);
        // Never below the base triangle (displaced, not flat) even at a tiny cap.
        assert!(rt_coarsen_cap(1.0, 1.0) >= 1.0);
        // The coarse reservation is strictly smaller than the raster one for a non-trivial cap.
        let raster = tess_worst_case(100, 32);
        let coarse = tess_worst_case(100, rt_coarsen_cap(32.0, 1.0) as u32);
        assert!(
            coarse.0 < raster.0 && coarse.1 < raster.1,
            "coarse reservation ({coarse:?}) must be smaller than raster ({raster:?})"
        );
    }

    /// Split recursion subdivides only as far as the dice cap requires, and every leaf factor lands
    /// within the cap — so each diced leaf is a valid CLAS-sized cluster.
    #[test]
    fn split_recursion_bounds_the_leaf_to_the_dice_cap() {
        // Within the cap → no split.
        assert_eq!(split_recursion(8.0, TESS_MAX_DICE_FACTOR), (0, 8.0));
        assert_eq!(split_recursion(11.0, 11), (0, 11.0));
        // Just over the cap → one split level, leaf halved and within the cap.
        let (levels, leaf) = split_recursion(20.0, 11);
        assert_eq!(levels, 1);
        assert!(leaf <= 11.0 && (leaf - 10.0).abs() < 1e-4);
        // Far over → enough levels that the leaf fits; leaf never exceeds the cap.
        for &f in &[12.0f32, 45.0, 64.0, 200.0, 1000.0] {
            let (lv, lf) = split_recursion(f, 11);
            assert!(lf <= 11.0 + 1e-4, "leaf {lf} within cap at factor {f}");
            assert!(
                (lf * (1u32 << lv) as f32 - f).abs() < 1e-2,
                "leaf * 2^levels reconstructs the factor (per-edge halving) at {f}"
            );
        }
        // A leaf diced at the cap fits the CLAS caps.
        let l = TESS_MAX_DICE_FACTOR;
        assert!(
            l * l <= TESS_CLAS_MAX_TRIS,
            "leaf triangles fit the CLAS cap"
        );
        assert!(
            (l + 1) * (l + 2) / 2 <= TESS_CLAS_MAX_VERTS,
            "leaf verts fit the CLAS cap"
        );
    }
}
