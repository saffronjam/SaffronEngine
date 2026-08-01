//! Materializing the reconstructed micro-field blades for acceleration-structure builds.
//!
//! A grass blade has no CPU instance: the raster executor derives its vertices from a
//! frame-transient candidate the micro reconstruction scatters. Ray traversal has no vertex stage,
//! so the only way a blade reaches a bottom-level structure is as real vertices and indices. This
//! plans one dispatch per materialized tile, writing world-space blade geometry — wind bend
//! included, from the same analytic field the raster reconstruction bakes into its candidates —
//! into a reserved slice of a transient arena, and hands each tile back as generated geometry the
//! per-frame `MODE_BUILD` path rebuilds like any other minted topology.
//!
//! The reservation is per tile and fixed, because the blade count is decided on device: the tile's
//! whole texel grid at full density is the bound, and the dispatch degenerate-pads whatever it does
//! not fill, exactly as the amplification arena does.

use ash::vk;

use crate::draw_list::TessRtSlice;

/// Resident field tiles one frame materializes ray geometry for. The near-field reach gate keeps
/// distant tiles empty, so this bounds the per-frame build cost rather than the world's tile count.
pub const MICRO_RT_MAX_TILES: u32 = 4;

/// Blades one materialized tile reserves. A tile's texel grid at full density reconstructs
/// `texels * SCENE_MICRO_MAX_PER_TEXEL` blades; past this the tile's tail is dropped rather than
/// spilling into the next tile's slice.
pub const MICRO_RT_BLADES_PER_TILE: u32 = 4_096;

/// The camera distance beyond which a tile contributes no ray geometry. Held below the raster
/// reconstruction's own reach so the materialized set is a near-field subset of what draws.
pub const MICRO_RT_REACH_METRES: f32 = 48.0;

/// Distinguishes a materialized tile's structure key from every entity-keyed one, and from the
/// wind, deforming, and unmirrored key bases the placement keys use (bits 63, 62, and 61).
const MICRO_BLAS_KEY_BASE: u64 = 1 << 60;

/// One materialized tile: the structure key its per-frame build is cached under, and the arena
/// slice its geometry occupies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MicroRtTile {
    /// Keys the grow-only per-frame rebuild structure.
    pub key: u64,
    /// The reserved geometry slice, degenerate-padded past the blades the dispatch wrote.
    pub slice: TessRtSlice,
}

/// The structure key of directory entry `tile`.
#[must_use]
pub fn micro_blas_key(tile: u32) -> u64 {
    MICRO_BLAS_KEY_BASE | u64::from(tile)
}

/// One materialization dispatch, pushed whole (112 bytes, inside the 128-byte guaranteed range).
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct MicroRtDeformPush {
    /// World-space camera position, the reach gate's origin.
    pub eye: [f32; 3],
    /// Tiles beyond this camera distance reconstruct nothing.
    pub max_distance: f32,
    /// The resident-tile directory's byte offset within the fields arena.
    pub directory_offset: u32,
    /// Directory entries this dispatch covers (the materialized prefix).
    pub directory_count: u32,
    /// Blades each tile's slice reserves.
    pub blades_per_tile: u32,
    /// Reserved ABI word.
    pub reserved: u32,
    /// Device address of the arena's vertex slice base.
    pub vertices: vk::DeviceAddress,
    /// Device address of the arena's index slice base.
    pub indices: vk::DeviceAddress,
    /// Wind direction (x, z), speed, and gust fraction.
    pub wind_dir_speed_gust: [f32; 4],
    /// Wind roughness, gust frequency, reference height, height exponent.
    pub wind_params: [f32; 4],
    /// Turbulence octave count.
    pub wind_octaves: u32,
    /// Phase seed.
    pub wind_seed: u32,
    /// Simulation seconds this frame.
    pub wind_time_current: f32,
    /// Simulation seconds the previous frame.
    pub wind_time_previous: f32,
    /// Device address of this frame's local wind-source list.
    pub wind_sources: u64,
    /// Sources in the list.
    pub wind_source_count: u32,
    /// Reserved ABI word.
    pub wind_reserved: u32,
}

const _: () = assert!(size_of::<MicroRtDeformPush>() == 112);

/// The micro-blade materialization push byte size for pipeline creation.
pub const MICRO_RT_DEFORM_PUSH_SIZE: u32 = 112;

/// The arena the frame's materialized tiles share: element counts and, once acquired, handles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MicroRtArena {
    /// Tiles the frame materializes (the directory prefix).
    pub tiles: u32,
    /// Vertices the arena reserves in total.
    pub vertices: u32,
    /// Indices the arena reserves in total.
    pub indices: u32,
}

/// Sizes the arena for `directory_count` resident tiles: the materialized prefix under
/// [`MICRO_RT_MAX_TILES`], and the vertex and index elements its fixed per-tile reservations span.
///
/// Returns `None` when no tile is resident, so a frame with no field pays no arena.
#[must_use]
pub fn plan_micro_rt_arena(directory_count: u32) -> Option<MicroRtArena> {
    let tiles = directory_count.min(MICRO_RT_MAX_TILES);
    if tiles == 0 {
        return None;
    }
    Some(MicroRtArena {
        tiles,
        vertices: tiles * MICRO_RT_BLADES_PER_TILE * crate::MICRO_BLADE_VERTEX_COUNT,
        indices: tiles * MICRO_RT_BLADES_PER_TILE * crate::MICRO_BLADE_INDEX_COUNT,
    })
}

/// Lays the arena out into one generated-geometry slice per materialized tile.
///
/// Slices are disjoint and equal: the dispatch writes a blade's ten vertices and twenty-four
/// indices at its own slot within the tile's reservation, so a tile's index values are relative to
/// its own `vertex_base` — which is what the build's `max_vertex` bound is stated against.
#[must_use]
pub fn plan_micro_rt_tiles(
    arena: MicroRtArena,
    vertex_buffer: vk::Buffer,
    index_buffer: vk::Buffer,
) -> Vec<MicroRtTile> {
    (0..arena.tiles)
        .map(|tile| MicroRtTile {
            key: micro_blas_key(tile),
            slice: TessRtSlice {
                vertex_buffer,
                index_buffer,
                vertex_base: tile * MICRO_RT_BLADES_PER_TILE * crate::MICRO_BLADE_VERTEX_COUNT,
                index_base: tile * MICRO_RT_BLADES_PER_TILE * crate::MICRO_BLADE_INDEX_COUNT,
                worst_case_verts: MICRO_RT_BLADES_PER_TILE * crate::MICRO_BLADE_VERTEX_COUNT,
                worst_case_prims: MICRO_RT_BLADES_PER_TILE * crate::MICRO_BLADE_INDEX_COUNT / 3,
            },
        })
        .collect()
}

/// Records the frame's materialization: one workgroup per materialized tile, its lanes striding
/// the tile's texels.
pub fn record_micro_rt_deform(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: &crate::Pipeline,
    set: vk::DescriptorSet,
    push: &MicroRtDeformPush,
) {
    // SAFETY: the ash seam. The PSO/set are valid this frame; the push spans the declared range
    // and both addresses reference the frame's live arena.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline.handle());
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::COMPUTE,
            pipeline.layout(),
            0,
            &[set],
            &[],
        );
        raw.cmd_push_constants(
            cmd,
            pipeline.layout(),
            vk::ShaderStageFlags::COMPUTE,
            0,
            bytemuck::bytes_of(push),
        );
        raw.cmd_dispatch(cmd, 1, push.directory_count.max(1), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_with_no_resident_tile_reserves_no_arena() {
        assert_eq!(plan_micro_rt_arena(0), None);
    }

    /// The blade shape the materialization dispatch writes, read off the shared template rather
    /// than off the constants the arena sizes itself with: `(vertices, indices)` per blade.
    fn template_blade_shape() -> (u32, u32) {
        let indices = crate::micro_blade_template_indices();
        assert!(
            !indices.is_empty() && indices.len().is_multiple_of(3),
            "the template is whole triangles"
        );
        let vertices = indices.iter().max().copied().expect("template corners") + 1;
        (vertices, indices.len() as u32)
    }

    #[test]
    fn the_arena_covers_every_materialized_tile_and_no_more() {
        // Sized against the template the dispatch actually writes: a corner the template addresses
        // past what the arena reserved is a write into the next tile's blades.
        let (blade_vertices, blade_indices) = template_blade_shape();
        let one = plan_micro_rt_arena(1).expect("one resident tile materializes");
        assert_eq!(one.vertices, MICRO_RT_BLADES_PER_TILE * blade_vertices);
        assert_eq!(one.indices, MICRO_RT_BLADES_PER_TILE * blade_indices);
        // Tiles never share a reservation, so the arena is linear in the materialized prefix.
        let two = plan_micro_rt_arena(2).expect("two resident tiles materialize");
        assert_eq!(two.tiles, 2);
        assert_eq!(two.vertices, 2 * one.vertices);
        assert_eq!(two.indices, 2 * one.indices);
    }

    #[test]
    fn the_materialized_prefix_is_capped() {
        let arena = plan_micro_rt_arena(MICRO_RT_MAX_TILES + 7).expect("tiles materialize");
        assert_eq!(arena.tiles, MICRO_RT_MAX_TILES);
    }

    #[test]
    fn tile_slices_are_disjoint_and_fit_the_arena() {
        let arena = plan_micro_rt_arena(MICRO_RT_MAX_TILES).expect("tiles materialize");
        let tiles = plan_micro_rt_tiles(arena, vk::Buffer::null(), vk::Buffer::null());
        assert_eq!(tiles.len() as u32, arena.tiles);
        let mut vertex_cursor = 0;
        let mut index_cursor = 0;
        for tile in &tiles {
            // A slice starting before the previous one ended would let two tiles write the same
            // vertices, and each tile's structure would trace the other's blades.
            assert_eq!(tile.slice.vertex_base, vertex_cursor);
            assert_eq!(tile.slice.index_base, index_cursor);
            vertex_cursor += tile.slice.worst_case_verts;
            index_cursor += tile.slice.worst_case_prims * 3;
        }
        assert_eq!(vertex_cursor, arena.vertices);
        assert_eq!(index_cursor, arena.indices);
    }

    #[test]
    fn a_slice_bounds_exactly_the_blades_its_reservation_holds() {
        let (blade_vertices, blade_indices) = template_blade_shape();
        let arena = plan_micro_rt_arena(1).expect("one tile materializes");
        let tile = plan_micro_rt_tiles(arena, vk::Buffer::null(), vk::Buffer::null())[0];
        // The build states `max_vertex` from `worst_case_verts` and reads the whole index range,
        // so an index the dispatch writes must land inside the slice it is stated against.
        assert_eq!(
            tile.slice.worst_case_verts,
            MICRO_RT_BLADES_PER_TILE * blade_vertices
        );
        assert_eq!(
            tile.slice.worst_case_prims * 3,
            MICRO_RT_BLADES_PER_TILE * blade_indices
        );
        // The last blade of the tile is the one that reaches furthest: its highest template corner
        // still has to land below the bound `max_vertex` is stated from.
        let highest_corner = (MICRO_RT_BLADES_PER_TILE - 1) * blade_vertices + blade_vertices - 1;
        assert!(highest_corner < tile.slice.worst_case_verts);
    }

    #[test]
    fn materialized_tile_keys_never_collide_with_an_entity() {
        // Structure keys share one map with the entity-keyed refits; a tile whose key could be an
        // entity id would trace that entity's last build instead of its own blades.
        let keys: Vec<u64> = (0..MICRO_RT_MAX_TILES).map(micro_blas_key).collect();
        assert!(keys.iter().all(|key| *key & MICRO_BLAS_KEY_BASE != 0));
        assert!(keys.iter().all(|key| *key > u64::from(u32::MAX)));
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), keys.len());
    }
}
