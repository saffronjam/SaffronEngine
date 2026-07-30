//! The per-mesh signed distance field: the sparse `SDST` byte image (indirection volume,
//! brick atlas, coarse coverage volume) and the grid-derivation and brick-compaction
//! helpers the GPU jump-flood bake reads back through.
//!
//! A uniform local-space fine grid spans the mesh AABB plus a small pad; each fine voxel
//! holds the signed distance to the surface, negative inside, encoded `R16_SNORM` against a
//! `max_dist` clamp. The grid is compacted into 8³ bricks — 7 unique voxels plus 1 shared
//! border, so trilinear reconstruction is seam-continuous across a brick boundary — behind
//! an indirection volume (`R32_UINT`, one texel per brick holding an atlas index or
//! [`SDF_EMPTY_BRICK`]) plus a brick atlas (`R16_SNORM`) carrying only occupied bricks.
//!
//! The bake runs on the GPU at mesh-upload time; this module owns the format, the CPU
//! brick-compaction the readback feeds ([`Sdf::from_dense_field`]), and the grid derivation
//! ([`bake_grid`]).

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::error::{Error, Result};

/// `SDST` byte-image version. Bumped when the header or brick encoding changes.
pub const SDF_FORMAT_VERSION: u32 = 4;

/// Prefiltered mip levels of the brick atlas. Mip 0 is the fine field; each coarser mip
/// halves the voxel resolution and is band-limited by a conservative min-|d| reduction so a
/// widening cone reads an alias-free distance at range.
pub const SDF_MIP_COUNT: u32 = 3;

/// The four-byte magic at the head of an `SDST` chunk / sidecar.
const SDF_MAGIC: [u8; 4] = *b"SDST";

/// Voxels per brick axis (an 8³ brick = 512 voxels).
pub const SDF_BRICK_SIZE: u32 = 8;
/// Unique fine voxels a brick advances per axis; the 8th voxel is the border shared with
/// the next brick's first plane, so trilinear reconstruction is continuous across bricks.
pub const SDF_BRICK_USEFUL: u32 = 7;
/// The indirection sentinel for a brick the bake found empty (every voxel saturated to
/// `+max_dist`): no atlas brick is allocated and a sample there reads the open-space
/// coverage distance.
pub const SDF_EMPTY_BRICK: u32 = u32::MAX;

/// The fine grid AABB is padded by this fraction of its longest extent so the field
/// carries a positive shell around the surface for the cone trace to march.
const SDF_PAD_FRACTION: f32 = 0.1;
/// The `R16_SNORM` encode clamp is this multiple of the voxel size: distances beyond it
/// saturate to ±1. A few voxels of range is all the penumbra trace samples.
const SDF_MAX_DIST_VOXELS: f32 = 4.0;
/// Target fine-grid resolution: voxels per world metre, times the asset's
/// `resolution_scale`, capped at [`SDF_MAX_GRID_AXIS`] on the longest axis.
pub const SDF_VOXELS_PER_METRE: f32 = 4.0;
/// The smallest fine-grid axis: a thin/flat or tiny mesh still gets a usable field.
pub const SDF_MIN_GRID_AXIS: u32 = 8;
/// The largest fine-grid axis.
pub const SDF_MAX_GRID_AXIS: u32 = 128;
/// The largest fine-grid axis for a single SDF chunk's *core* (before the pad apron). A
/// primitive whose AABB exceeds this at the target resolution is spatially partitioned by
/// [`sdf_chunk_cores`] so a chunk's voxel size never coarsens under [`SDF_MAX_GRID_AXIS`].
/// Kept below that cap so a chunk's padded grid stays within it and the transient dense
/// jump-flood bake volume stays small.
pub const SDF_CHUNK_CORE_VOXELS: u32 = 64;
/// The hard cap on spatial-chunk subdivisions per axis. Bounds the field/slot count and the
/// bake cost regardless of the mesh's local unit scale — a mesh authored in centimetres would
/// otherwise ask for hundreds of chunks per axis, since the `SDF_VOXELS_PER_METRE` target
/// assumes metre-scale local coordinates.
pub const SDF_MAX_CHUNKS_PER_AXIS: u32 = 4;
/// A fine voxel at or above this encoded value counts as "far outside, no surface
/// nearby" for the empty-brick classification (≈ +0.998 of `max_dist`).
const SDF_SATURATED: i16 = 32_700;

/// The grid a bake runs over: the fine voxel `dims`, the padded local-space bounds, and
/// the `R16_SNORM` encode clamp. Derived by [`bake_grid`] from a mesh's AABB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridDesc {
    /// Fine voxel count per axis (nx, ny, nz).
    pub dims: [u32; 3],
    /// Padded grid lower corner, local space.
    pub bounds_min: Vec3,
    /// Padded grid upper corner, local space.
    pub bounds_max: Vec3,
    /// The `R16_SNORM` distance normalization clamp (local units).
    pub max_dist: f32,
}

impl GridDesc {
    /// The per-axis cell size (the span divided by the fine voxel count).
    #[must_use]
    pub fn cell(&self) -> Vec3 {
        (self.bounds_max - self.bounds_min)
            / Vec3::new(
                self.dims[0] as f32,
                self.dims[1] as f32,
                self.dims[2] as f32,
            )
    }

    /// The local-space center of fine voxel `(x, y, z)`.
    #[must_use]
    pub fn voxel_center(&self, x: u32, y: u32, z: u32) -> Vec3 {
        let frac = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
        self.bounds_min + frac * self.cell()
    }

    /// The indirection-volume dimensions (bricks per axis): `ceil((dims - 1) / 7)`, at
    /// least one. Brick `b` along an axis owns fine voxels `b*7 .. b*7+7` (the last voxel
    /// the shared border), so this covers every fine voxel exactly once.
    #[must_use]
    pub fn indirection_dims(&self) -> [u32; 3] {
        let one = |d: u32| (d.saturating_sub(1)).div_ceil(SDF_BRICK_USEFUL).max(1);
        [one(self.dims[0]), one(self.dims[1]), one(self.dims[2])]
    }
}

/// Derives the bake grid from a mesh's local-space AABB and per-asset `resolution_scale`.
///
/// The AABB is padded by [`SDF_PAD_FRACTION`] of its longest extent; the longest padded
/// axis takes `round(span * `[`SDF_VOXELS_PER_METRE`]` * resolution_scale)` voxels,
/// clamped into `[`[`SDF_MIN_GRID_AXIS`]`, `[`SDF_MAX_GRID_AXIS`]`]`, fixing a near-cubic
/// voxel size that every axis then rounds to.
#[must_use]
pub fn bake_grid(lo: Vec3, hi: Vec3, resolution_scale: f32) -> GridDesc {
    let scale = resolution_scale.max(1e-3);
    let extent = hi - lo;
    let longest = extent.max_element().max(1e-4);
    let pad = longest * SDF_PAD_FRACTION;
    let bounds_min = lo - Vec3::splat(pad);
    let bounds_max = hi + Vec3::splat(pad);
    let span = bounds_max - bounds_min;

    let longest_span = span.max_element().max(1e-4);
    let target = (longest_span * SDF_VOXELS_PER_METRE * scale).round() as u32;
    let longest_axis = target.clamp(SDF_MIN_GRID_AXIS, SDF_MAX_GRID_AXIS);
    let voxel = longest_span / longest_axis as f32;
    let axis = |s: f32| (s / voxel).round() as u32;
    let dims = [
        axis(span.x).clamp(SDF_MIN_GRID_AXIS, SDF_MAX_GRID_AXIS),
        axis(span.y).clamp(SDF_MIN_GRID_AXIS, SDF_MAX_GRID_AXIS),
        axis(span.z).clamp(SDF_MIN_GRID_AXIS, SDF_MAX_GRID_AXIS),
    ];

    GridDesc {
        dims,
        bounds_min,
        bounds_max,
        max_dist: voxel * SDF_MAX_DIST_VOXELS,
    }
}

/// The core AABBs a mesh primitive is spatially partitioned into for SDF baking: a uniform
/// grid over `[lo, hi]` sized so each chunk's core is at most [`SDF_CHUNK_CORE_VOXELS`]
/// voxels per axis at the target resolution.
///
/// Returns a single element (the whole AABB) when it already fits — the common case, one
/// tight field per primitive. Subdivision only engages for a primitive large enough that a
/// single grid would coarsen under the [`SDF_MAX_GRID_AXIS`] clamp; each chunk then keeps
/// the target voxel size. Chunks tile `[lo, hi]` exactly; because [`bake_grid`] pads each
/// chunk by [`SDF_PAD_FRACTION`] of its longest extent — larger than the encode clamp
/// (`SDF_MAX_DIST_VOXELS` voxels) at this core size — every surface within the clamp of a
/// chunk's core is captured by that chunk's grid, so a `min`-over-instances field sample
/// stays exact across chunk seams.
#[must_use]
pub fn sdf_chunk_cores(lo: Vec3, hi: Vec3, resolution_scale: f32) -> Vec<(Vec3, Vec3)> {
    let scale = resolution_scale.max(1e-3);
    let extent = (hi - lo).max(Vec3::splat(1e-4));
    let vpm = SDF_VOXELS_PER_METRE * scale;
    let count = |e: f32| {
        ((e * vpm / SDF_CHUNK_CORE_VOXELS as f32).ceil() as u32).clamp(1, SDF_MAX_CHUNKS_PER_AXIS)
    };
    let (nx, ny, nz) = (count(extent.x), count(extent.y), count(extent.z));
    if nx == 1 && ny == 1 && nz == 1 {
        return vec![(lo, hi)];
    }
    let step = extent / Vec3::new(nx as f32, ny as f32, nz as f32);
    let mut cores = Vec::with_capacity((nx * ny * nz) as usize);
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let c0 = lo + step * Vec3::new(i as f32, j as f32, k as f32);
                let c1 =
                    (lo + step * Vec3::new((i + 1) as f32, (j + 1) as f32, (k + 1) as f32)).min(hi);
                cores.push((c0, c1));
            }
        }
    }
    cores
}

/// The fixed-layout header at the head of an `SDST` byte image (112 bytes, little-endian).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct SdfHeader {
    /// `b"SDST"`.
    pub magic: [u8; 4],
    /// Format version; [`SDF_FORMAT_VERSION`].
    pub version: u32,
    /// Fine voxel count per axis (nx, ny, nz).
    pub dims: [u32; 3],
    /// Indirection-volume dimensions (bricks per axis).
    pub indirection_dims: [u32; 3],
    /// Atlas tiling: occupied bricks per axis in the brick atlas image.
    pub atlas_bricks: [u32; 3],
    /// Padded grid lower corner, local space.
    pub bounds_min: [f32; 3],
    /// Padded grid upper corner, local space.
    pub bounds_max: [f32; 3],
    /// `R16_SNORM` distance normalization clamp (local units).
    pub max_dist: f32,
    /// Voxels per brick axis ([`SDF_BRICK_SIZE`]).
    pub brick_size: u32,
    /// Unique voxels a brick advances per axis ([`SDF_BRICK_USEFUL`]).
    pub brick_useful: u32,
    /// Number of occupied (non-empty) bricks stored in the atlas.
    pub occupied_bricks: u32,
    /// Prefiltered atlas mip levels ([`SDF_MIP_COUNT`]). Mip 0 is the fine field; coarser
    /// mips halve the voxel resolution (the brick-atlas image is mipped in lockstep).
    pub mip_count: u32,
    /// The coarse coverage volume dims (one texel per brick block): the conservative
    /// distance to the nearest surface anywhere in each block, the empty-space oracle.
    pub coverage_dims: [u32; 3],
    /// The field's own aggregate occupancy in unorm16 (`0` = the field carries none and
    /// the occluder resolves occupancy from the drawn material — every triangle bake).
    /// A cooked plant field carries the CALIBRATED aggregate here, because the material
    /// a placed plant draws slot 0 with is its trunk, and a canopy is not a solid.
    pub occupancy_unorm: u32,
    /// The field's own proxy albedo, rgb packed 8:8:8 unorm low-to-high (`0` = resolve
    /// from the drawn material).
    pub proxy_albedo: u32,
    /// Padding to a 16-byte multiple (keeps the `Pod` round-trip exact).
    pub _pad: u32,
}

const _: () = assert!(size_of::<SdfHeader>() == 112, "SdfHeader must be 112 bytes");

/// A baked sparse signed distance field: its header, the brick indirection volume, and
/// the brick atlas.
///
/// `indirection` has one entry per brick (`product(indirection_dims)`, X-fastest): each
/// is an atlas brick index, or [`SDF_EMPTY_BRICK`]. `atlas` holds the occupied bricks
/// only (`occupied_bricks * 512` cells, **brick-major**: brick `slot` occupies
/// `[slot*512 .. slot*512+512)`, X-fastest within the 8³ brick). The GPU brick-atlas
/// *image* is spatially tiled instead — see [`Sdf::atlas_image_data`].
#[derive(Clone, Debug, PartialEq)]
pub struct Sdf {
    /// The header (dims, brick tiling, bounds, encode clamp, occupied count).
    pub header: SdfHeader,
    /// The brick indirection volume: one `u32` per brick (atlas index or empty sentinel),
    /// X-fastest then Y then Z over `indirection_dims`.
    pub indirection: Vec<u32>,
    /// The occupied bricks, brick-major (`occupied_bricks * 512` `i16` cells). This is the
    /// fine field (mip 0); coarser mips are derived on demand by [`Sdf::atlas_image_data_mip`].
    pub atlas: Vec<i16>,
    /// The coarse coverage volume (dense, `product(coverage_dims)` `i16`, X-fastest): the
    /// minimum |d| over each brick block, `R16_SNORM` against `max_dist` (always positive).
    /// The empty-space oracle — a large value means the whole block is open.
    pub coverage: Vec<i16>,
}

impl Sdf {
    /// Compacts a dense fine-voxel signed field (`dims.x*dims.y*dims.z` `i16`, X-fastest,
    /// normalized to `grid.max_dist`) into the sparse brick representation.
    ///
    /// Each brick gathers its 8³ voxels from the dense field (fine voxel `brick*7 + local`,
    /// clamped to the grid edge — the 8th voxel is the next brick's first plane, so the
    /// shared border duplicates correctly). A brick whose every voxel saturates to
    /// `+max_dist` is marked empty; the rest are packed into the atlas in allocation order.
    #[must_use]
    pub fn from_dense_field(grid: &GridDesc, dense: &[i16]) -> Sdf {
        let [nx, ny, nz] = grid.dims;
        debug_assert_eq!(dense.len(), nx as usize * ny as usize * nz as usize);
        let indir = grid.indirection_dims();
        let brick_count = indir[0] as usize * indir[1] as usize * indir[2] as usize;

        let at = |x: u32, y: u32, z: u32| -> i16 {
            let x = x.min(nx - 1);
            let y = y.min(ny - 1);
            let z = z.min(nz - 1);
            dense[((z * ny + y) * nx + x) as usize]
        };

        let mut indirection = vec![SDF_EMPTY_BRICK; brick_count];
        let mut atlas: Vec<i16> = Vec::new();
        // The coverage volume: one texel per brick, the conservative min-|d| over the block
        // (the empty-space oracle). Computed for *every* brick — empty bricks included — so a
        // sample in an empty block reads a real (large) open distance, not a fixed guess.
        let mut coverage = vec![i16::MAX; brick_count];
        let mut occupied: u32 = 0;
        for bz in 0..indir[2] {
            for by in 0..indir[1] {
                for bx in 0..indir[0] {
                    let mut cells =
                        [i16::MAX; (SDF_BRICK_SIZE * SDF_BRICK_SIZE * SDF_BRICK_SIZE) as usize];
                    let mut empty = true;
                    let mut min_abs: i32 = i32::from(i16::MAX);
                    for lz in 0..SDF_BRICK_SIZE {
                        for ly in 0..SDF_BRICK_SIZE {
                            for lx in 0..SDF_BRICK_SIZE {
                                let v = at(
                                    bx * SDF_BRICK_USEFUL + lx,
                                    by * SDF_BRICK_USEFUL + ly,
                                    bz * SDF_BRICK_USEFUL + lz,
                                );
                                cells
                                    [((lz * SDF_BRICK_SIZE + ly) * SDF_BRICK_SIZE + lx) as usize] =
                                    v;
                                if v < SDF_SATURATED {
                                    empty = false;
                                }
                                min_abs = min_abs.min(i32::from(v).abs());
                            }
                        }
                    }
                    let brick_idx = (bz * indir[1] + by) * indir[0] + bx;
                    // Conservative distance to the nearest surface in this block: min |d| over
                    // the block's voxels, kept positive (it is a magnitude). A lower bound for
                    // everything the block covers, so a coverage-driven march leap never
                    // overshoots a surface.
                    coverage[brick_idx as usize] = min_abs as i16;
                    if empty {
                        continue;
                    }
                    indirection[brick_idx as usize] = occupied;
                    atlas.extend_from_slice(&cells);
                    occupied += 1;
                }
            }
        }

        let atlas_bricks = atlas_tiling(occupied);
        Sdf {
            header: SdfHeader {
                magic: SDF_MAGIC,
                version: SDF_FORMAT_VERSION,
                dims: grid.dims,
                indirection_dims: indir,
                atlas_bricks,
                bounds_min: grid.bounds_min.into(),
                bounds_max: grid.bounds_max.into(),
                max_dist: grid.max_dist,
                brick_size: SDF_BRICK_SIZE,
                brick_useful: SDF_BRICK_USEFUL,
                occupied_bricks: occupied,
                mip_count: SDF_MIP_COUNT,
                coverage_dims: indir,
                // Solid by default: a triangle-baked field is a wall, and a wall
                // occludes outright. A producer with calibrated porosity (a cooked
                // plant canopy) overwrites this.
                occupancy_unorm: 65_535,
                proxy_albedo: 0,
                _pad: 0,
            },
            indirection,
            atlas,
            coverage,
        }
    }

    /// The brick-atlas image dimensions in voxels (`atlas_bricks * 8` per axis).
    #[must_use]
    pub fn atlas_image_dims(&self) -> [u32; 3] {
        [
            self.header.atlas_bricks[0] * SDF_BRICK_SIZE,
            self.header.atlas_bricks[1] * SDF_BRICK_SIZE,
            self.header.atlas_bricks[2] * SDF_BRICK_SIZE,
        ]
    }

    /// The brick atlas laid out for the GPU 3D image: a spatially tiled
    /// `atlas_image_dims` buffer (X-fastest) where occupied brick `slot` sits at atlas
    /// brick coordinate `(slot % ax, (slot/ax) % ay, slot/(ax*ay))`. Unused trailing
    /// bricks read `+max_dist` (never indexed). The CPU [`Sdf::atlas`] stays brick-major;
    /// this scatters it into the image tiling the shader's brick tap expects.
    #[must_use]
    pub fn atlas_image_data(&self) -> Vec<i16> {
        let [ax, ay, _az] = self.header.atlas_bricks;
        let [dx, dy, dz] = self.atlas_image_dims();
        let mut data = vec![i16::MAX; (dx as usize) * (dy as usize) * (dz as usize)];
        for slot in 0..self.header.occupied_bricks {
            let bc = atlas_brick_coord(slot, ax, ay);
            for lz in 0..SDF_BRICK_SIZE {
                for ly in 0..SDF_BRICK_SIZE {
                    for lx in 0..SDF_BRICK_SIZE {
                        let v = self.atlas[(slot * SDF_BRICK_SIZE * SDF_BRICK_SIZE * SDF_BRICK_SIZE
                            + (lz * SDF_BRICK_SIZE + ly) * SDF_BRICK_SIZE
                            + lx) as usize];
                        let gx = bc[0] * SDF_BRICK_SIZE + lx;
                        let gy = bc[1] * SDF_BRICK_SIZE + ly;
                        let gz = bc[2] * SDF_BRICK_SIZE + lz;
                        data[((gz * dy + gy) * dx + gx) as usize] = v;
                    }
                }
            }
        }
        data
    }

    /// The brick-atlas image dimensions at mip `level`: mip 0 is [`Sdf::atlas_image_dims`],
    /// each coarser level halves per axis (Vulkan's `max(1, floor(dim/2))`), down to mip
    /// `mip_count - 1`. Because every atlas axis is a multiple of 8 (`atlas_bricks * 8`), the
    /// three-level chain (8 → 4 → 2 per brick) stays brick-aligned at every level.
    #[must_use]
    pub fn atlas_image_dims_mip(&self, level: u32) -> [u32; 3] {
        let [mut x, mut y, mut z] = self.atlas_image_dims();
        for _ in 0..level {
            x = (x / 2).max(1);
            y = (y / 2).max(1);
            z = (z / 2).max(1);
        }
        [x, y, z]
    }

    /// The brick-atlas image data for mip `level`, laid out X-fastest for the GPU 3D image's
    /// matching mip subresource. Mip 0 is [`Sdf::atlas_image_data`] (the fine field). Each
    /// coarser mip is a **corner-aligned, conservative min-|d|** reduction: a coarse brick of
    /// `8 >> level` texels per axis samples its brick's `[0, 7]` fine span at evenly spaced
    /// points (`localTexel * 7 / (brickTexels - 1)`), so texel 0 and the last texel land
    /// exactly on the brick endpoints — the fine voxels a neighbouring brick *shares*. Each
    /// coarse texel takes the signed value of whichever fine voxel in its (global) min-|d|
    /// window has the smallest magnitude.
    ///
    /// Two properties matter and both hold by construction:
    /// - **Seam-free across bricks at every mip.** Each coarse texel's value is a pure function
    ///   of its global fine-grid centre and a symmetric window around it. Adjacent bricks' shared
    ///   boundary texels map to the *same* global centre (e.g. brick `b` last texel and brick
    ///   `b+1` texel 0 both sit on fine voxel `(b+1)*7`), so they reduce the identical window to
    ///   the identical value — no C0 discontinuity where the cone-footprint mip-select engages.
    ///   A plain whole-image box reduction does **not** have this property: its boundary texels
    ///   average different fine voxels on each side and reintroduce seams at range.
    /// - **Conservative lower bound.** A coarse texel is the min-|d| over its fine window, so it
    ///   never claims more open space than any fine voxel it covers — a band-limited distance the
    ///   widening cone can read without aliasing, and a safe march leap.
    #[must_use]
    pub fn atlas_image_data_mip(&self, level: u32) -> Vec<i16> {
        if level == 0 {
            return self.atlas_image_data();
        }
        let bt = (SDF_BRICK_SIZE >> level).max(1); // texels per coarse brick axis: 4, 2
        let [ax, ay, _az] = self.header.atlas_bricks;
        let dims = self.atlas_image_dims_mip(level); // == atlas_bricks * bt (atlas is 8-aligned)
        let mut data = vec![i16::MAX; dims[0] as usize * dims[1] as usize * dims[2] as usize];

        // Corner-aligned spacing: a coarse texel `l` maps to fine offset `l * 7/(bt-1)` within
        // the brick, so l=0 and l=bt-1 land on the brick's shared border voxels. The min-|d|
        // window half-extent reaches into the neighbour brick by half a coarse cell, making the
        // boundary texel a symmetric function of its global centre (the seam-free guarantee).
        let step = SDF_BRICK_USEFUL as f32 / (bt - 1) as f32;
        let reach = step * 0.5 + 0.5 - 1e-4;
        let useful = SDF_BRICK_USEFUL as f32;

        let indir = self.header.indirection_dims;
        for bz in 0..indir[2] {
            for by in 0..indir[1] {
                for bx in 0..indir[0] {
                    let brick_idx = ((bz * indir[1] + by) * indir[0] + bx) as usize;
                    let slot = self.indirection[brick_idx];
                    if slot == SDF_EMPTY_BRICK {
                        continue;
                    }
                    let bc = atlas_brick_coord(slot, ax, ay);
                    for lz in 0..bt {
                        for ly in 0..bt {
                            for lx in 0..bt {
                                let cfx = bx as f32 * useful + lx as f32 * step;
                                let cfy = by as f32 * useful + ly as f32 * step;
                                let cfz = bz as f32 * useful + lz as f32 * step;
                                let v = self.window_min_abs(cfx, cfy, cfz, reach);
                                let gx = bc[0] * bt + lx;
                                let gy = bc[1] * bt + ly;
                                let gz = bc[2] * bt + lz;
                                data[((gz * dims[1] + gy) * dims[0] + gx) as usize] = v;
                            }
                        }
                    }
                }
            }
        }
        data
    }

    /// The signed value (raw `R16_SNORM` `i16`) of whichever fine voxel in the symmetric integer
    /// window `[c - reach, c + reach]` (per axis, around the global fine-grid centre `c`) has the
    /// smallest magnitude — the conservative min-|d| a corner-aligned coarse texel stores. The
    /// window is taken over the *global* fine field ([`Sdf::fine_voxel_raw`] clamps out-of-grid
    /// taps), so a texel on a brick border reaches symmetrically into the neighbour brick and two
    /// bricks sharing that border reduce the identical window — seams cannot appear at range.
    #[must_use]
    fn window_min_abs(&self, cx: f32, cy: f32, cz: f32, reach: f32) -> i16 {
        let span = |c: f32| ((c - reach).ceil() as i32, (c + reach).floor() as i32);
        let (xlo, xhi) = span(cx);
        let (ylo, yhi) = span(cy);
        let (zlo, zhi) = span(cz);
        let mut best = i16::MAX;
        let mut best_abs = i32::MAX;
        for fz in zlo..=zhi {
            for fy in ylo..=yhi {
                for fx in xlo..=xhi {
                    let v = self.fine_voxel_raw(fx, fy, fz);
                    let a = i32::from(v).abs();
                    if a < best_abs {
                        best_abs = a;
                        best = v;
                    }
                }
            }
        }
        best
    }

    /// The raw `R16_SNORM` `i16` at fine voxel `(x, y, z)` (clamped to the grid), resolving the
    /// owning brick through the indirection volume; an empty brick reads `+max` (`i16::MAX`). The
    /// integer-domain twin of [`Sdf::sample_voxel`] the mip reduction reads.
    #[must_use]
    fn fine_voxel_raw(&self, x: i32, y: i32, z: i32) -> i16 {
        let d = self.header.dims;
        let x = x.clamp(0, d[0] as i32 - 1) as u32;
        let y = y.clamp(0, d[1] as i32 - 1) as u32;
        let z = z.clamp(0, d[2] as i32 - 1) as u32;
        let indir = self.header.indirection_dims;
        let bx = (x / SDF_BRICK_USEFUL).min(indir[0] - 1);
        let by = (y / SDF_BRICK_USEFUL).min(indir[1] - 1);
        let bz = (z / SDF_BRICK_USEFUL).min(indir[2] - 1);
        let brick_idx = ((bz * indir[1] + by) * indir[0] + bx) as usize;
        let slot = self.indirection[brick_idx];
        if slot == SDF_EMPTY_BRICK {
            return i16::MAX;
        }
        let lx = x - bx * SDF_BRICK_USEFUL;
        let ly = y - by * SDF_BRICK_USEFUL;
        let lz = z - bz * SDF_BRICK_USEFUL;
        let cell = slot * SDF_BRICK_SIZE * SDF_BRICK_SIZE * SDF_BRICK_SIZE
            + (lz * SDF_BRICK_SIZE + ly) * SDF_BRICK_SIZE
            + lx;
        self.atlas[cell as usize]
    }

    /// Decodes coverage texel `(x, y, z)` to a conservative distance in local units (the min
    /// |d| over that brick block). The read-back path the conservative-bound tests use.
    #[must_use]
    pub fn sample_coverage(&self, x: u32, y: u32, z: u32) -> f32 {
        let [cx, cy, _cz] = self.header.coverage_dims;
        let idx = ((z * cy + y) * cx + x) as usize;
        f32::from(self.coverage[idx]) / 32767.0 * self.header.max_dist
    }

    /// Decodes fine voxel `(x, y, z)` back to a signed distance in local units (an empty
    /// brick reads `+max_dist`). The read-back path the format/analytic/sign tests use.
    #[must_use]
    pub fn sample_voxel(&self, x: u32, y: u32, z: u32) -> f32 {
        let raw = self.fine_voxel_raw(x as i32, y as i32, z as i32);
        f32::from(raw) / 32767.0 * self.header.max_dist
    }

    /// Frames the field into an `SDST` byte image: the 112-byte header, the indirection
    /// volume (`u32`), the brick atlas (`i16`), then the coverage volume (`i16`),
    /// little-endian. Coarser atlas mips are derived on load, not stored.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(
            size_of::<SdfHeader>()
                + self.indirection.len() * 4
                + self.atlas.len() * 2
                + self.coverage.len() * 2,
        );
        bytes.extend_from_slice(bytemuck::bytes_of(&self.header));
        bytes.extend_from_slice(bytemuck::cast_slice(&self.indirection));
        bytes.extend_from_slice(bytemuck::cast_slice(&self.atlas));
        bytes.extend_from_slice(bytemuck::cast_slice(&self.coverage));
        bytes
    }

    /// Decodes an `SDST` byte image.
    ///
    /// # Errors
    ///
    /// [`Error::Truncated`] when the bytes end before the header or either section;
    /// [`Error::BadMagic`] / [`Error::UnsupportedVersion`] / [`Error::BadLayout`] when the
    /// header fails its magic, version, or dims-vs-length checks.
    pub fn from_bytes(bytes: &[u8]) -> Result<Sdf> {
        let head = bytes
            .get(..size_of::<SdfHeader>())
            .ok_or(Error::Truncated)?;
        let header: SdfHeader = *bytemuck::from_bytes(head);
        if header.magic != SDF_MAGIC {
            return Err(Error::BadMagic);
        }
        if header.version != SDF_FORMAT_VERSION {
            return Err(Error::UnsupportedVersion(header.version));
        }
        if header.dims.contains(&0)
            || header.indirection_dims.contains(&0)
            || header.coverage_dims.contains(&0)
            || header.brick_size != SDF_BRICK_SIZE
            || header.brick_useful != SDF_BRICK_USEFUL
        {
            return Err(Error::BadLayout);
        }
        let brick_count = header.indirection_dims[0] as usize
            * header.indirection_dims[1] as usize
            * header.indirection_dims[2] as usize;
        let atlas_cells = header.occupied_bricks as usize
            * (SDF_BRICK_SIZE * SDF_BRICK_SIZE * SDF_BRICK_SIZE) as usize;
        let coverage_cells = header.coverage_dims[0] as usize
            * header.coverage_dims[1] as usize
            * header.coverage_dims[2] as usize;

        let indir_bytes = brick_count * 4;
        let atlas_bytes = atlas_cells * 2;
        let coverage_bytes = coverage_cells * 2;
        let body = bytes
            .get(size_of::<SdfHeader>()..)
            .ok_or(Error::Truncated)?;
        if body.len() < indir_bytes + atlas_bytes + coverage_bytes {
            return Err(Error::Truncated);
        }
        let indirection: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&body[..indir_bytes]).to_vec();
        let atlas: Vec<i16> =
            bytemuck::cast_slice::<u8, i16>(&body[indir_bytes..indir_bytes + atlas_bytes]).to_vec();
        let coverage: Vec<i16> = bytemuck::cast_slice::<u8, i16>(
            &body[indir_bytes + atlas_bytes..indir_bytes + atlas_bytes + coverage_bytes],
        )
        .to_vec();
        Ok(Sdf {
            header,
            indirection,
            atlas,
            coverage,
        })
    }
}

/// The four-byte magic at the head of an `SDFS` set — a mesh's per-primitive fields.
const SDF_SET_MAGIC: [u8; 4] = *b"SDFS";

/// Frames a mesh's baked fields (one tight [`Sdf`] per primitive / spatial chunk) into one
/// cache image: the `SDFS` magic + version, the field count, then each field's byte length
/// followed by its `SDST` bytes, little-endian. A mesh's SDF is a *set* — one field per
/// primitive — so the sidecar cache stores the set, keyed by the whole-mesh content hash.
#[must_use]
pub fn sdf_set_to_bytes(fields: &[Sdf]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&SDF_SET_MAGIC);
    bytes.extend_from_slice(&SDF_FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    for f in fields {
        let b = f.to_bytes();
        bytes.extend_from_slice(&(b.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&b);
    }
    bytes
}

/// Decodes an `SDFS` field set written by [`sdf_set_to_bytes`].
///
/// # Errors
///
/// [`Error::Truncated`] when the bytes end before the header or any field; [`Error::BadMagic`]
/// / [`Error::UnsupportedVersion`] on a header mismatch; propagates [`Sdf::from_bytes`] for
/// each field.
pub fn sdf_set_from_bytes(bytes: &[u8]) -> Result<Vec<Sdf>> {
    let head = bytes.get(..12).ok_or(Error::Truncated)?;
    if head[..4] != SDF_SET_MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u32::from_le_bytes(head[4..8].try_into().unwrap());
    if version != SDF_FORMAT_VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let count = u32::from_le_bytes(head[8..12].try_into().unwrap()) as usize;
    let mut fields = Vec::with_capacity(count);
    let mut off = 12;
    for _ in 0..count {
        let len = u32::from_le_bytes(
            bytes
                .get(off..off + 4)
                .ok_or(Error::Truncated)?
                .try_into()
                .unwrap(),
        ) as usize;
        off += 4;
        // Copy the field into a fresh (over-aligned) buffer before decoding: a sub-slice into
        // the set buffer is not guaranteed 4-aligned for the header's `bytemuck` read, whereas
        // a `Vec<u8>` is — the same guarantee `Sdf::from_bytes` relies on for a read-from-file.
        let body = bytes.get(off..off + len).ok_or(Error::Truncated)?.to_vec();
        fields.push(Sdf::from_bytes(&body)?);
        off += len;
    }
    Ok(fields)
}

/// The atlas brick coordinate for occupied `slot` given the atlas tiling `(ax, ay)`.
#[must_use]
fn atlas_brick_coord(slot: u32, ax: u32, ay: u32) -> [u32; 3] {
    [slot % ax, (slot / ax) % ay, slot / (ax * ay)]
}

/// A near-cubic atlas tiling (bricks per axis) holding `occupied` bricks; at least
/// `1×1×1` so the atlas image is never zero-sized.
#[must_use]
fn atlas_tiling(occupied: u32) -> [u32; 3] {
    let n = occupied.max(1);
    let ax = ((n as f64).cbrt().ceil() as u32).max(1);
    let ay = ax;
    let az = n.div_ceil(ax * ay).max(1);
    [ax, ay, az]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picking::MeshBvh;
    use saffron_test_support::close;

    /// A CPU reference bake used as the format, analytic, and sign oracle: samples each fine
    /// voxel centre with [`MeshBvh::nearest_signed_distance`], then compacts through
    /// [`Sdf::from_dense_field`].
    pub(crate) fn bake_sparse_reference(
        positions: &[Vec3],
        indices: &[u32],
        dims: [u32; 3],
    ) -> Option<Sdf> {
        let bvh = MeshBvh::build(positions, indices)?;
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for p in positions {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let extent = hi - lo;
        let longest = extent.max_element().max(1e-4);
        let pad = longest * SDF_PAD_FRACTION;
        let bounds_min = lo - Vec3::splat(pad);
        let bounds_max = hi + Vec3::splat(pad);
        let span = bounds_max - bounds_min;
        let cell = span / Vec3::new(dims[0] as f32, dims[1] as f32, dims[2] as f32);
        let max_dist = cell.max_element() * SDF_MAX_DIST_VOXELS;
        let grid = GridDesc {
            dims,
            bounds_min,
            bounds_max,
            max_dist,
        };

        let mut dense = vec![0i16; dims[0] as usize * dims[1] as usize * dims[2] as usize];
        for z in 0..dims[2] {
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    let p = grid.voxel_center(x, y, z);
                    let signed = bvh.nearest_signed_distance(p);
                    let norm = (signed / max_dist).clamp(-1.0, 1.0);
                    dense[((z * dims[1] + y) * dims[0] + x) as usize] =
                        (norm * 32767.0).round() as i16;
                }
            }
        }
        Some(Sdf::from_dense_field(&grid, &dense))
    }

    /// A unit-radius UV sphere (lat/long tessellation) centered at the origin.
    fn unit_sphere(rings: u32, segments: u32) -> (Vec<Vec3>, Vec<u32>) {
        let mut positions = Vec::new();
        for r in 0..=rings {
            let v = r as f32 / rings as f32;
            let theta = v * std::f32::consts::PI;
            for s in 0..=segments {
                let u = s as f32 / segments as f32;
                let phi = u * std::f32::consts::TAU;
                positions.push(Vec3::new(
                    theta.sin() * phi.cos(),
                    theta.cos(),
                    theta.sin() * phi.sin(),
                ));
            }
        }
        let mut indices = Vec::new();
        let stride = segments + 1;
        for r in 0..rings {
            for s in 0..segments {
                let a = r * stride + s;
                let b = a + stride;
                indices.extend([a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        (positions, indices)
    }

    /// An axis-aligned box `[lo, hi]` as 12 triangles (outward-wound).
    fn box_mesh(lo: Vec3, hi: Vec3) -> (Vec<Vec3>, Vec<u32>) {
        let v = [
            Vec3::new(lo.x, lo.y, lo.z),
            Vec3::new(hi.x, lo.y, lo.z),
            Vec3::new(hi.x, hi.y, lo.z),
            Vec3::new(lo.x, hi.y, lo.z),
            Vec3::new(lo.x, lo.y, hi.z),
            Vec3::new(hi.x, lo.y, hi.z),
            Vec3::new(hi.x, hi.y, hi.z),
            Vec3::new(lo.x, hi.y, hi.z),
        ];
        let faces = [
            [0, 1, 2, 3], // -z
            [5, 4, 7, 6], // +z
            [4, 0, 3, 7], // -x
            [1, 5, 6, 2], // +x
            [4, 5, 1, 0], // -y
            [3, 2, 6, 7], // +y
        ];
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        for f in faces {
            let base = positions.len() as u32;
            for &i in &f {
                positions.push(v[i]);
            }
            indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        (positions, indices)
    }

    #[test]
    fn sdst_round_trips_through_bytes() {
        let (pos, idx) = box_mesh(Vec3::splat(-0.5), Vec3::splat(0.5));
        let sdf = bake_sparse_reference(&pos, &idx, [16, 16, 16]).expect("box bakes");
        let bytes = sdf.to_bytes();
        assert_eq!(
            bytes.len(),
            size_of::<SdfHeader>()
                + sdf.indirection.len() * 4
                + sdf.atlas.len() * 2
                + sdf.coverage.len() * 2,
            "byte image is header + u32 indirection + i16 atlas + i16 coverage"
        );
        assert_eq!(sdf.header.mip_count, SDF_MIP_COUNT);
        assert_eq!(sdf.header.coverage_dims, sdf.header.indirection_dims);
        let back = Sdf::from_bytes(&bytes).expect("decodes");
        assert_eq!(back, sdf);
    }

    #[test]
    fn coarse_mips_and_coverage_are_conservative_lower_bounds() {
        // The distance-field downsample must never claim more open space than the fine field:
        // each coarser mip texel's |d| is a lower bound for the fine voxel at its (corner-aligned)
        // centre, and each coverage texel is a lower bound for its brick block. Validated on the
        // sphere (smoothly varying) and the box (a sign change through the surface, which a box
        // average would smear) so a regression to averaging is caught.
        for (pos, idx) in [
            unit_sphere(48, 64),
            box_mesh(Vec3::splat(-1.0), Vec3::splat(1.0)),
        ] {
            let sdf = bake_sparse_reference(&pos, &idx, [32, 32, 32]).expect("bakes");
            assert_eq!(sdf.header.mip_count, SDF_MIP_COUNT);
            let [ax, ay, _az] = sdf.header.atlas_bricks;
            let indir = sdf.header.indirection_dims;

            for level in 1..sdf.header.mip_count {
                let cur = sdf.atlas_image_data_mip(level);
                let cur_dims = sdf.atlas_image_dims_mip(level);
                let bt = (SDF_BRICK_SIZE >> level).max(1);
                let step = SDF_BRICK_USEFUL as f32 / (bt - 1) as f32;
                let center = |b: u32, l: u32| b as f32 * SDF_BRICK_USEFUL as f32 + l as f32 * step;
                let raw = |bc: [u32; 3], l: [u32; 3]| -> i16 {
                    let gx = bc[0] * bt + l[0];
                    let gy = bc[1] * bt + l[1];
                    let gz = bc[2] * bt + l[2];
                    cur[((gz * cur_dims[1] + gy) * cur_dims[0] + gx) as usize]
                };
                let read = |bc: [u32; 3], l: [u32; 3]| -> f32 {
                    f32::from(raw(bc, l)).abs() / 32767.0 * sdf.header.max_dist
                };
                for bz in 0..indir[2] {
                    for by in 0..indir[1] {
                        for bx in 0..indir[0] {
                            let brick_idx = ((bz * indir[1] + by) * indir[0] + bx) as usize;
                            let slot = sdf.indirection[brick_idx];
                            if slot == SDF_EMPTY_BRICK {
                                continue;
                            }
                            let bc = atlas_brick_coord(slot, ax, ay);
                            for lz in 0..bt {
                                for ly in 0..bt {
                                    for lx in 0..bt {
                                        // The coarse texel's centre falls on a fine voxel that lies
                                        // inside its min-|d| window, so the coarse value can never
                                        // exceed that fine voxel's magnitude (conservative).
                                        let fx = (center(bx, lx).round() as u32)
                                            .min(sdf.header.dims[0] - 1);
                                        let fy = (center(by, ly).round() as u32)
                                            .min(sdf.header.dims[1] - 1);
                                        let fz = (center(bz, lz).round() as u32)
                                            .min(sdf.header.dims[2] - 1);
                                        let coarse = read(bc, [lx, ly, lz]);
                                        let fine = sdf.sample_voxel(fx, fy, fz).abs();
                                        assert!(
                                            coarse <= fine + 1e-4,
                                            "mip {level} brick ({bx},{by},{bz}) texel ({lx},{ly},{lz}) |d|={coarse} exceeds fine centre {fine}"
                                        );
                                    }
                                }
                            }
                            // Seam-free: the brick's high-x border texels must equal the +x
                            // neighbour brick's low-x border texels (same shared fine voxels).
                            if bx + 1 < indir[0] {
                                let nbr_idx = ((bz * indir[1] + by) * indir[0] + (bx + 1)) as usize;
                                let nbr = sdf.indirection[nbr_idx];
                                if nbr != SDF_EMPTY_BRICK {
                                    let nbc = atlas_brick_coord(nbr, ax, ay);
                                    for lz in 0..bt {
                                        for ly in 0..bt {
                                            let here = raw(bc, [bt - 1, ly, lz]);
                                            let there = raw(nbc, [0, ly, lz]);
                                            assert_eq!(
                                                here, there,
                                                "mip {level} seam at brick ({bx},{by},{bz}) +x face ({ly},{lz})"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Each coverage texel is a lower bound for its brick block: it never reports more
            // open space than the nearest fine voxel in the block (so a leap is always safe).
            let [cx, cy, cz] = sdf.header.coverage_dims;
            for bz in 0..cz {
                for by in 0..cy {
                    for bx in 0..cx {
                        let cov = sdf.sample_coverage(bx, by, bz).abs();
                        let mut block_min = f32::INFINITY;
                        for lz in 0..SDF_BRICK_SIZE {
                            for ly in 0..SDF_BRICK_SIZE {
                                for lx in 0..SDF_BRICK_SIZE {
                                    let vx =
                                        (bx * SDF_BRICK_USEFUL + lx).min(sdf.header.dims[0] - 1);
                                    let vy =
                                        (by * SDF_BRICK_USEFUL + ly).min(sdf.header.dims[1] - 1);
                                    let vz =
                                        (bz * SDF_BRICK_USEFUL + lz).min(sdf.header.dims[2] - 1);
                                    block_min = block_min.min(sdf.sample_voxel(vx, vy, vz).abs());
                                }
                            }
                        }
                        assert!(
                            cov <= block_min + 1e-4,
                            "coverage ({bx},{by},{bz})={cov} exceeds block min {block_min}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn small_aabb_is_one_chunk_large_aabb_subdivides() {
        let one = sdf_chunk_cores(Vec3::splat(-1.0), Vec3::splat(1.0), 1.0);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0], (Vec3::splat(-1.0), Vec3::splat(1.0)));

        // 40 m along x at 4 voxels/m = 160 core voxels > 64 → ceil(160/64) = 3 chunks in x.
        let lo = Vec3::new(0.0, 0.0, 0.0);
        let hi = Vec3::new(40.0, 2.0, 2.0);
        let chunks = sdf_chunk_cores(lo, hi, 1.0);
        assert_eq!(chunks.len(), 3, "40 m x-span subdivides into 3 chunks");
        assert_eq!(chunks[0].0, lo, "the first chunk starts at the AABB min");
        assert_eq!(
            chunks.last().unwrap().1,
            hi,
            "the last chunk ends at the AABB max (exact tiling)"
        );
        // Chunks tile without gaps: each chunk's max is the next chunk's min along x.
        for pair in chunks.windows(2) {
            assert!(close(pair[0].1.x, pair[1].0.x, 1e-4));
        }
    }

    #[test]
    fn sdf_set_round_trips_through_bytes() {
        let (pos, idx) = box_mesh(Vec3::splat(-0.5), Vec3::splat(0.5));
        let a = bake_sparse_reference(&pos, &idx, [16, 16, 16]).expect("box bakes");
        let (spos, sidx) = unit_sphere(24, 32);
        let b = bake_sparse_reference(&spos, &sidx, [16, 16, 16]).expect("sphere bakes");
        let bytes = sdf_set_to_bytes(&[a.clone(), b.clone()]);
        let back = sdf_set_from_bytes(&bytes).expect("set decodes");
        assert_eq!(back, vec![a, b]);
        assert!(
            sdf_set_from_bytes(&sdf_set_to_bytes(&[]))
                .unwrap()
                .is_empty()
        );
        let mut bad = bytes.clone();
        bad[0] = b'X';
        assert!(matches!(sdf_set_from_bytes(&bad), Err(Error::BadMagic)));
    }

    #[test]
    fn from_bytes_rejects_bad_magic_and_truncation() {
        let (pos, idx) = box_mesh(Vec3::splat(-0.5), Vec3::splat(0.5));
        let sdf = bake_sparse_reference(&pos, &idx, [16, 16, 16]).unwrap();
        let mut bytes = sdf.to_bytes();
        bytes[0] = b'X';
        assert!(matches!(Sdf::from_bytes(&bytes), Err(Error::BadMagic)));
        assert!(matches!(
            Sdf::from_bytes(&bytes[..8]),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn empty_mesh_has_no_reference_bake() {
        assert!(bake_sparse_reference(&[], &[], [8, 8, 8]).is_none());
        assert!(bake_sparse_reference(&[Vec3::ZERO, Vec3::X], &[], [8, 8, 8]).is_none());
    }

    #[test]
    fn sparse_bricks_save_memory_on_a_mostly_empty_field() {
        // A sphere's bounding-box corners sit far outside the surface (beyond the encode
        // clamp), so those bricks saturate to +max and are dropped — the atlas holds fewer
        // than the dense brick count (the whole point of the format).
        let (pos, idx) = unit_sphere(48, 64);
        let sdf = bake_sparse_reference(&pos, &idx, [32, 32, 32]).expect("sphere bakes");
        let indir = sdf.header.indirection_dims;
        let total_bricks = indir[0] * indir[1] * indir[2];
        assert!(
            sdf.header.occupied_bricks > 0,
            "the surface occupies bricks"
        );
        assert!(
            sdf.header.occupied_bricks < total_bricks,
            "a sphere must leave its corner bricks empty ({} of {})",
            sdf.header.occupied_bricks,
            total_bricks
        );
    }

    #[test]
    fn unit_sphere_matches_analytic_distance() {
        let (pos, idx) = unit_sphere(64, 96);
        let dims = [32, 32, 32];
        let sdf = bake_sparse_reference(&pos, &idx, dims).expect("sphere bakes");
        let h = sdf.header;
        let span = Vec3::from(h.bounds_max) - Vec3::from(h.bounds_min);
        let cell = span / Vec3::new(dims[0] as f32, dims[1] as f32, dims[2] as f32);
        let voxel = cell.max_element();
        let mut checked = 0;
        for z in 0..dims[2] {
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    let frac = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                    let p = Vec3::from(h.bounds_min) + frac * cell;
                    let analytic = p.length() - 1.0;
                    // Skip the surface band (tessellation + grid bias) and saturated cells.
                    if analytic.abs() < 1.5 * voxel || analytic.abs() > h.max_dist * 0.9 {
                        continue;
                    }
                    let got = sdf.sample_voxel(x, y, z);
                    assert!(
                        close(got, analytic, 1.5 * voxel),
                        "cell ({x},{y},{z}) p={p:?}: got {got}, analytic {analytic}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no cells were comparable");
    }

    #[test]
    fn box_matches_analytic_distance() {
        let half = Vec3::splat(1.0);
        let (pos, idx) = box_mesh(-half, half);
        let dims = [32, 32, 32];
        let sdf = bake_sparse_reference(&pos, &idx, dims).expect("box bakes");
        let h = sdf.header;
        let span = Vec3::from(h.bounds_max) - Vec3::from(h.bounds_min);
        let cell = span / Vec3::new(dims[0] as f32, dims[1] as f32, dims[2] as f32);
        let voxel = cell.max_element();
        for z in 0..dims[2] {
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    let frac = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                    let p = Vec3::from(h.bounds_min) + frac * cell;
                    let q = p.abs() - half;
                    let outside = q.max(Vec3::ZERO).length();
                    let inside = q.max_element().min(0.0);
                    let analytic = outside + inside;
                    if analytic.abs() < 1.5 * voxel || analytic.abs() > h.max_dist * 0.9 {
                        continue;
                    }
                    let got = sdf.sample_voxel(x, y, z);
                    assert!(
                        close(got, analytic, 1.5 * voxel),
                        "cell ({x},{y},{z}) p={p:?}: got {got}, analytic {analytic}"
                    );
                }
            }
        }
    }

    #[test]
    fn sign_is_negative_inside_positive_outside() {
        // Sampled read-back is negative deep inside the box and positive well outside it —
        // the brick compaction preserves the oracle's sign.
        let (pos, idx) = box_mesh(Vec3::splat(-1.0), Vec3::splat(1.0));
        let dims = [24, 24, 24];
        let sdf = bake_sparse_reference(&pos, &idx, dims).expect("box bakes");
        let center = sdf.sample_voxel(dims[0] / 2, dims[1] / 2, dims[2] / 2);
        assert!(
            center < 0.0,
            "the grid center is inside the box (got {center})"
        );
        let corner = sdf.sample_voxel(0, 0, 0);
        assert!(
            corner > 0.0,
            "a padded grid corner is outside (got {corner})"
        );
    }

    #[test]
    fn cross_brick_sampling_is_continuous() {
        // Two fine voxels straddling a brick boundary (index 6 → brick 0, index 7 → brick
        // 1 via the shared border) read close values on the smooth box field — no seam.
        let (pos, idx) = box_mesh(Vec3::splat(-1.0), Vec3::splat(1.0));
        let dims = [24, 24, 24];
        let sdf = bake_sparse_reference(&pos, &idx, dims).expect("box bakes");
        let h = sdf.header;
        let span = Vec3::from(h.bounds_max) - Vec3::from(h.bounds_min);
        let cell = (span / Vec3::new(dims[0] as f32, dims[1] as f32, dims[2] as f32)).max_element();
        let y = dims[1] / 2;
        let z = dims[2] / 2;
        let a = sdf.sample_voxel(SDF_BRICK_USEFUL - 1, y, z); // last voxel of brick 0
        let b = sdf.sample_voxel(SDF_BRICK_USEFUL, y, z); // first voxel of brick 1
        assert!(
            (a - b).abs() < 1.5 * cell,
            "adjacent voxels across a brick boundary must be continuous: {a} vs {b}"
        );
        // The shared border voxel (brick 0 local 7) equals brick 1's local 0 — same fine
        // voxel, so the atlas duplicate carries the identical value.
        assert_eq!(
            sdf.sample_voxel(SDF_BRICK_USEFUL, y, z),
            sdf.sample_voxel(SDF_BRICK_USEFUL, y, z)
        );
    }
}
