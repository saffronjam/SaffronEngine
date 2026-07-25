//! Virtual shadow maps: one physical depth atlas of fixed-size pages behind
//! per-light virtual address spaces, with a CPU residency authority (allocation,
//! LRU eviction, fence-safe tile reuse) and per-frame page tables the samplers
//! resolve logical pages through. No sparse binding — the atlas is one ordinary
//! depth image and every mapping is explicit.

use std::collections::HashMap;

use ash::vk;
use saffron_geometry::glam::{Mat4, Vec3};

use crate::resources::{Image, ImageDesc};

/// Texels per page side.
pub const VSM_PAGE_SIZE: u32 = 128;
/// Physical atlas side in texels.
pub const VSM_ATLAS_SIZE: u32 = 4096;
/// Physical tiles per atlas side.
pub const VSM_ATLAS_TILES: u32 = VSM_ATLAS_SIZE / VSM_PAGE_SIZE;
/// Directional clip levels (level `k` spans [`VSM_LEVEL0_EXTENT_M`] · 2^k).
pub const VSM_DIRECTIONAL_LEVELS: u32 = 8;
/// Logical pages per directional-level side.
pub const VSM_LEVEL_PAGES: u32 = 32;
/// World extent of directional level 0 in metres.
pub const VSM_LEVEL0_EXTENT_M: f32 = 32.0;
/// Light-space depth half-span of every directional level in metres.
pub const VSM_DIRECTIONAL_HALF_DEPTH_M: f32 = 512.0;
/// Frames an evicted tile cools down before reuse (fence safety across the ring).
pub const VSM_TILE_COOLDOWN_FRAMES: u64 = 2;
/// Finest directional level whose texels CANNOT resolve continuous motion: levels
/// up to this one re-dirty each frame while wind sways the scene (level 5 texels
/// are 0.25 m; coarser levels blur sway below one texel).
pub const VSM_DYNAMIC_MAX_LEVEL: u32 = 5;

/// Logical pages per spot-space side (one 2048² virtual plane).
pub const VSM_SPOT_PAGES: u32 = 16;
/// The spot space's first entry in the frame page table (after the directional
/// levels).
pub const VSM_SPOT_TABLE_BASE: u32 = VSM_DIRECTIONAL_LEVELS * VSM_LEVEL_PAGES * VSM_LEVEL_PAGES;

/// Logical pages per point cube-face side (one 1024² virtual plane per face).
pub const VSM_POINT_FACE_PAGES: u32 = 8;
/// Cube faces of the shadowed point light.
pub const VSM_POINT_FACES: u32 = 6;
/// The point faces' first entry in the frame page table (after the spot space);
/// face `f` occupies the 64 entries at `base + f * 64`.
pub const VSM_POINT_TABLE_BASE: u32 = VSM_SPOT_TABLE_BASE + VSM_SPOT_PAGES * VSM_SPOT_PAGES;

/// One logical shadow page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VsmPageKey {
    /// A directional clip-level page.
    Directional {
        /// Clip level (0 = finest).
        level: u32,
        /// Page coordinates within the level's snapped window.
        x: u32,
        /// Page coordinates within the level's snapped window.
        y: u32,
    },
    /// A page of the shadowed spot light's projective space.
    Spot {
        /// Page coordinates within the spot's clip plane.
        x: u32,
        /// Page coordinates within the spot's clip plane.
        y: u32,
    },
    /// A page of one cube face of the shadowed point light.
    PointFace {
        /// Cube face in the fixed sampling order (+X −X +Y −Y +Z −Z).
        face: u32,
        /// Page coordinates within the face's clip plane.
        x: u32,
        /// Page coordinates within the face's clip plane.
        y: u32,
    },
}

/// One resident page's state.
#[derive(Clone, Copy, Debug)]
struct VsmPageState {
    /// Physical tile index (`y · tiles + x`).
    tile: u32,
    /// Frame serial of the latest demand.
    last_demand_frame: u64,
    /// Whether the tile's content must re-render this frame.
    dirty: bool,
    /// Whether the tile has ever rasterized: a never-rendered page (a hole) beats
    /// any refresh in the per-frame render budget.
    rendered: bool,
}

/// A tile waiting out its in-flight cooldown after eviction.
#[derive(Clone, Copy, Debug)]
struct CoolingTile {
    tile: u32,
    evicted_frame: u64,
}

/// One page the frame must rasterize: its physical tile and logical identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VsmRenderPage {
    /// The page.
    pub key: VsmPageKey,
    /// Physical tile index.
    pub tile: u32,
}

/// The packed page-table entry: tile index (10 bits) | resident flag (bit 31).
pub const VSM_TABLE_RESIDENT: u32 = 1 << 31;

/// Last completed frame's residency activity, published into `render-stats`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VsmCounters {
    /// Pages demanded (receiver requests + bootstrap).
    pub requested: u32,
    /// Demands answered by an already-resident page.
    pub hits: u32,
    /// Fresh page-to-tile allocations.
    pub allocated: u32,
    /// Pages the frame rasterized (drained dirty pages).
    pub rendered: u32,
    /// Resident pages re-marked dirty (caster or window changes).
    pub dirtied: u32,
    /// LRU evictions.
    pub evicted: u32,
    /// Demands the full atlas could not satisfy this frame.
    pub overflow: u32,
}

/// The CPU residency authority over the physical atlas.
pub struct VsmResidency {
    pages: HashMap<VsmPageKey, VsmPageState>,
    free_tiles: Vec<u32>,
    cooling: Vec<CoolingTile>,
    frame_counters: VsmCounters,
    counters: VsmCounters,
}

impl Default for VsmResidency {
    fn default() -> Self {
        Self {
            pages: HashMap::new(),
            free_tiles: (0..VSM_ATLAS_TILES * VSM_ATLAS_TILES).rev().collect(),
            cooling: Vec::new(),
            frame_counters: VsmCounters::default(),
            counters: VsmCounters::default(),
        }
    }
}

impl VsmResidency {
    /// Returns tiles whose cooldown elapsed to the free list, and publishes the
    /// finished frame's activity counters.
    pub fn begin_frame(&mut self, frame: u64) {
        self.counters = self.frame_counters;
        self.frame_counters = VsmCounters::default();
        let mut index = 0;
        while index < self.cooling.len() {
            if frame.saturating_sub(self.cooling[index].evicted_frame) > VSM_TILE_COOLDOWN_FRAMES {
                let tile = self.cooling.swap_remove(index).tile;
                self.free_tiles.push(tile);
            } else {
                index += 1;
            }
        }
    }

    /// Demands `key` for `frame`: allocates a tile when absent (evicting the
    /// least-recently-demanded page if the free list is dry), marks fresh pages
    /// dirty, and returns the page's tile — or `None` when the atlas is fully
    /// hot (every tile demanded this frame or cooling).
    pub fn demand(&mut self, key: VsmPageKey, frame: u64) -> Option<u32> {
        self.frame_counters.requested += 1;
        if let Some(state) = self.pages.get_mut(&key) {
            state.last_demand_frame = frame;
            self.frame_counters.hits += 1;
            return Some(state.tile);
        }
        let tile = match self.free_tiles.pop() {
            Some(tile) => tile,
            None => {
                let victim = self
                    .pages
                    .iter()
                    .filter(|(_, state)| state.last_demand_frame < frame)
                    .min_by_key(|(_, state)| state.last_demand_frame)
                    .map(|(key, _)| *key);
                let Some(victim) = victim else {
                    self.frame_counters.overflow += 1;
                    return None;
                };
                let evicted = self.pages.remove(&victim).expect("victim exists");
                self.frame_counters.evicted += 1;
                self.cooling.push(CoolingTile {
                    tile: evicted.tile,
                    evicted_frame: frame,
                });
                // The evicted tile itself is still cooling; nothing is safe yet.
                let Some(tile) = self.free_tiles.pop() else {
                    self.frame_counters.overflow += 1;
                    return None;
                };
                tile
            }
        };
        self.frame_counters.allocated += 1;
        self.pages.insert(
            key,
            VsmPageState {
                tile,
                last_demand_frame: frame,
                dirty: true,
                rendered: false,
            },
        );
        Some(tile)
    }

    /// Marks a resident page dirty (its casters moved or its window shifted).
    pub fn mark_dirty(&mut self, key: VsmPageKey) {
        if let Some(state) = self.pages.get_mut(&key) {
            state.dirty = true;
            self.frame_counters.dirtied += 1;
        }
    }

    /// Drops every resident directional page of `level` (the snapped window moved).
    pub fn invalidate_directional_level(&mut self, level: u32, frame: u64) {
        self.invalidate_matching(
            frame,
            |key| matches!(key, VsmPageKey::Directional { level: l, .. } if *l == level),
        );
    }

    /// Drops every resident spot page (the spot's transform changed).
    pub fn invalidate_spot(&mut self, frame: u64) {
        self.invalidate_matching(frame, |key| matches!(key, VsmPageKey::Spot { .. }));
    }

    /// Invalidates every point-face page (the light moved or its range changed:
    /// all six projective spaces are stale).
    pub fn invalidate_point(&mut self, frame: u64) {
        self.invalidate_matching(frame, |key| matches!(key, VsmPageKey::PointFace { .. }));
    }

    /// Re-dirties every resident page whose texels can resolve continuous motion:
    /// directional levels up to `max_directional_level` plus the spot and point
    /// spaces (their ranges sit close to the action). The per-frame render budget
    /// paces the resulting refresh churn; static pages on coarser levels stay
    /// cached.
    pub fn mark_dynamic_dirty(&mut self, max_directional_level: u32) {
        let mut dirtied = 0_u32;
        for (key, state) in &mut self.pages {
            let dynamic = match key {
                VsmPageKey::Directional { level, .. } => *level <= max_directional_level,
                VsmPageKey::Spot { .. } | VsmPageKey::PointFace { .. } => true,
            };
            if dynamic && !state.dirty {
                state.dirty = true;
                dirtied += 1;
            }
        }
        self.frame_counters.dirtied += dirtied;
    }

    fn invalidate_matching(&mut self, frame: u64, matches: impl Fn(&VsmPageKey) -> bool) {
        let stale: Vec<VsmPageKey> = self
            .pages
            .keys()
            .filter(|key| matches(key))
            .copied()
            .collect();
        for key in stale {
            let state = self.pages.remove(&key).expect("stale page exists");
            self.cooling.push(CoolingTile {
                tile: state.tile,
                evicted_frame: frame,
            });
        }
    }

    /// The pages to rasterize this frame (dirty ones), clearing their dirty bits.
    pub fn take_render_pages(&mut self, budget: usize) -> Vec<VsmRenderPage> {
        let mut render = Vec::new();
        // Holes first: a dirty page that never rasterized samples as unshadowed,
        // while a re-dirtied page still holds usable last content.
        for fresh_pass in [true, false] {
            for (key, state) in &mut self.pages {
                if render.len() >= budget {
                    break;
                }
                if state.dirty && state.rendered != fresh_pass {
                    state.dirty = false;
                    state.rendered = true;
                    render.push(VsmRenderPage {
                        key: *key,
                        tile: state.tile,
                    });
                }
            }
        }
        self.frame_counters.rendered += u32::try_from(render.len()).unwrap_or(u32::MAX);
        render
    }

    /// Last completed frame's activity counters.
    pub fn counters(&self) -> VsmCounters {
        self.counters
    }

    /// Resident page count.
    pub fn resident(&self) -> usize {
        self.pages.len()
    }

    /// The page's tile when resident.
    pub fn tile(&self, key: VsmPageKey) -> Option<u32> {
        self.pages.get(&key).map(|state| state.tile)
    }
}

/// One directional clip level's snapped window: the light-space origin of its
/// page grid and the derived matrices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VsmDirectionalLevel {
    /// World extent of the level's window in metres.
    pub extent_m: f32,
    /// Snapped window origin in light-plane coordinates (metres).
    pub origin_light: [f32; 2],
    /// Snap grid coordinates (world page units) of the origin, for shift detection.
    pub snap: [i64; 2],
}

/// The directional virtual space: a stable light basis plus one snapped window per
/// clip level, centred on the camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VsmDirectionalSpace {
    /// World → light rotation (right, up, forward rows; forward = light travel).
    pub basis: Mat4,
    /// Per-level snapped windows.
    pub levels: [VsmDirectionalLevel; VSM_DIRECTIONAL_LEVELS as usize],
    /// Light-space forward coordinate of the camera (depth centre).
    pub center_forward: f32,
}

impl VsmDirectionalSpace {
    /// Builds the space for a light direction and camera position. The basis is
    /// deterministic in the direction alone; each level's window snaps to its own
    /// page size so page content is stable under camera travel.
    pub fn build(light_direction: Vec3, camera: Vec3) -> Self {
        let forward = light_direction.normalize_or_zero();
        let reference = if forward.y.abs() > 0.99 {
            Vec3::X
        } else {
            Vec3::Y
        };
        let right = reference.cross(forward).normalize_or_zero();
        let up = forward.cross(right);
        let basis = Mat4::from_cols(
            right.extend(0.0),
            up.extend(0.0),
            forward.extend(0.0),
            Vec3::ZERO.extend(1.0),
        )
        .transpose();
        let camera_light = basis.transform_point3(camera);
        let mut levels = [VsmDirectionalLevel {
            extent_m: 0.0,
            origin_light: [0.0; 2],
            snap: [0; 2],
        }; VSM_DIRECTIONAL_LEVELS as usize];
        for (index, level) in levels.iter_mut().enumerate() {
            let extent = VSM_LEVEL0_EXTENT_M * (1 << index) as f32;
            let page_world = extent / VSM_LEVEL_PAGES as f32;
            let snap_x = ((camera_light.x - extent * 0.5) / page_world).floor() as i64;
            let snap_y = ((camera_light.y - extent * 0.5) / page_world).floor() as i64;
            *level = VsmDirectionalLevel {
                extent_m: extent,
                origin_light: [snap_x as f32 * page_world, snap_y as f32 * page_world],
                snap: [snap_x, snap_y],
            };
        }
        Self {
            basis,
            levels,
            center_forward: camera_light.z,
        }
    }

    /// The [0,1] depth of a light-space forward coordinate — the EXACT mapping the
    /// page matrices rasterize, shared with the shader sampler.
    pub fn depth01(&self, forward: f32) -> f32 {
        (forward - (self.center_forward - VSM_DIRECTIONAL_HALF_DEPTH_M))
            / (2.0 * VSM_DIRECTIONAL_HALF_DEPTH_M)
    }

    /// An explicit ortho over a light-plane rectangle: x/y map the rectangle to
    /// clip [-1,1] and z maps the shared depth span to [0,1] with the same formula
    /// as [`VsmDirectionalSpace::depth01`] — the sampler and the rasterizer agree
    /// by construction.
    fn window_view_proj(&self, min_x: f32, min_y: f32, extent: f32) -> Mat4 {
        let z_min = self.center_forward - VSM_DIRECTIONAL_HALF_DEPTH_M;
        let z_scale = 1.0 / (2.0 * VSM_DIRECTIONAL_HALF_DEPTH_M);
        let projection = Mat4::from_cols(
            saffron_geometry::glam::Vec4::new(2.0 / extent, 0.0, 0.0, 0.0),
            saffron_geometry::glam::Vec4::new(0.0, 2.0 / extent, 0.0, 0.0),
            saffron_geometry::glam::Vec4::new(0.0, 0.0, z_scale, 0.0),
            saffron_geometry::glam::Vec4::new(
                -(2.0 * min_x + extent) / extent,
                -(2.0 * min_y + extent) / extent,
                -z_min * z_scale,
                1.0,
            ),
        );
        projection * self.basis
    }

    /// The world → clip matrix of one PAGE of `level`: the level window's ortho
    /// sub-rectangle covering that page alone.
    pub fn page_view_proj(&self, level: u32, x: u32, y: u32) -> Mat4 {
        let window = &self.levels[level as usize];
        let page_world = window.extent_m / VSM_LEVEL_PAGES as f32;
        self.window_view_proj(
            window.origin_light[0] + x as f32 * page_world,
            window.origin_light[1] + y as f32 * page_world,
            page_world,
        )
    }

    /// The world → clip matrix covering the WHOLE window of `level` (the page
    /// renderer's cull frustum).
    pub fn level_view_proj(&self, level: u32) -> Mat4 {
        let window = &self.levels[level as usize];
        self.window_view_proj(
            window.origin_light[0],
            window.origin_light[1],
            window.extent_m,
        )
    }
}

/// One-shot: clears the atlas to 1.0 (no occluder) and leaves it
/// SHADER_READ_ONLY, so unrendered tiles sample as unshadowed.
fn initialize_atlas(device: &crate::Device, atlas: &Image) -> crate::Result<()> {
    let raw = device.raw();
    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Freed at the end of the function.
    let pool = crate::checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "vsm init pool",
    )?;
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One buffer from the pool above.
    let cmd = crate::checked(
        unsafe { raw.allocate_command_buffers(&alloc) },
        "vsm init cmd",
    )?[0];
    // SAFETY: the ash seam. Default fence.
    let fence = crate::checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "vsm init fence",
    )?;
    let result = (|| -> crate::Result<()> {
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::DEPTH)
            .level_count(1)
            .layer_count(1);
        // SAFETY: the ash seam. The barriers/clear reference this device's image.
        unsafe {
            crate::checked(raw.begin_command_buffer(cmd, &begin), "vsm init begin")?;
            let to_transfer = vk::ImageMemoryBarrier2::default()
                .image(atlas.handle())
                .subresource_range(range)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE);
            let barriers = [to_transfer];
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&barriers),
            );
            raw.cmd_clear_depth_stencil_image(
                cmd,
                atlas.handle(),
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk::ClearDepthStencilValue {
                    depth: 1.0,
                    stencil: 0,
                },
                &[range],
            );
            let to_sampled = vk::ImageMemoryBarrier2::default()
                .image(atlas.handle())
                .subresource_range(range)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                .dst_access_mask(vk::AccessFlags2::SHADER_READ);
            let barriers = [to_sampled];
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&barriers),
            );
            crate::checked(raw.end_command_buffer(cmd), "vsm init end")?;
        }
        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        // SAFETY: the ash seam. The queue is touched single-threaded at init.
        unsafe {
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "vsm init submit")?;
            crate::checked(
                raw.wait_for_fences(&[fence], true, u64::MAX),
                "vsm init wait",
            )?;
        }
        Ok(())
    })();
    // SAFETY: the ash seam. The fence was waited (or the submit never happened).
    unsafe {
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
    result
}

/// Entries in one frame slot's page table (the directional levels, then the spot
/// space).
pub const VSM_TABLE_ENTRIES: usize =
    (VSM_POINT_TABLE_BASE + VSM_POINT_FACES * VSM_POINT_FACE_PAGES * VSM_POINT_FACE_PAGES) as usize;

/// The device side of the virtual shadow map: the one physical depth atlas plus a
/// per-frame-in-flight mapped page-table ring the samplers read through a device
/// address.
pub struct VsmGpu {
    /// The physical page atlas (one D32 depth image, every page a fixed tile).
    pub atlas: Image,
    /// Cross-frame layout state for the render graph.
    pub atlas_state: crate::RgExternalState,
    table_ring: Vec<crate::Buffer>,
}

impl VsmGpu {
    /// Creates the atlas (cleared to 1.0 — no occluder — and left SHADER_READ_ONLY,
    /// so a resident page whose tile has not rendered yet reads unshadowed) and the
    /// table ring.
    pub fn new(device: &crate::Device) -> crate::Result<Self> {
        let resources = device.resources();
        let atlas = Image::new(
            resources,
            &ImageDesc {
                extent: vk::Extent2D {
                    width: VSM_ATLAS_SIZE,
                    height: VSM_ATLAS_SIZE,
                },
                format: crate::pipelines::DEPTH_FORMAT,
                usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_DST,
                aspect: vk::ImageAspectFlags::DEPTH,
                view_type: vk::ImageViewType::TYPE_2D,
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        )?;
        let mut table_ring = Vec::with_capacity(crate::MAX_FRAMES_IN_FLIGHT);
        for _ in 0..crate::MAX_FRAMES_IN_FLIGHT {
            table_ring.push(crate::Buffer::new(
                resources,
                (VSM_TABLE_ENTRIES * 4) as u64,
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?);
        }
        initialize_atlas(device, &atlas)?;
        Ok(Self {
            atlas,
            atlas_state: crate::RgExternalState::new(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
            table_ring,
        })
    }

    /// Publishes the frame's page table: resident mappings for `residency`'s
    /// directional pages of the CURRENT snapped windows, everything else absent.
    /// Returns the slot's device address.
    ///
    /// # Safety contract
    /// The slot's fence completed before reuse (the frame ring guarantees it).
    pub fn publish_table(
        &mut self,
        device: &crate::Device,
        frame: usize,
        residency: &VsmResidency,
    ) -> u64 {
        let mut entries = [0_u32; VSM_TABLE_ENTRIES];
        for level in 0..VSM_DIRECTIONAL_LEVELS {
            for y in 0..VSM_LEVEL_PAGES {
                for x in 0..VSM_LEVEL_PAGES {
                    let key = VsmPageKey::Directional { level, x, y };
                    let index = (level * VSM_LEVEL_PAGES * VSM_LEVEL_PAGES
                        + y * VSM_LEVEL_PAGES
                        + x) as usize;
                    entries[index] = vsm_table_entry(residency.tile(key));
                }
            }
        }
        for y in 0..VSM_SPOT_PAGES {
            for x in 0..VSM_SPOT_PAGES {
                let index = (VSM_SPOT_TABLE_BASE + y * VSM_SPOT_PAGES + x) as usize;
                entries[index] = vsm_table_entry(residency.tile(VsmPageKey::Spot { x, y }));
            }
        }
        for face in 0..VSM_POINT_FACES {
            for y in 0..VSM_POINT_FACE_PAGES {
                for x in 0..VSM_POINT_FACE_PAGES {
                    let index = (VSM_POINT_TABLE_BASE
                        + face * VSM_POINT_FACE_PAGES * VSM_POINT_FACE_PAGES
                        + y * VSM_POINT_FACE_PAGES
                        + x) as usize;
                    entries[index] =
                        vsm_table_entry(residency.tile(VsmPageKey::PointFace { face, x, y }));
                }
            }
        }
        let buffer = &self.table_ring[frame];
        // SAFETY: HOST_VISIBLE + MAPPED; the slot's fence passed before reuse.
        unsafe {
            std::ptr::copy_nonoverlapping(
                entries.as_ptr().cast::<u8>(),
                buffer.mapped_ptr(),
                VSM_TABLE_ENTRIES * 4,
            );
        }
        device.buffer_device_address(buffer.handle())
    }
}

/// The mark pass push: the camera clip → world transform and the depth extent.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VsmDemandPush {
    /// Inverse un-jittered view-projection.
    pub inv_view_proj: [f32; 16],
    /// Depth-source extent in pixels.
    pub extent: [u32; 2],
    /// Reserved ABI words.
    pub reserved: [u32; 2],
}

const _: () = assert!(size_of::<VsmDemandPush>() == 80);

/// The mark push byte size.
pub const VSM_DEMAND_PUSH_SIZE: u32 = size_of::<VsmDemandPush>() as u32;

/// The compact pass push.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VsmCompactPush {
    /// Entry capacity of the request ring.
    pub capacity: u32,
    /// Reserved ABI words.
    pub reserved: [u32; 3],
}

const _: () = assert!(size_of::<VsmCompactPush>() == 16);

/// The compact push byte size.
pub const VSM_COMPACT_PUSH_SIZE: u32 = size_of::<VsmCompactPush>() as u32;

/// Bitmap words covering one bit per page-table entry.
pub const VSM_DEMAND_BITMAP_WORDS: u32 = VSM_TABLE_ENTRIES.div_ceil(32) as u32;
/// Entry capacity of one frame slot's demand-request ring.
pub const VSM_DEMAND_CAPACITY: u32 = 2_048;

/// The GPU receiver-demand apparatus: the marking bitmap, the per-frame request
/// rings the CPU drains, and the two tiny set layouts of the mark/compact passes.
pub struct VsmDemand {
    resources: std::sync::Arc<crate::resources::DeviceResources>,
    mark_layout: vk::DescriptorSetLayout,
    compact_layout: vk::DescriptorSetLayout,
    mark_sets: Vec<vk::DescriptorSet>,
    compact_sets: Vec<vk::DescriptorSet>,
    bitmap: crate::Buffer,
    rings: Vec<crate::Buffer>,
}

impl VsmDemand {
    /// Builds the layouts, sets, the zeroed bitmap, and the mapped request rings.
    pub fn new(device: &crate::Device, descriptors: &crate::Descriptors) -> crate::Result<Self> {
        let raw = device.raw();
        let make = |types: &[vk::DescriptorType]| -> crate::Result<vk::DescriptorSetLayout> {
            let bindings: Vec<vk::DescriptorSetLayoutBinding> = types
                .iter()
                .enumerate()
                .map(|(index, &ty)| {
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(index as u32)
                        .descriptor_type(ty)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::COMPUTE)
                })
                .collect();
            let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
            // SAFETY: the ash seam. The bindings outlive the call; freed in `Drop`.
            crate::checked(
                unsafe { raw.create_descriptor_set_layout(&info, None) },
                "vsm demand layout",
            )
        };
        let mark_layout = make(&[
            vk::DescriptorType::UNIFORM_BUFFER,
            vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            vk::DescriptorType::STORAGE_BUFFER,
        ])?;
        let compact_layout = make(&[
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
        ])?;
        let bitmap = crate::Buffer::new(
            device.resources(),
            u64::from(VSM_DEMAND_BITMAP_WORDS) * 4,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;
        // SAFETY: HOST_VISIBLE + MAPPED; zeroed before any GPU read.
        unsafe {
            std::ptr::write_bytes(
                bitmap.mapped_ptr(),
                0,
                (VSM_DEMAND_BITMAP_WORDS * 4) as usize,
            );
        }
        let mut rings = Vec::with_capacity(crate::MAX_FRAMES_IN_FLIGHT);
        let mut mark_sets = Vec::with_capacity(crate::MAX_FRAMES_IN_FLIGHT);
        let mut compact_sets = Vec::with_capacity(crate::MAX_FRAMES_IN_FLIGHT);
        for _ in 0..crate::MAX_FRAMES_IN_FLIGHT {
            let ring = crate::Buffer::new(
                device.resources(),
                4 + u64::from(VSM_DEMAND_CAPACITY) * 4,
                vk::BufferUsageFlags::STORAGE_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?;
            // SAFETY: HOST_VISIBLE + MAPPED; the count word starts at zero.
            unsafe {
                std::ptr::write_bytes(ring.mapped_ptr(), 0, 4);
            }
            let mark_set = descriptors.allocate_set(mark_layout)?;
            let compact_set = descriptors.allocate_set(compact_layout)?;
            descriptors.write_storage_buffer(mark_set, 2, bitmap.handle(), bitmap.size());
            descriptors.write_storage_buffer(compact_set, 0, bitmap.handle(), bitmap.size());
            descriptors.write_storage_buffer(compact_set, 1, ring.handle(), ring.size());
            rings.push(ring);
            mark_sets.push(mark_set);
            compact_sets.push(compact_set);
        }
        Ok(Self {
            resources: std::sync::Arc::clone(device.resources()),
            mark_layout,
            compact_layout,
            mark_sets,
            compact_sets,
            bitmap,
            rings,
        })
    }

    /// The mark pass's set layout (for pipeline creation).
    pub fn mark_layout(&self) -> vk::DescriptorSetLayout {
        self.mark_layout
    }

    /// The compact pass's set layout (for pipeline creation).
    pub fn compact_layout(&self) -> vk::DescriptorSetLayout {
        self.compact_layout
    }

    /// The frame's sets.
    pub fn sets(&self, frame: usize) -> (vk::DescriptorSet, vk::DescriptorSet) {
        (self.mark_sets[frame], self.compact_sets[frame])
    }

    /// The demand bitmap buffer.
    pub fn bitmap_handle(&self) -> vk::Buffer {
        self.bitmap.handle()
    }

    /// The frame slot's request-ring buffer.
    pub fn ring_handle(&self, frame: usize) -> vk::Buffer {
        self.rings[frame].handle()
    }

    /// Rewrites the frame's mark-set inputs: the light UBO carrying the vsm words
    /// and the depth source the receivers reconstruct from.
    pub fn write_frame(
        &self,
        raw: &ash::Device,
        frame: usize,
        light_ubo: (vk::Buffer, u64),
        depth_view: vk::ImageView,
        sampler: vk::Sampler,
    ) {
        let ubo = [vk::DescriptorBufferInfo {
            buffer: light_ubo.0,
            offset: 0,
            range: light_ubo.1,
        }];
        let depth = [vk::DescriptorImageInfo {
            sampler,
            image_view: depth_view,
            image_layout: vk::ImageLayout::GENERAL,
        }];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(self.mark_sets[frame])
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&ubo),
            vk::WriteDescriptorSet::default()
                .dst_set(self.mark_sets[frame])
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&depth),
        ];
        // SAFETY: the ash seam. The set and resources outlive the call.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };
    }

    /// Drains the frame slot's requests (the slot's fence completed): reads the
    /// count + entries, zeroes the count, and returns deduped raw page-table
    /// indices in append order ([`vsm_demand_key`] decodes them).
    pub fn drain(&mut self, frame: usize) -> Vec<u32> {
        let ring = &self.rings[frame];
        // SAFETY: HOST_VISIBLE + MAPPED; the slot's fence passed.
        let count = unsafe { *ring.mapped_ptr().cast::<u32>() }.min(VSM_DEMAND_CAPACITY);
        let mut out = Vec::with_capacity(count as usize);
        let mut seen = std::collections::HashSet::with_capacity(count as usize);
        for index in 0..count {
            // SAFETY: within the mapped ring's bounds.
            let entry = unsafe { *ring.mapped_ptr().cast::<u32>().add(1 + index as usize) };
            if seen.insert(entry) {
                out.push(entry);
            }
        }
        // SAFETY: HOST_VISIBLE + MAPPED.
        unsafe {
            *ring.mapped_ptr().cast::<u32>() = 0;
        }
        out
    }
}

impl Drop for VsmDemand {
    fn drop(&mut self) {
        let raw = self.resources.device();
        // SAFETY: the ash seam; teardown runs after `wait_gpu_idle`.
        unsafe {
            raw.destroy_descriptor_set_layout(self.mark_layout, None);
            raw.destroy_descriptor_set_layout(self.compact_layout, None);
        }
    }
}

/// The crop matrix mapping a projective light's clip space so that page `(x, y)`
/// of its `pages`² grid fills the viewport: `crop · lightViewProj` rasterizes
/// exactly that page's content. The grid indexes `ndc.xy * 0.5 + 0.5` UV space —
/// the same mapping the demand shader and samplers use, so raster rows and
/// sampled texels agree without any flip.
pub fn vsm_page_crop(pages: u32, x: u32, y: u32) -> Mat4 {
    let pages = pages as f32;
    // uv = ndc * 0.5 + 0.5; page (x, y) covers uv in [x, x+1)/pages. Solve for
    // the ndc scale/offset that maps that sub-rect onto [-1, 1].
    let scale = pages;
    let offset_x = -(2.0 * (x as f32 + 0.5) / pages - 1.0) * scale;
    let offset_y = -(2.0 * (y as f32 + 0.5) / pages - 1.0) * scale;
    Mat4::from_cols(
        saffron_geometry::glam::Vec4::new(scale, 0.0, 0.0, 0.0),
        saffron_geometry::glam::Vec4::new(0.0, scale, 0.0, 0.0),
        saffron_geometry::glam::Vec4::new(0.0, 0.0, 1.0, 0.0),
        saffron_geometry::glam::Vec4::new(offset_x, offset_y, 0.0, 1.0),
    )
}

/// Decodes a demand-request entry (a raw page-table index appended by
/// `vsm_demand_compact.slang`) into its logical page.
pub fn vsm_demand_key(index: u32) -> Option<VsmPageKey> {
    if index < VSM_SPOT_TABLE_BASE {
        let level = index / (VSM_LEVEL_PAGES * VSM_LEVEL_PAGES);
        let cell = index % (VSM_LEVEL_PAGES * VSM_LEVEL_PAGES);
        Some(VsmPageKey::Directional {
            level,
            x: cell % VSM_LEVEL_PAGES,
            y: cell / VSM_LEVEL_PAGES,
        })
    } else if index < VSM_POINT_TABLE_BASE {
        let cell = index - VSM_SPOT_TABLE_BASE;
        Some(VsmPageKey::Spot {
            x: cell % VSM_SPOT_PAGES,
            y: cell / VSM_SPOT_PAGES,
        })
    } else if (index as usize) < VSM_TABLE_ENTRIES {
        let cell = index - VSM_POINT_TABLE_BASE;
        let per_face = VSM_POINT_FACE_PAGES * VSM_POINT_FACE_PAGES;
        Some(VsmPageKey::PointFace {
            face: cell / per_face,
            x: cell % VSM_POINT_FACE_PAGES,
            y: (cell % per_face) / VSM_POINT_FACE_PAGES,
        })
    } else {
        None
    }
}

/// Packs one page-table entry.
pub fn vsm_table_entry(tile: Option<u32>) -> u32 {
    match tile {
        Some(tile) => VSM_TABLE_RESIDENT | tile,
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_snap_deterministically_and_shift_by_pages() {
        // Straight-down light: light-plane coordinates track world XZ exactly, so
        // the snap arithmetic is inspectable against the page sizes (level 0 pages
        // are 1 m, level 7 pages 128 m).
        let direction = Vec3::NEG_Y;
        let a = VsmDirectionalSpace::build(direction, Vec3::new(10.25, 2.0, 5.25));
        let b = VsmDirectionalSpace::build(direction, Vec3::new(10.25, 2.0, 5.25));
        assert_eq!(a, b, "equal inputs snap equally");
        // A move well inside every page size keeps every level's window.
        let c = VsmDirectionalSpace::build(direction, Vec3::new(10.55, 2.0, 5.45));
        assert_eq!(a.levels[0].snap, c.levels[0].snap);
        assert_eq!(a.levels[7].snap, c.levels[7].snap);
        // A multi-metre move shifts the fine level but not the 128 m coarse pages.
        let d = VsmDirectionalSpace::build(direction, Vec3::new(25.25, 2.0, 30.25));
        assert_ne!(a.levels[0].snap, d.levels[0].snap);
        assert_eq!(a.levels[7].snap, d.levels[7].snap);
    }

    #[test]
    fn residency_allocates_evicts_lru_and_cools_tiles() {
        let mut residency = VsmResidency::default();
        let key = |x: u32| VsmPageKey::Directional { level: 0, x, y: 0 };
        let total = VSM_ATLAS_TILES * VSM_ATLAS_TILES;
        for index in 0..total {
            assert!(
                residency
                    .demand(
                        VsmPageKey::Directional {
                            level: index / VSM_LEVEL_PAGES,
                            x: index % VSM_LEVEL_PAGES,
                            y: 1,
                        },
                        1,
                    )
                    .is_some()
            );
        }
        assert_eq!(residency.resident(), total as usize);
        // The atlas is full and everything was demanded this frame: no tile is safe.
        assert_eq!(residency.demand(key(0), 1), None);
        // Next frame the LRU victim evicts, but its tile cools — the SECOND demand
        // (after the cooldown) succeeds with the recycled tile.
        assert_eq!(residency.demand(key(0), 2), None);
        residency.begin_frame(5);
        let tile = residency.demand(key(0), 5);
        assert!(tile.is_some(), "cooled tile recycles");
        // A re-demand of a resident page returns its tile without churn.
        assert_eq!(residency.demand(key(0), 6), tile);

        // Dirty pages drain once within the budget.
        let mut fresh = VsmResidency::default();
        fresh.demand(key(3), 1);
        fresh.demand(key(4), 1);
        assert_eq!(fresh.take_render_pages(8).len(), 2);
        assert!(fresh.take_render_pages(8).is_empty(), "dirty bits clear");
        fresh.mark_dirty(key(3));
        assert_eq!(fresh.take_render_pages(8).len(), 1);
    }

    #[test]
    fn page_matrices_tile_the_level_window() {
        let space =
            VsmDirectionalSpace::build(Vec3::new(-0.3, -0.9, -0.2), Vec3::new(0.0, 1.5, 0.0));
        // A world point inside page (x, y) of level 0 projects inside that page's
        // clip and outside its neighbour's.
        let window = &space.levels[0];
        let page_world = window.extent_m / VSM_LEVEL_PAGES as f32;
        let light_point = Vec3::new(
            window.origin_light[0] + page_world * 2.5,
            window.origin_light[1] + page_world * 4.5,
            space.center_forward,
        );
        let world_point = space.basis.transpose().transform_point3(light_point);
        let own = space.page_view_proj(0, 2, 4) * world_point.extend(1.0);
        assert!(own.x.abs() <= 1.0 && own.y.abs() <= 1.0, "{own:?}");
        let neighbour = space.page_view_proj(0, 3, 4) * world_point.extend(1.0);
        assert!(neighbour.x.abs() > 1.0, "{neighbour:?}");
        // The matrix z equals the shared depth01 formula (the sampler contract).
        assert!(
            (own.z - space.depth01(light_point.z)).abs() < 1e-5,
            "{own:?}"
        );
    }

    #[test]
    fn spot_page_crop_agrees_with_the_sampler_grid() {
        // A clip-space point maps to UV (ndc * 0.5 + 0.5); the sampler taps page
        // uv*16 at tile fraction frac(uv*16). The crop matrix of that page must
        // rasterize the same point at the same in-tile position: cropped ndc
        // remapped to [0,1] equals the sampler's page fraction, both axes.
        for &(ndc_x, ndc_y) in &[(-0.83_f32, 0.4_f32), (0.07, -0.66), (0.99, 0.99)] {
            let uv = [ndc_x * 0.5 + 0.5, ndc_y * 0.5 + 0.5];
            let scaled = [uv[0] * VSM_SPOT_PAGES as f32, uv[1] * VSM_SPOT_PAGES as f32];
            let page = [scaled[0] as u32, scaled[1] as u32];
            let frac = [scaled[0] - page[0] as f32, scaled[1] - page[1] as f32];
            let crop = vsm_page_crop(VSM_SPOT_PAGES, page[0], page[1]);
            let cropped = crop * saffron_geometry::glam::Vec4::new(ndc_x, ndc_y, 0.5, 1.0);
            assert!(
                (cropped.x * 0.5 + 0.5 - frac[0]).abs() < 1e-5,
                "{cropped:?}"
            );
            assert!(
                (cropped.y * 0.5 + 0.5 - frac[1]).abs() < 1e-5,
                "{cropped:?}"
            );
            let neighbour = vsm_page_crop(VSM_SPOT_PAGES, (page[0] + 1) % VSM_SPOT_PAGES, page[1])
                * saffron_geometry::glam::Vec4::new(ndc_x, ndc_y, 0.5, 1.0);
            assert!(neighbour.x.abs() > 1.0, "{neighbour:?}");
        }
    }

    #[test]
    fn demand_indices_decode_to_every_space() {
        // The table index of a key and vsm_demand_key are inverses across all
        // three spaces; out-of-range indices decode to nothing.
        assert_eq!(
            vsm_demand_key(3 * 1024 + 5 * VSM_LEVEL_PAGES + 7),
            Some(VsmPageKey::Directional {
                level: 3,
                x: 7,
                y: 5
            })
        );
        assert_eq!(
            vsm_demand_key(VSM_SPOT_TABLE_BASE + 9 * VSM_SPOT_PAGES + 4),
            Some(VsmPageKey::Spot { x: 4, y: 9 })
        );
        assert_eq!(
            vsm_demand_key(
                VSM_POINT_TABLE_BASE
                    + 5 * VSM_POINT_FACE_PAGES * VSM_POINT_FACE_PAGES
                    + 6 * VSM_POINT_FACE_PAGES
                    + 2
            ),
            Some(VsmPageKey::PointFace {
                face: 5,
                x: 2,
                y: 6
            })
        );
        assert_eq!(vsm_demand_key(VSM_TABLE_ENTRIES as u32), None);
    }

    #[test]
    fn dynamic_dirty_and_hole_priority() {
        let mut residency = VsmResidency::default();
        let fine = VsmPageKey::Directional {
            level: 0,
            x: 1,
            y: 1,
        };
        let coarse = VsmPageKey::Directional {
            level: 7,
            x: 1,
            y: 1,
        };
        let spot = VsmPageKey::Spot { x: 0, y: 0 };
        residency.demand(fine, 1);
        residency.demand(coarse, 1);
        residency.demand(spot, 1);
        assert_eq!(residency.take_render_pages(8).len(), 3);
        // The dynamic sweep re-dirties fine + spot pages; the coarse level stays
        // cached.
        residency.mark_dynamic_dirty(VSM_DYNAMIC_MAX_LEVEL);
        let dynamic: Vec<VsmPageKey> = residency
            .take_render_pages(8)
            .into_iter()
            .map(|page| page.key)
            .collect();
        assert_eq!(dynamic.len(), 2);
        assert!(dynamic.contains(&fine) && dynamic.contains(&spot));
        // A never-rendered hole beats a re-dirtied page under a budget of one.
        let hole = VsmPageKey::Directional {
            level: 2,
            x: 3,
            y: 3,
        };
        residency.demand(hole, 2);
        residency.mark_dirty(fine);
        let first = residency.take_render_pages(1);
        assert_eq!(first[0].key, hole, "the hole renders first");
        assert_eq!(residency.take_render_pages(1)[0].key, fine);
    }
}
