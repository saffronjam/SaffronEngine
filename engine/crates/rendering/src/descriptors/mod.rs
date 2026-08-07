//! The device-global descriptor state built once at startup: the set layouts, the two pools, the
//! single global bindless combined-image-sampler set, the samplers, and the bindless slot allocator
//! with its reclaim free-list.
//!
//! Set 0 holds runtime-sized descriptor arrays, partially bound and update-after-bind, clamped to
//! the device's aggregate descriptor limits. The default white texture takes slot 0;
//! [`Descriptors::claim_slot`] pops the reclaim free-list before growing the high-water index, so a
//! churny scene stays bounded. Both the free-list and the `vkUpdateDescriptorSets` write take the
//! bindless mutex, because the thumbnail worker uploads off the main thread. Every
//! [`crate::GpuTexture`] holds a clone of the free-list `Arc` so its `Drop` returns its slot.

mod layouts;
mod partial;

use std::sync::{Arc, Mutex};

use ash::vk;

use crate::resources::{BindlessFreeList, DeviceResources};
use crate::{Device, Error, Result, checked};

use layouts::*;
use partial::*;

/// Engine maximum for the bindless texture array (set 0).
pub const MAX_BINDLESS_TEXTURES: u32 = 1024;

/// Engine maximum for the bindless per-mesh SDF array (set 0, binding 1). One global
/// `Texture3D` combined-image-sampler array the lighting cone-trace indexes per static
/// instance; one slot per unique baked field. A mesh bakes one field per spatial cell
/// (the whole-mesh partition that localizes indoor GI), so a large scene reaches the low
/// thousands of distinct fields — update-after-bind + partially-bound means only occupied
/// slots consume resources, so this is a generous ceiling, not a fixed cost.
pub const MAX_BINDLESS_SDF: u32 = 1024;

/// The bindless slot the default white texture occupies. Every material that names
/// no albedo texture indexes this slot, so it is claimed first at init and never
/// reclaimed.
pub const DEFAULT_WHITE_SLOT: u32 = 0;

/// Hard cap on reflection probes. The IBL set's probe-cube arrays are sized to it.
pub const MAX_REFLECTION_PROBES: u32 = 8;

/// The maximum bloom mip-pyramid depth (≈6 levels at 1080p, 7 at 1440p+). The transient chain,
/// the per-view bloom descriptor sets, and the mip-key table are all sized against this.
pub(crate) const MAX_BLOOM_MIPS: usize = 7;

/// The per-frame-in-flight bloom descriptor-set count: one set per pyramid pass —
/// `MAX_BLOOM_MIPS` downsamples + `MAX_BLOOM_MIPS - 1` upsamples + one composite (`2 *
/// MAX_BLOOM_MIPS - 1`) plus the two anamorphic streak ping-pong passes. The set stride, the pool
/// budget, and the per-view allocation are all sized against it.
pub(crate) const BLOOM_PASSES_PER_FRAME: usize = 2 * MAX_BLOOM_MIPS + 2;

/// The number of editor render views (scene + asset-preview + offscreen thumbnail). The
/// general descriptor pool sizes its per-view post-process headroom against this.
const VIEW_COUNT: u32 = 3;

/// The device-global descriptor infrastructure: layouts, pools, the bindless set,
/// the samplers, and the bindless slot allocator.
///
/// Built once in [`Descriptors::new`], then borrowed `&Descriptors` — its layouts
/// and samplers are immutable after init. The one piece that mutates is the bindless
/// slot allocator (`next_index` + the shared `free_list`), guarded by the bindless
/// mutex so the thumbnail worker can upload concurrently.
///
/// Owns an [`Arc`]`<`[`DeviceResources`]`>` so its [`Drop`] frees its pools, layouts,
/// and samplers without a live `&Device` (the same structural-outlives discipline as
/// the resource wrappers): the device is destroyed only when the last `Arc` holder
/// drops, after every descriptor here is freed. The bindless *set* is freed
/// implicitly with its pool, so it needs no explicit teardown.
pub struct Descriptors {
    resources: Arc<DeviceResources>,

    linear_sampler: vk::Sampler,
    shadow_sampler: vk::Sampler,
    /// Linear, clamp-to-edge sampler the per-mesh SDF cone-trace reads its `Texture3D`
    /// with (a repeat wrap would alias the field's positive shell at the grid border).
    sdf_sampler: vk::Sampler,
    /// Point (nearest) sampler the tessellation factor kernel reads the per-height min/max pyramid
    /// with. Nearest min/mag/mip so a level's `(min, max)` texel is read without blending min into max
    /// (linear filtering would break the conservative bound); clamp-to-edge so an edge UV reads the
    /// boundary texel rather than wrapping.
    minmax_sampler: vk::Sampler,

    bindless_set_layout: vk::DescriptorSetLayout,
    light_set_layout: vk::DescriptorSetLayout,
    instance_set_layout: vk::DescriptorSetLayout,
    ibl_set_layout: vk::DescriptorSetLayout,
    ssao_mesh_set_layout: vk::DescriptorSetLayout,
    ddgi_mesh_set_layout: vk::DescriptorSetLayout,
    rt_mesh_set_layout: Option<vk::DescriptorSetLayout>,
    restir_mesh_set_layout: Option<vk::DescriptorSetLayout>,
    cluster_set_layout: vk::DescriptorSetLayout,
    tonemap_set_layout: vk::DescriptorSetLayout,
    fog_set_layout: vk::DescriptorSetLayout,
    fxaa_set_layout: vk::DescriptorSetLayout,
    bloom_set_layout: vk::DescriptorSetLayout,
    taa_set_layout: vk::DescriptorSetLayout,
    depth_upscale_set_layout: vk::DescriptorSetLayout,

    descriptor_pool: vk::DescriptorPool,
    bindless_pool: vk::DescriptorPool,
    bindless_set: vk::DescriptorSet,

    texture_capacity: u32,
    slots: Mutex<SlotAllocator>,
    free_list: BindlessFreeList,

    /// The per-mesh SDF bindless slot allocator (binding 1 of the bindless set), with
    /// its own high-water mark + reclaim free-list, the same bounded-pool discipline as
    /// the albedo allocator above.
    sdf_capacity: u32,
    sdf_slots: Mutex<SlotAllocator>,
    sdf_free_list: BindlessFreeList,
}

/// The bindless slot allocator: the high-water `next_index` and a reference to the
/// shared reclaim free-list. Lives behind the [`Descriptors`] bindless `Mutex` so a
/// claim that grows `next_index` and a reclaim that pushes the free-list never race
/// (one lock covers both).
struct SlotAllocator {
    next_index: u32,
    /// The array's fixed capacity (`descriptor_count` of the binding). A claim past it
    /// returns `None` rather than handing out an out-of-range `dstArrayElement`.
    cap: u32,
    free_list: BindlessFreeList,
}

impl SlotAllocator {
    /// Hands out the next bindless slot: reuse a reclaimed one (LIFO) if any, else grow
    /// `next_index` while it stays under `cap`. Returns `None` when the array is full (the
    /// free-list is empty and the high-water mark reached the capacity) so the caller skips
    /// the write instead of indexing past the bindless array.
    fn claim(&mut self) -> Option<u32> {
        if let Ok(mut free) = self.free_list.lock()
            && let Some(slot) = free.pop()
        {
            return Some(slot);
        }
        if self.next_index >= self.cap {
            return None;
        }
        let slot = self.next_index;
        self.next_index += 1;
        Some(slot)
    }
}

impl Descriptors {
    /// Builds the descriptor infrastructure: the samplers, the seven device-global
    /// layouts, the two pools, and the single bindless set, then claims slot 0 for
    /// the default white texture so the first uploaded slot is 1.
    ///
    /// The `free_list` is the shared [`BindlessFreeList`] every [`crate::GpuTexture`]
    /// clones — passed in so the [`Device`] (or the renderer) owns the single
    /// canonical `Arc` that outlives both these descriptors and any texture whose
    /// `Drop` pushes to it.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing Vulkan call; already-created
    /// handles are freed before returning on a partial failure.
    pub fn new(device: &Device, free_list: &BindlessFreeList) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let raw = resources.device();
        let bindless_capacity = device.capabilities.max_bindless_array_elements;
        if bindless_capacity == 0 {
            return Err(Error::InvalidUploadData(
                "device exposes no capacity for the bindless descriptor table".to_owned(),
            ));
        }
        let texture_capacity = MAX_BINDLESS_TEXTURES.min(bindless_capacity);
        let sdf_capacity = MAX_BINDLESS_SDF.min(bindless_capacity);

        // Build everything into a partial set so a mid-init failure can free what was
        // already created (the `Partial` Drop reclaims). `?` over each step
        // short-circuits to the cleanup.
        let mut partial = Partial::new(&resources);

        partial.linear_sampler = Some(create_linear_sampler(
            raw,
            device.capabilities.max_anisotropy,
        )?);
        partial.shadow_sampler = Some(create_shadow_sampler(raw)?);
        partial.sdf_sampler = Some(create_sdf_sampler(raw)?);
        partial.minmax_sampler = Some(create_minmax_sampler(raw)?);

        partial.bindless_set_layout =
            Some(create_bindless_layout(raw, texture_capacity, sdf_capacity)?);
        partial.light_set_layout = Some(create_light_layout(raw, partial.shadow_sampler.unwrap())?);
        partial.instance_set_layout = Some(create_instance_layout(
            raw,
            device.capabilities.mesh_shader,
        )?);
        partial.ibl_set_layout = Some(create_ibl_layout(raw)?);
        partial.ssao_mesh_set_layout = Some(create_ssao_mesh_layout(
            raw,
            partial.linear_sampler.unwrap(),
        )?);
        partial.ddgi_mesh_set_layout = Some(create_ddgi_mesh_layout(raw)?);
        // Sets 6/7 (TLAS + ReSTIR radiance) need the AS extension, so they exist only
        // when RT is supported; the mesh PSO appends them to its layout only then.
        if device.capabilities.rt_supported {
            partial.rt_mesh_set_layout = Some(create_rt_mesh_layout(
                raw,
                device.capabilities.partitioned_acceleration_structure,
            )?);
            partial.restir_mesh_set_layout = Some(create_restir_mesh_layout(raw)?);
        }
        partial.cluster_set_layout = Some(create_cluster_layout(raw)?);
        partial.tonemap_set_layout = Some(create_tonemap_layout(raw)?);
        partial.fog_set_layout = Some(create_fog_layout(raw)?);
        partial.fxaa_set_layout = Some(create_fxaa_layout(raw)?);
        partial.bloom_set_layout = Some(create_bloom_layout(raw)?);
        partial.taa_set_layout = Some(create_taa_layout(raw)?);
        partial.depth_upscale_set_layout = Some(create_depth_upscale_layout(raw)?);

        partial.descriptor_pool = Some(create_descriptor_pool(
            raw,
            device.capabilities.rt_supported,
            device.capabilities.partitioned_acceleration_structure,
        )?);
        partial.bindless_pool = Some(create_bindless_pool(raw, texture_capacity, sdf_capacity)?);

        let bindless_set = allocate_bindless_set(
            raw,
            partial.bindless_pool.unwrap(),
            partial.bindless_set_layout.unwrap(),
        )?;

        // Slot 0 is the default white texture: claim it up front so the allocator's
        // high-water mark starts at 1 and the first uploaded texture gets slot 1.
        let mut allocator = SlotAllocator {
            next_index: 0,
            cap: texture_capacity,
            free_list: Arc::clone(free_list),
        };
        let white_slot = allocator.claim().expect("default white slot");
        debug_assert_eq!(white_slot, DEFAULT_WHITE_SLOT);

        // The per-mesh SDF array's own allocator + reclaim free-list. No slot is reserved
        // up front (a mesh without a baked field simply claims none).
        let sdf_free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let sdf_allocator = SlotAllocator {
            next_index: 0,
            cap: sdf_capacity,
            free_list: Arc::clone(&sdf_free_list),
        };

        tracing::info!(
            "bindless descriptor table ready ({} albedo + {} sdf slots, update-after-bind)",
            texture_capacity,
            sdf_capacity
        );

        Ok(Self {
            resources: Arc::clone(&resources),
            linear_sampler: partial.take_linear_sampler(),
            shadow_sampler: partial.take_shadow_sampler(),
            sdf_sampler: partial.take_sdf_sampler(),
            minmax_sampler: partial.take_minmax_sampler(),
            bindless_set_layout: partial.take_bindless_set_layout(),
            light_set_layout: partial.take_light_set_layout(),
            instance_set_layout: partial.take_instance_set_layout(),
            ibl_set_layout: partial.take_ibl_set_layout(),
            ssao_mesh_set_layout: partial.take_ssao_mesh_set_layout(),
            ddgi_mesh_set_layout: partial.take_ddgi_mesh_set_layout(),
            rt_mesh_set_layout: partial.rt_mesh_set_layout.take(),
            restir_mesh_set_layout: partial.restir_mesh_set_layout.take(),
            cluster_set_layout: partial.take_cluster_set_layout(),
            tonemap_set_layout: partial.take_tonemap_set_layout(),
            fog_set_layout: partial.take_fog_set_layout(),
            fxaa_set_layout: partial.take_fxaa_set_layout(),
            bloom_set_layout: partial.take_bloom_set_layout(),
            taa_set_layout: partial.take_taa_set_layout(),
            depth_upscale_set_layout: partial.take_depth_upscale_set_layout(),
            descriptor_pool: partial.take_descriptor_pool(),
            bindless_pool: partial.take_bindless_pool(),
            bindless_set,
            texture_capacity,
            slots: Mutex::new(allocator),
            free_list: Arc::clone(free_list),
            sdf_capacity,
            sdf_slots: Mutex::new(sdf_allocator),
            sdf_free_list,
        })
    }

    /// The linear repeat sampler (the default texture sampler, also used by the
    /// bindless writes and the point-shadow cube lookup).
    pub fn linear_sampler(&self) -> vk::Sampler {
        self.linear_sampler
    }

    /// The depth-compare PCF sampler for directional/spot shadow-map lookups.
    pub fn shadow_sampler(&self) -> vk::Sampler {
        self.shadow_sampler
    }

    /// Set 0: the bindless combined-image-sampler array layout.
    pub fn bindless_set_layout(&self) -> vk::DescriptorSetLayout {
        self.bindless_set_layout
    }

    /// Set 1: the directional/punctual light + shadow layout.
    pub fn light_set_layout(&self) -> vk::DescriptorSetLayout {
        self.light_set_layout
    }

    /// Set 2: the per-instance + joint-palette + material-params layout.
    pub fn instance_set_layout(&self) -> vk::DescriptorSetLayout {
        self.instance_set_layout
    }

    /// Set 3 in the mesh pipeline: the IBL set (global sky SH/prefiltered/BRDF +
    /// the reflection-probe cube arrays + probe metadata). The mesh PSO layout binds
    /// it; the descriptor set + its data resources land in the IBL phase.
    pub fn ibl_set_layout(&self) -> vk::DescriptorSetLayout {
        self.ibl_set_layout
    }

    /// Set 4 in the mesh pipeline: the screen-space AO + contact + SSGI sampler set.
    pub fn ssao_mesh_set_layout(&self) -> vk::DescriptorSetLayout {
        self.ssao_mesh_set_layout
    }

    /// Set 5 in the mesh pipeline: the DDGI irradiance + distance sampler set.
    pub fn ddgi_mesh_set_layout(&self) -> vk::DescriptorSetLayout {
        self.ddgi_mesh_set_layout
    }

    /// Set 6 in the mesh pipeline: the ray-tracing TLAS set — present only when RT is
    /// supported (the layout needs the acceleration-structure extension).
    pub fn rt_mesh_set_layout(&self) -> Option<vk::DescriptorSetLayout> {
        self.rt_mesh_set_layout
    }

    /// Set 7 in the mesh pipeline: the ReSTIR radiance sampler set — present only when
    /// RT is supported (it rides the RT path).
    pub fn restir_mesh_set_layout(&self) -> Option<vk::DescriptorSetLayout> {
        self.restir_mesh_set_layout
    }

    /// The clustered-light-culling compute layout.
    pub fn cluster_set_layout(&self) -> vk::DescriptorSetLayout {
        self.cluster_set_layout
    }

    /// The tonemap compute layout (one storage image).
    pub fn tonemap_set_layout(&self) -> vk::DescriptorSetLayout {
        self.tonemap_set_layout
    }

    /// The height-fog compute layout: offscreen storage image (0), the fog params UBO (1, a
    /// dynamic-offset UBO), the scene depth (2), and the sky-view LUT (3).
    pub fn fog_set_layout(&self) -> vk::DescriptorSetLayout {
        self.fog_set_layout
    }

    /// The FXAA compute layout (sampler source + storage-image target).
    pub fn fxaa_set_layout(&self) -> vk::DescriptorSetLayout {
        self.fxaa_set_layout
    }

    /// The bloom compute layout (sampler source + storage-image target), one set per pyramid pass.
    pub fn bloom_set_layout(&self) -> vk::DescriptorSetLayout {
        self.bloom_set_layout
    }

    /// The TAA resolve compute layout (current/history/motion samplers + offscreen/
    /// history storage images).
    pub fn taa_set_layout(&self) -> vk::DescriptorSetLayout {
        self.taa_set_layout
    }

    /// The depth-upscale graphics layout (one fragment sampler: the input scene depth).
    pub fn depth_upscale_layout(&self) -> vk::DescriptorSetLayout {
        self.depth_upscale_set_layout
    }

    /// The descriptor pool the per-frame light/instance/cluster + per-view sets are
    /// allocated from (`FREE_DESCRIPTOR_SET` so texture sets free on drop).
    pub fn descriptor_pool(&self) -> vk::DescriptorPool {
        self.descriptor_pool
    }

    /// The single global bindless set bound as set 0 by every draw.
    pub fn bindless_set(&self) -> vk::DescriptorSet {
        self.bindless_set
    }

    /// The shared reclaim free-list every [`crate::GpuTexture`] clones so its `Drop`
    /// returns its slot.
    pub fn free_list(&self) -> &BindlessFreeList {
        &self.free_list
    }

    /// Number of texture and height-pyramid slots exposed by the bindless set.
    pub fn texture_capacity(&self) -> u32 {
        self.texture_capacity
    }

    /// Claims a stable bindless slot, reusing a reclaimed one if available, under the
    /// bindless mutex. The upload path then writes the texture into this slot with
    /// [`Descriptors::write_texture`] and constructs the [`crate::GpuTexture`] holding
    /// the free-list clone. `None` when the device-sized array is full; the caller then
    /// fails the upload rather than writing out of range.
    pub fn claim_slot(&self) -> Option<u32> {
        self.slots
            .lock()
            .expect("bindless slot allocator lock")
            .claim()
    }

    /// Writes `view` into bindless slot `index` of the global set with the linear
    /// sampler, under the bindless mutex (host access to a descriptor set is
    /// externally synchronized; the worker writes it too). The image must
    /// be in `SHADER_READ_ONLY_OPTIMAL` when sampled (the graph guarantees it).
    pub fn write_texture(&self, view: vk::ImageView, index: u32) {
        let image_info = [vk::DescriptorImageInfo {
            sampler: self.linear_sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.bindless_set)
            .dst_binding(0)
            .dst_array_element(index)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&image_info);
        let _guard = self.slots.lock().expect("bindless slot allocator lock");
        // SAFETY: the ash seam. The set + layout outlive this; the view is valid for
        // the call. The lock serializes concurrent worker/main writes to the set.
        unsafe {
            self.resources
                .device()
                .update_descriptor_sets(&[write], &[])
        };
    }

    /// Writes `view` into *every* bindless slot of the global set with the linear
    /// sampler, in one `vkUpdateDescriptorSets`. Called once at init with the default
    /// white view so no slot is ever sampled while unbound: some drivers (lavapipe)
    /// fault sampling a partially-bound array even on a slot a shader never reads, and
    /// it is undefined behaviour on real hardware. Real uploads overwrite their slot
    /// afterwards.
    pub fn seed_all_textures(&self, view: vk::ImageView) {
        let image_info = vec![
            vk::DescriptorImageInfo {
                sampler: self.linear_sampler,
                image_view: view,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            };
            self.texture_capacity as usize
        ];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.bindless_set)
            .dst_binding(0)
            .dst_array_element(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&image_info);
        let _guard = self.slots.lock().expect("bindless slot allocator lock");
        // SAFETY: the ash seam. The set + layout outlive this; `view` is valid for the
        // call and `image_info` lives until the call returns. The lock serializes the
        // concurrent worker/main writes to the set (this runs only at init, but the
        // mutex discipline is uniform). The array length equals the layout's count.
        unsafe {
            self.resources
                .device()
                .update_descriptor_sets(&[write], &[]);
        }
    }

    /// The per-mesh SDF sampler (linear, clamp-to-edge) the cone-trace reads the
    /// `Texture3D` field with.
    pub fn sdf_sampler(&self) -> vk::Sampler {
        self.sdf_sampler
    }

    /// The shared reclaim free-list every [`crate::GpuSdf`] clones so its `Drop` returns
    /// its SDF bindless slot (binding 1 of the bindless set).
    pub fn sdf_free_list(&self) -> &BindlessFreeList {
        &self.sdf_free_list
    }

    /// Number of per-mesh SDF slots exposed by the bindless set.
    pub fn sdf_capacity(&self) -> u32 {
        self.sdf_capacity
    }

    /// Claims a stable per-mesh SDF bindless slot (binding 1), reusing a reclaimed one if
    /// available, under the SDF allocator mutex. The upload path writes the field's view
    /// into this slot with [`Descriptors::write_sdf_texture`] and constructs the
    /// [`crate::GpuSdf`] holding the free-list clone. `None` when the device-sized array
    /// is full; the caller then skips the field rather than writing an out-of-range
    /// `dstArrayElement`.
    pub fn claim_sdf_slot(&self) -> Option<u32> {
        self.sdf_slots
            .lock()
            .expect("bindless sdf slot allocator lock")
            .claim()
    }

    /// Writes the brick-atlas `view` into SDF bindless slot `index` (binding 1) with the
    /// SDF sampler, the brick-indirection `indirection` view into the same slot at binding 2
    /// (a sampled image, no sampler), and the coarse `coverage` view at binding 3 (combined
    /// image sampler). The images must be in `SHADER_READ_ONLY_OPTIMAL` when sampled (the
    /// upload transitions them there). Serialized under the SDF allocator mutex (the worker
    /// thread can upload meshes off the main thread).
    pub fn write_sdf_texture(
        &self,
        view: vk::ImageView,
        indirection: vk::ImageView,
        coverage: vk::ImageView,
        index: u32,
    ) {
        let atlas_info = [vk::DescriptorImageInfo {
            sampler: self.sdf_sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let indir_info = [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: indirection,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let coverage_info = [vk::DescriptorImageInfo {
            sampler: self.sdf_sampler,
            image_view: coverage,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(self.bindless_set)
                .dst_binding(1)
                .dst_array_element(index)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&atlas_info),
            vk::WriteDescriptorSet::default()
                .dst_set(self.bindless_set)
                .dst_binding(2)
                .dst_array_element(index)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&indir_info),
            vk::WriteDescriptorSet::default()
                .dst_set(self.bindless_set)
                .dst_binding(3)
                .dst_array_element(index)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&coverage_info),
        ];
        let _guard = self
            .sdf_slots
            .lock()
            .expect("bindless sdf slot allocator lock");
        // SAFETY: the ash seam. The set + layout outlive this; both views are valid for the
        // call. The lock serializes concurrent worker/main writes to the set.
        unsafe { self.resources.device().update_descriptor_sets(&writes, &[]) };
    }

    /// Seeds the default atlas + indirection + coverage views into *every* SDF bindless slot
    /// (bindings 1 + 2 + 3) in one `vkUpdateDescriptorSets`. Called once at init (from
    /// [`crate::Uploader::upload_default_sdf`]) with the default "empty space" SDF so the
    /// partially-bound `Texture3D` arrays are never sampled while unbound (lavapipe faults
    /// on an unbound slot even one the shader never reads, and it is UB on real hardware).
    /// Real SDF uploads overwrite their slot afterwards.
    pub fn seed_all_sdf_textures(
        &self,
        atlas: vk::ImageView,
        indirection: vk::ImageView,
        coverage: vk::ImageView,
    ) {
        let atlas_info = vec![
            vk::DescriptorImageInfo {
                sampler: self.sdf_sampler,
                image_view: atlas,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            };
            self.sdf_capacity as usize
        ];
        let indir_info = vec![
            vk::DescriptorImageInfo {
                sampler: vk::Sampler::null(),
                image_view: indirection,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            };
            self.sdf_capacity as usize
        ];
        let coverage_info = vec![
            vk::DescriptorImageInfo {
                sampler: self.sdf_sampler,
                image_view: coverage,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            };
            self.sdf_capacity as usize
        ];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(self.bindless_set)
                .dst_binding(1)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&atlas_info),
            vk::WriteDescriptorSet::default()
                .dst_set(self.bindless_set)
                .dst_binding(2)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&indir_info),
            vk::WriteDescriptorSet::default()
                .dst_set(self.bindless_set)
                .dst_binding(3)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&coverage_info),
        ];
        let _guard = self
            .sdf_slots
            .lock()
            .expect("bindless sdf slot allocator lock");
        // SAFETY: the ash seam. The set + layout outlive this; both views are valid and the
        // `image_info` vectors live until the call returns; the array lengths equal the
        // layout's counts.
        unsafe {
            self.resources.device().update_descriptor_sets(&writes, &[]);
        }
    }

    /// Writes the per-height min/max pyramid `view` into bindless slot `index` of binding 4 (the
    /// parallel `heightMinMaxTextures` array) with the point sampler, under the bindless mutex. The
    /// slot is the height texture's own albedo slot, so the factor kernel's `heightIndex` addresses
    /// both the texture (binding 0) and its pyramid here. The image must be in
    /// `SHADER_READ_ONLY_OPTIMAL` (the upload transitions it there).
    pub fn write_height_minmax(&self, view: vk::ImageView, index: u32) {
        let image_info = [vk::DescriptorImageInfo {
            sampler: self.minmax_sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.bindless_set)
            .dst_binding(4)
            .dst_array_element(index)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&image_info);
        let _guard = self.slots.lock().expect("bindless slot allocator lock");
        // SAFETY: the ash seam. The set + layout outlive this; the view is valid for the call. The
        // lock serializes concurrent worker/main writes to the set.
        unsafe {
            self.resources
                .device()
                .update_descriptor_sets(&[write], &[]);
        }
    }

    /// Seeds a default 1×1 pyramid `view` into *every* slot of binding 4 in one
    /// `vkUpdateDescriptorSets`. Called once at init so the partially-bound array is never sampled
    /// while unbound (lavapipe faults on an unbound slot even one the shader never reads, and it is UB
    /// on real hardware). A non-displacement texture keeps this default (zero local range → no extra
    /// refinement); a displacement height map overwrites its slot with its real pyramid.
    pub fn seed_all_height_minmax(&self, view: vk::ImageView) {
        let image_info = vec![
            vk::DescriptorImageInfo {
                sampler: self.minmax_sampler,
                image_view: view,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            };
            self.texture_capacity as usize
        ];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.bindless_set)
            .dst_binding(4)
            .dst_array_element(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&image_info);
        let _guard = self.slots.lock().expect("bindless slot allocator lock");
        // SAFETY: the ash seam. The set + layout outlive this; `view` is valid and `image_info` lives
        // until the call returns; the array length equals the layout's count.
        unsafe {
            self.resources
                .device()
                .update_descriptor_sets(&[write], &[]);
        }
    }

    /// Allocates one descriptor set of `layout` from the shared descriptor pool (the
    /// `FREE_DESCRIPTOR_SET` pool sized for the per-frame light/instance sets). The
    /// per-frame instance set is allocated once and rewritten as its buffers grow.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if `vkAllocateDescriptorSets` fails (pool
    /// exhaustion).
    /// Returns transient sets to the pool (created with `FREE_DESCRIPTOR_SET`). The
    /// caller guarantees no in-flight frame still binds them (a resize idle wait).
    pub fn free_sets(&self, sets: &[vk::DescriptorSet]) {
        if sets.is_empty() {
            return;
        }
        // SAFETY: the ash seam. The pool carries FREE_DESCRIPTOR_SET; the caller waited
        // out in-flight use.
        let _ = unsafe {
            self.resources
                .device()
                .free_descriptor_sets(self.descriptor_pool, sets)
        };
    }

    pub fn allocate_set(&self, layout: vk::DescriptorSetLayout) -> Result<vk::DescriptorSet> {
        let layouts = [layout];
        let info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.descriptor_pool)
            .set_layouts(&layouts);
        // SAFETY: the ash seam. The layout outlives the call; the returned set lives
        // for the renderer's lifetime (freed when the pool is destroyed in teardown).
        let sets = checked(
            unsafe { self.resources.device().allocate_descriptor_sets(&info) },
            "allocate_descriptor_sets",
        )?;
        Ok(sets[0])
    }

    /// Writes a byte slice of a storage buffer into `binding` of `set`.
    pub fn write_storage_buffer_slice(
        &self,
        set: vk::DescriptorSet,
        binding: u32,
        buffer: vk::Buffer,
        offset: vk::DeviceSize,
        range: vk::DeviceSize,
    ) {
        let buffer_info = [vk::DescriptorBufferInfo {
            buffer,
            offset,
            range,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(binding)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&buffer_info);
        // SAFETY: the ash seam. The set and buffer outlive the call.
        unsafe {
            self.resources
                .device()
                .update_descriptor_sets(&[write], &[]);
        }
    }

    /// Writes a storage buffer into `(set, binding)` — the per-frame instance / material SSBO
    /// rebind after a grow. Host access to a descriptor set is externally synchronized, but these
    /// per-frame sets are only touched on the render thread after the frame's fence is waited, so no
    /// lock is taken here.
    pub fn write_storage_buffer(
        &self,
        set: vk::DescriptorSet,
        binding: u32,
        buffer: vk::Buffer,
        size: vk::DeviceSize,
    ) {
        let buffer_info = [vk::DescriptorBufferInfo {
            buffer,
            offset: 0,
            range: size,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(binding)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&buffer_info);
        // SAFETY: the ash seam. The set + buffer outlive the call; the write targets a
        // single binding the set's layout declares.
        unsafe {
            self.resources
                .device()
                .update_descriptor_sets(&[write], &[]);
        }
    }

    /// Writes a uniform buffer into `(set, binding)` — the per-frame light UBO + cluster
    /// params UBO binds. Host access to these per-frame sets is on the render thread only
    /// (after the frame's fence is waited), so no lock is taken (mirrors
    /// [`Descriptors::write_storage_buffer`]).
    pub fn write_uniform_buffer(
        &self,
        set: vk::DescriptorSet,
        binding: u32,
        buffer: vk::Buffer,
        size: vk::DeviceSize,
    ) {
        let buffer_info = [vk::DescriptorBufferInfo {
            buffer,
            offset: 0,
            range: size,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(binding)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .buffer_info(&buffer_info);
        // SAFETY: the ash seam. The set + buffer outlive the call; the write targets a
        // single binding the set's layout declares.
        unsafe {
            self.resources
                .device()
                .update_descriptor_sets(&[write], &[]);
        }
    }

    /// Writes a `UNIFORM_BUFFER` binding into `(set, binding)` over one `range`-byte slice at
    /// `offset` — the per-frame GPU-scene address block on the instance set. Written once at
    /// bring-up per frame set; the buffer and offsets stay stable for the renderer's lifetime.
    pub fn write_uniform_buffer_at(
        &self,
        set: vk::DescriptorSet,
        binding: u32,
        buffer: vk::Buffer,
        offset: vk::DeviceSize,
        range: vk::DeviceSize,
    ) {
        let buffer_info = [vk::DescriptorBufferInfo {
            buffer,
            offset,
            range,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(binding)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .buffer_info(&buffer_info);
        // SAFETY: the ash seam. The set + buffer outlive the call; the write targets a single
        // binding the set's layout declares.
        unsafe {
            self.resources
                .device()
                .update_descriptor_sets(&[write], &[]);
        }
    }

    /// Writes a `UNIFORM_BUFFER_DYNAMIC` binding into `(set, binding)` over one `range`-byte slice —
    /// the per-view grade UBO on the tonemap set. `range` is the aligned size of one element; the
    /// dispatch supplies the per-frame `frame * range` dynamic offset at bind time, so one set serves
    /// every frame-in-flight without a per-frame rewrite. Written once at view build (single-threaded).
    pub fn write_dynamic_uniform_buffer(
        &self,
        set: vk::DescriptorSet,
        binding: u32,
        buffer: vk::Buffer,
        range: vk::DeviceSize,
    ) {
        let buffer_info = [vk::DescriptorBufferInfo {
            buffer,
            offset: 0,
            range,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(binding)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
            .buffer_info(&buffer_info);
        // SAFETY: the ash seam. The set + buffer outlive the call; the write targets a single binding
        // the set's layout declares.
        unsafe {
            self.resources
                .device()
                .update_descriptor_sets(&[write], &[]);
        }
    }

    /// The number of bindless slots ever handed out (the high-water mark). Slot 0 (the
    /// default white) is counted, so this is `>= 1` after init.
    pub fn texture_count(&self) -> u32 {
        self.slots
            .lock()
            .expect("bindless slot allocator lock")
            .next_index
    }

    /// The number of reclaimed slots currently available for reuse.
    pub fn free_count(&self) -> u32 {
        self.free_list.lock().map(|f| f.len() as u32).unwrap_or(0)
    }
}

impl Drop for Descriptors {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The `Arc<DeviceResources>` keeps the device alive for
        // this call; the run loop idled it before teardown. The bindless
        // set frees implicitly with its pool, so only the pools/layouts/samplers are
        // destroyed here, each exactly once. Pools before layouts is not required
        // (they are independent device children).
        let raw = self.resources.device();
        unsafe {
            raw.destroy_descriptor_pool(self.bindless_pool, None);
            raw.destroy_descriptor_pool(self.descriptor_pool, None);
            raw.destroy_descriptor_set_layout(self.bindless_set_layout, None);
            raw.destroy_descriptor_set_layout(self.light_set_layout, None);
            raw.destroy_descriptor_set_layout(self.instance_set_layout, None);
            raw.destroy_descriptor_set_layout(self.ibl_set_layout, None);
            raw.destroy_descriptor_set_layout(self.ssao_mesh_set_layout, None);
            raw.destroy_descriptor_set_layout(self.ddgi_mesh_set_layout, None);
            if let Some(layout) = self.rt_mesh_set_layout {
                raw.destroy_descriptor_set_layout(layout, None);
            }
            if let Some(layout) = self.restir_mesh_set_layout {
                raw.destroy_descriptor_set_layout(layout, None);
            }
            raw.destroy_descriptor_set_layout(self.cluster_set_layout, None);
            raw.destroy_descriptor_set_layout(self.tonemap_set_layout, None);
            raw.destroy_descriptor_set_layout(self.fog_set_layout, None);
            raw.destroy_descriptor_set_layout(self.fxaa_set_layout, None);
            raw.destroy_descriptor_set_layout(self.bloom_set_layout, None);
            raw.destroy_descriptor_set_layout(self.taa_set_layout, None);
            raw.destroy_descriptor_set_layout(self.depth_upscale_set_layout, None);
            raw.destroy_sampler(self.linear_sampler, None);
            raw.destroy_sampler(self.shadow_sampler, None);
            raw.destroy_sampler(self.sdf_sampler, None);
            raw.destroy_sampler(self.minmax_sampler, None);
        }
    }
}

#[cfg(test)]
mod tests;
