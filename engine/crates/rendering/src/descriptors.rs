//! The descriptor infrastructure built once at startup: the device-global
//! descriptor-set layouts, the two descriptor pools, the single global bindless
//! combined-image-sampler set, the samplers, and the bindless slot allocator with
//! its reclaim free-list.
//!
//! The scope is the *device-global* descriptor state — layouts that never change
//! after init, the two pools, the one bindless set bound by every draw, and the
//! samplers. The per-frame light/instance/cluster *sets* and the per-view
//! post-process sets are not built here; only the layouts they allocate against live
//! here, immutable and borrowed `&`.
//!
//! # The bindless table and its slot allocator
//!
//! Set 0 is one global runtime-sized combined-image-sampler array
//! (`MAX_BINDLESS_TEXTURES` slots), partially bound + update-after-bind: a texture
//! upload writes a stable slot into the live set and the shader indexes it
//! per-instance. The default white texture takes slot 0. Slots are handed out by
//! [`Descriptors::claim_slot`]: it pops the reclaim free-list before growing the
//! high-water `next_index`, so a churny scene stays bounded. Both the free-list and
//! the `vkUpdateDescriptorSets` write into the shared set take the
//! [`crate::resources::BindlessFreeList`] / bindless mutex, because the thumbnail
//! worker can also upload off the main thread (README §5). Every [`crate::GpuTexture`]
//! holds a clone of the free-list `Arc` so its `Drop` returns its slot.

use std::sync::{Arc, Mutex};

use ash::vk;

use crate::resources::{BindlessFreeList, DeviceResources};
use crate::{Device, Result, checked};

/// Capacity of the bindless texture array (set 0). One global combined-image-sampler
/// array indexed per-instance; lavapipe and desktop GPUs allow far more, this is
/// plenty.
pub const MAX_BINDLESS_TEXTURES: u32 = 1024;

/// Capacity of the bindless per-mesh SDF array (set 0, binding 1). One global
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
/// mutex so the thumbnail worker can upload concurrently (README §5).
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

    slots: Mutex<SlotAllocator>,
    free_list: BindlessFreeList,

    /// The per-mesh SDF bindless slot allocator (binding 1 of the bindless set), with
    /// its own high-water mark + reclaim free-list, the same bounded-pool discipline as
    /// the albedo allocator above.
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
    /// `Drop` pushes to it (README §4/§5).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing Vulkan call; already-created
    /// handles are freed before returning on a partial failure.
    pub fn new(device: &Device, free_list: &BindlessFreeList) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let raw = resources.device();

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

        partial.bindless_set_layout = Some(create_bindless_layout(raw)?);
        partial.light_set_layout = Some(create_light_layout(raw, partial.shadow_sampler.unwrap())?);
        partial.instance_set_layout = Some(create_instance_layout(raw)?);
        partial.ibl_set_layout = Some(create_ibl_layout(raw)?);
        partial.ssao_mesh_set_layout = Some(create_ssao_mesh_layout(
            raw,
            partial.linear_sampler.unwrap(),
        )?);
        partial.ddgi_mesh_set_layout = Some(create_ddgi_mesh_layout(raw)?);
        // Sets 6/7 (TLAS + ReSTIR radiance) need the AS extension, so they exist only
        // when RT is supported; the mesh PSO appends them to its layout only then.
        if device.capabilities.rt_supported {
            partial.rt_mesh_set_layout = Some(create_rt_mesh_layout(raw)?);
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
        )?);
        partial.bindless_pool = Some(create_bindless_pool(raw)?);

        let bindless_set = allocate_bindless_set(
            raw,
            partial.bindless_pool.unwrap(),
            partial.bindless_set_layout.unwrap(),
        )?;

        // Slot 0 is the default white texture: claim it up front so the allocator's
        // high-water mark starts at 1 and the first uploaded texture gets slot 1.
        let mut allocator = SlotAllocator {
            next_index: 0,
            cap: MAX_BINDLESS_TEXTURES,
            free_list: Arc::clone(free_list),
        };
        let white_slot = allocator.claim().expect("default white slot");
        debug_assert_eq!(white_slot, DEFAULT_WHITE_SLOT);

        // The per-mesh SDF array's own allocator + reclaim free-list. No slot is reserved
        // up front (a mesh without a baked field simply claims none).
        let sdf_free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let sdf_allocator = SlotAllocator {
            next_index: 0,
            cap: MAX_BINDLESS_SDF,
            free_list: Arc::clone(&sdf_free_list),
        };

        tracing::info!(
            "bindless descriptor table ready ({} albedo + {} sdf slots, update-after-bind)",
            MAX_BINDLESS_TEXTURES,
            MAX_BINDLESS_SDF
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
            slots: Mutex::new(allocator),
            free_list: Arc::clone(free_list),
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

    /// Claims a stable bindless slot, reusing a reclaimed one if available, under the
    /// bindless mutex. The upload path then writes the texture into this slot with
    /// [`Descriptors::write_texture`] and constructs the [`crate::GpuTexture`] holding
    /// the free-list clone. `None` when the array is full ([`MAX_BINDLESS_TEXTURES`]
    /// slots occupied); the caller then fails the upload rather than writing out of range.
    pub fn claim_slot(&self) -> Option<u32> {
        self.slots
            .lock()
            .expect("bindless slot allocator lock")
            .claim()
    }

    /// Writes `view` into bindless slot `index` of the global set with the linear
    /// sampler, under the bindless mutex (host access to a descriptor set is
    /// externally synchronized; the worker writes it too — README §5). The image must
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
            MAX_BINDLESS_TEXTURES as usize
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

    /// Claims a stable per-mesh SDF bindless slot (binding 1), reusing a reclaimed one if
    /// available, under the SDF allocator mutex. The upload path writes the field's view
    /// into this slot with [`Descriptors::write_sdf_texture`] and constructs the
    /// [`crate::GpuSdf`] holding the free-list clone. `None` when the array is full
    /// ([`MAX_BINDLESS_SDF`] fields occupied); the caller then skips the field rather than
    /// writing an out-of-range `dstArrayElement`.
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
    /// thread can upload meshes off the main thread — README §5).
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
            MAX_BINDLESS_SDF as usize
        ];
        let indir_info = vec![
            vk::DescriptorImageInfo {
                sampler: vk::Sampler::null(),
                image_view: indirection,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            };
            MAX_BINDLESS_SDF as usize
        ];
        let coverage_info = vec![
            vk::DescriptorImageInfo {
                sampler: self.sdf_sampler,
                image_view: coverage,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            };
            MAX_BINDLESS_SDF as usize
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
            MAX_BINDLESS_TEXTURES as usize
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

    /// The number of per-mesh SDF bindless slots ever handed out (the high-water mark).
    pub fn sdf_count(&self) -> u32 {
        self.sdf_slots
            .lock()
            .expect("bindless sdf slot allocator lock")
            .next_index
    }

    /// Allocates one descriptor set of `layout` from the shared descriptor pool (the
    /// `FREE_DESCRIPTOR_SET` pool sized for the per-frame light/instance sets). The
    /// per-frame instance set is allocated once and rewritten as its buffers grow.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if `vkAllocateDescriptorSets` fails (pool
    /// exhaustion).
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

    /// Writes a storage buffer into `(set, binding)` — the per-frame instance /
    /// material SSBO rebind after a grow. Host access to a descriptor set is externally
    /// synchronized, but these per-frame sets are only touched on the render thread
    /// (after the frame's fence is waited), so no lock is taken here.
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
        // this call; the run loop idled it before teardown (README §4). The bindless
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

/// Holds the partially-built handles during [`Descriptors::new`] so a mid-init
/// failure frees what was already created (each `?` short-circuits to this `Drop`).
/// On success, the `take_*` methods move every handle out (clearing the field) so the
/// `Drop` frees nothing.
struct Partial<'a> {
    resources: &'a Arc<DeviceResources>,
    linear_sampler: Option<vk::Sampler>,
    shadow_sampler: Option<vk::Sampler>,
    sdf_sampler: Option<vk::Sampler>,
    minmax_sampler: Option<vk::Sampler>,
    bindless_set_layout: Option<vk::DescriptorSetLayout>,
    light_set_layout: Option<vk::DescriptorSetLayout>,
    instance_set_layout: Option<vk::DescriptorSetLayout>,
    ibl_set_layout: Option<vk::DescriptorSetLayout>,
    ssao_mesh_set_layout: Option<vk::DescriptorSetLayout>,
    ddgi_mesh_set_layout: Option<vk::DescriptorSetLayout>,
    rt_mesh_set_layout: Option<vk::DescriptorSetLayout>,
    restir_mesh_set_layout: Option<vk::DescriptorSetLayout>,
    cluster_set_layout: Option<vk::DescriptorSetLayout>,
    tonemap_set_layout: Option<vk::DescriptorSetLayout>,
    fog_set_layout: Option<vk::DescriptorSetLayout>,
    fxaa_set_layout: Option<vk::DescriptorSetLayout>,
    bloom_set_layout: Option<vk::DescriptorSetLayout>,
    taa_set_layout: Option<vk::DescriptorSetLayout>,
    depth_upscale_set_layout: Option<vk::DescriptorSetLayout>,
    descriptor_pool: Option<vk::DescriptorPool>,
    bindless_pool: Option<vk::DescriptorPool>,
}

/// Generates the `take_<field>` accessor (moves the handle out, leaving `None` so the
/// `Drop` skips it) for every owned handle in [`Partial`].
macro_rules! partial_take {
    ($($take:ident => $field:ident: $ty:ty),+ $(,)?) => {
        $(
            fn $take(&mut self) -> $ty {
                self.$field.take().expect("partial handle built before take")
            }
        )+
    };
}

impl<'a> Partial<'a> {
    fn new(resources: &'a Arc<DeviceResources>) -> Self {
        Self {
            resources,
            linear_sampler: None,
            shadow_sampler: None,
            sdf_sampler: None,
            minmax_sampler: None,
            bindless_set_layout: None,
            light_set_layout: None,
            instance_set_layout: None,
            ibl_set_layout: None,
            ssao_mesh_set_layout: None,
            ddgi_mesh_set_layout: None,
            rt_mesh_set_layout: None,
            restir_mesh_set_layout: None,
            cluster_set_layout: None,
            tonemap_set_layout: None,
            fog_set_layout: None,
            fxaa_set_layout: None,
            bloom_set_layout: None,
            taa_set_layout: None,
            depth_upscale_set_layout: None,
            descriptor_pool: None,
            bindless_pool: None,
        }
    }

    partial_take! {
        take_linear_sampler => linear_sampler: vk::Sampler,
        take_shadow_sampler => shadow_sampler: vk::Sampler,
        take_sdf_sampler => sdf_sampler: vk::Sampler,
        take_minmax_sampler => minmax_sampler: vk::Sampler,
        take_bindless_set_layout => bindless_set_layout: vk::DescriptorSetLayout,
        take_light_set_layout => light_set_layout: vk::DescriptorSetLayout,
        take_instance_set_layout => instance_set_layout: vk::DescriptorSetLayout,
        take_ibl_set_layout => ibl_set_layout: vk::DescriptorSetLayout,
        take_ssao_mesh_set_layout => ssao_mesh_set_layout: vk::DescriptorSetLayout,
        take_ddgi_mesh_set_layout => ddgi_mesh_set_layout: vk::DescriptorSetLayout,
        take_cluster_set_layout => cluster_set_layout: vk::DescriptorSetLayout,
        take_tonemap_set_layout => tonemap_set_layout: vk::DescriptorSetLayout,
        take_fog_set_layout => fog_set_layout: vk::DescriptorSetLayout,
        take_fxaa_set_layout => fxaa_set_layout: vk::DescriptorSetLayout,
        take_bloom_set_layout => bloom_set_layout: vk::DescriptorSetLayout,
        take_taa_set_layout => taa_set_layout: vk::DescriptorSetLayout,
        take_depth_upscale_set_layout => depth_upscale_set_layout: vk::DescriptorSetLayout,
        take_descriptor_pool => descriptor_pool: vk::DescriptorPool,
        take_bindless_pool => bindless_pool: vk::DescriptorPool,
    }
}

impl Drop for Partial<'_> {
    fn drop(&mut self) {
        // SAFETY: the ash seam. Frees only the handles still present (a successful
        // `Descriptors::new` `take`s them all out, so this frees nothing). Runs only
        // on the mid-init error path, where each present handle was created on this
        // device and not yet owned by a `Descriptors`.
        let raw = self.resources.device();
        unsafe {
            if let Some(pool) = self.bindless_pool {
                raw.destroy_descriptor_pool(pool, None);
            }
            if let Some(pool) = self.descriptor_pool {
                raw.destroy_descriptor_pool(pool, None);
            }
            for layout in [
                self.bindless_set_layout,
                self.light_set_layout,
                self.instance_set_layout,
                self.ibl_set_layout,
                self.ssao_mesh_set_layout,
                self.ddgi_mesh_set_layout,
                self.rt_mesh_set_layout,
                self.restir_mesh_set_layout,
                self.cluster_set_layout,
                self.tonemap_set_layout,
                self.fog_set_layout,
                self.fxaa_set_layout,
                self.bloom_set_layout,
                self.taa_set_layout,
                self.depth_upscale_set_layout,
            ]
            .into_iter()
            .flatten()
            {
                raw.destroy_descriptor_set_layout(layout, None);
            }
            if let Some(sampler) = self.minmax_sampler {
                raw.destroy_sampler(sampler, None);
            }
            if let Some(sampler) = self.sdf_sampler {
                raw.destroy_sampler(sampler, None);
            }
            if let Some(sampler) = self.shadow_sampler {
                raw.destroy_sampler(sampler, None);
            }
            if let Some(sampler) = self.linear_sampler {
                raw.destroy_sampler(sampler, None);
            }
        }
    }
}

/// The linear repeat sampler: linear min/mag/mip, repeat address, no LOD clamp.
fn create_linear_sampler(raw: &ash::Device, max_anisotropy: f32) -> Result<vk::Sampler> {
    let mut info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
        .address_mode_u(vk::SamplerAddressMode::REPEAT)
        .address_mode_v(vk::SamplerAddressMode::REPEAT)
        .address_mode_w(vk::SamplerAddressMode::REPEAT)
        .max_lod(vk::LOD_CLAMP_NONE);
    // Anisotropic minification: sample along the projected texel footprint so
    // high-frequency albedo/AO at grazing angles stays band-limited instead of aliasing.
    // `max_anisotropy <= 1.0` means the device lacks the feature — leave it isotropic.
    if max_anisotropy > 1.0 {
        info = info.anisotropy_enable(true).max_anisotropy(max_anisotropy);
    }
    // SAFETY: the ash seam. The create-info is valid for the call; the sampler is
    // owned and freed in `Descriptors::drop` (or the `Partial` error path).
    checked(unsafe { raw.create_sampler(&info, None) }, "createSampler")
}

/// The depth-compare PCF sampler: linear filtering across the 2×2 compare results,
/// clamp to an opaque-white (lit) border so off-map samples are unshadowed,
/// `LESS_OR_EQUAL` compare.
fn create_shadow_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .border_color(vk::BorderColor::FLOAT_OPAQUE_WHITE)
        .compare_enable(true)
        .compare_op(vk::CompareOp::LESS_OR_EQUAL);
    // SAFETY: the ash seam. As [`create_linear_sampler`].
    checked(
        unsafe { raw.create_sampler(&info, None) },
        "createSampler (shadow)",
    )
}

/// The per-height min/max pyramid sampler: **nearest** min/mag/mip and clamp-to-edge, no LOD clamp.
/// The pyramid is a conservative `(min, max)` bound the factor kernel point-samples with explicit LOD —
/// linear filtering would blend `min` into `max` and break the bound, so every axis is nearest.
fn create_minmax_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::NEAREST)
        .min_filter(vk::Filter::NEAREST)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .max_lod(vk::LOD_CLAMP_NONE);
    // SAFETY: the ash seam. As [`create_linear_sampler`].
    checked(
        unsafe { raw.create_sampler(&info, None) },
        "createSampler (minmax)",
    )
}

/// The per-mesh SDF sampler: linear filtering for the trilinear field lookup, **linear**
/// mipmap mode so a fractional `SampleLevel` LOD blends across the prefiltered mip pair
/// (quadrilinear — the cone-footprint mip-select's anti-alias depends on it), clamp-to-edge
/// so a sample past the grid reads the boundary cell (the positive shell) rather than
/// wrapping into the field, and no LOD clamp so every baked mip is reachable.
fn create_sdf_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .max_lod(vk::LOD_CLAMP_NONE);
    // SAFETY: the ash seam. As [`create_linear_sampler`].
    checked(
        unsafe { raw.create_sampler(&info, None) },
        "createSampler (sdf)",
    )
}

/// Set 0: the bindless arrays — binding 0 is the albedo combined-image-sampler array,
/// binding 1 the per-mesh SDST brick-atlas `Texture3D` array (combined image sampler, mipped),
/// binding 2 the per-mesh brick-indirection `Texture3D<uint>` array (a sampled image read by
/// integer `Load`, no sampler), binding 3 the coarse coverage `Texture3D` array (combined
/// image sampler), and binding 4 the per-height min/max pyramid array (`R32G32_SFLOAT`, sharing
/// the albedo slot space). All runtime-sized, partially bound + update-after-bind.
fn create_bindless_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(MAX_BINDLESS_TEXTURES)
            // FRAGMENT for the übershader's material sampling + COMPUTE for the `displace` pre-pass,
            // which samples the height (and vector-displacement) map from this same bindless array.
            .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(MAX_BINDLESS_SDF)
            // The per-mesh brick atlas. COMPUTE-only: the GDF composite (`gdf_composite`) and the
            // DDGI ray trace's near field (`sdf::sampleField`) are the only consumers. The
            // fragment lighting path reads the composited GDF clipmap (set 1 / 9-10), not the
            // per-mesh bricks.
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(2)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .descriptor_count(MAX_BINDLESS_SDF)
            // The brick indirection volume, read by integer texel `Load` in the same two compute
            // consumers as the atlas (the GDF composite + the DDGI trace near field).
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(3)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(MAX_BINDLESS_SDF)
            // The coarse coverage volume (one texel per brick), sampled for the empty-space
            // march leap + early-out — same two compute consumers as the atlas.
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(4)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(MAX_BINDLESS_TEXTURES)
            // The per-height min/max pyramid array (`R32G32_SFLOAT`, min in R / max in G, one mip per
            // pyramid level), sharing the albedo slot space so a height map's `heightIndex` addresses
            // both its texture (binding 0) and its pyramid (here). COMPUTE-only: the adaptive-
            // tessellation factor kernel is the sole consumer, sampling it point-filtered with explicit
            // LOD for a per-region detail-adaptive edge factor.
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
    ];
    let binding_flags = [
        vk::DescriptorBindingFlags::PARTIALLY_BOUND | vk::DescriptorBindingFlags::UPDATE_AFTER_BIND,
        vk::DescriptorBindingFlags::PARTIALLY_BOUND | vk::DescriptorBindingFlags::UPDATE_AFTER_BIND,
        vk::DescriptorBindingFlags::PARTIALLY_BOUND | vk::DescriptorBindingFlags::UPDATE_AFTER_BIND,
        vk::DescriptorBindingFlags::PARTIALLY_BOUND | vk::DescriptorBindingFlags::UPDATE_AFTER_BIND,
        vk::DescriptorBindingFlags::PARTIALLY_BOUND | vk::DescriptorBindingFlags::UPDATE_AFTER_BIND,
    ];
    let mut flags_info =
        vk::DescriptorSetLayoutBindingFlagsCreateInfo::default().binding_flags(&binding_flags);
    let info = vk::DescriptorSetLayoutCreateInfo::default()
        .flags(vk::DescriptorSetLayoutCreateFlags::UPDATE_AFTER_BIND_POOL)
        .bindings(&bindings)
        .push_next(&mut flags_info);
    // SAFETY: the ash seam. The binding + flags structs outlive the call; the layout
    // is owned and freed in teardown.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "bindlessSetLayout",
    )
}

/// Set 1: directional + punctual light UBO/SSBO, cluster lists + params, and the
/// directional/spot/point shadow samplers — all fragment-stage.
fn create_light_layout(
    raw: &ash::Device,
    shadow_sampler: vk::Sampler,
) -> Result<vk::DescriptorSetLayout> {
    let uniform = vk::DescriptorType::UNIFORM_BUFFER;
    let storage = vk::DescriptorType::STORAGE_BUFFER;
    let sampler = vk::DescriptorType::COMBINED_IMAGE_SAMPLER;
    let immutable_shadow = [shadow_sampler];
    let shadow_binding = |slot| {
        vk::DescriptorSetLayoutBinding::default()
            .binding(slot)
            .descriptor_type(sampler)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::COMPUTE)
            .immutable_samplers(&immutable_shadow)
    };
    let bindings = [
        light_binding(0, uniform), // directional + ambient + counts UBO
        light_binding(1, storage), // punctual light storage buffer
        light_binding(2, storage), // per-cluster light lists (read)
        light_binding(3, uniform), // cluster params UBO
        shadow_binding(4),         // directional shadow map (immutable compare sampler)
        shadow_binding(5),         // spot shadow map (immutable compare sampler)
        light_binding(6, sampler), // point shadow STATIC distance cube (linear sampler)
        light_binding(7, sampler), // point shadow DYNAMIC distance cube (linear sampler)
        // per-mesh SDF-occluder instance list (the near-field sphere-march): COMPUTE-only, read
        // solely by the DDGI ray trace (`sdf::sampleField`). The fragment reflection occlusion taps
        // the composited GDF clipmap (bindings 9/10), not the per-mesh instance list.
        vk::DescriptorSetLayoutBinding::default()
            .binding(8)
            .descriptor_type(storage)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        // Global-SDF cascade clipmap (binding 9: a `GDF_CASCADES` combined-sampler array, the
        // far-field distance tap) + its params UBO (binding 10). FRAGMENT for the übershader's GDF
        // reflection occlusion, COMPUTE for the DDGI ray trace's far field.
        vk::DescriptorSetLayoutBinding::default()
            .binding(9)
            .descriptor_type(sampler)
            .descriptor_count(crate::GDF_CASCADES)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(10)
            .descriptor_type(uniform)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::COMPUTE),
        // Froxel volumetric-fog integration volume (binding 11): the integrated `(inScatter,
        // transmittance)` grid, sampled trilinearly by the forward transparent path so translucent
        // surfaces receive the same volumetric fog the composite applies to opaque geometry. FRAGMENT
        // only — the fog-inject compute pass reuses this layout for its light set but never reads it.
        vk::DescriptorSetLayoutBinding::default()
            .binding(11)
            .descriptor_type(sampler)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        light_binding(12, sampler), // cascaded cloud-shadow map
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "lightSetLayout",
    )
}

/// One binding of `kind` at `slot`, count 1 — the light set's shape. FRAGMENT for the mesh forward
/// shade, plus COMPUTE so the froxel fog-inject pass can bind the same per-frame light set (globals,
/// lights, clusters, cluster params, and the four shadow maps) into its compute pipeline.
fn light_binding(slot: u32, kind: vk::DescriptorType) -> vk::DescriptorSetLayoutBinding<'static> {
    vk::DescriptorSetLayoutBinding::default()
        .binding(slot)
        .descriptor_type(kind)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::COMPUTE)
}

/// Set 2: per-instance array (vertex) + joint palette (vertex) + per-material params
/// (fragment), all storage buffers.
fn create_instance_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let storage = vk::DescriptorType::STORAGE_BUFFER;
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(storage)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(storage)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX),
        vk::DescriptorSetLayoutBinding::default()
            .binding(2)
            .descriptor_type(storage)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "instanceSetLayout",
    )
}

/// Set 3 (mesh pipeline): the IBL set. Binding 0 is the global sky-radiance SH buffer;
/// bindings 1-2 are the prefiltered environment and BRDF combined-image-samplers; bindings 3-4 carry the
/// reflection-probe cube arrays (`MAX_REFLECTION_PROBES` each); binding 5 is the
/// probe-metadata SSBO — all fragment-stage. Probes ride the always-present IBL set
/// rather than a 9th bound set.
fn create_ibl_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let sampler = vk::DescriptorType::COMBINED_IMAGE_SAMPLER;
    let bindings = [
        light_binding(0, vk::DescriptorType::STORAGE_BUFFER),
        light_binding(1, sampler),
        light_binding(2, sampler),
        vk::DescriptorSetLayoutBinding::default()
            .binding(3)
            .descriptor_type(sampler)
            .descriptor_count(MAX_REFLECTION_PROBES)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        vk::DescriptorSetLayoutBinding::default()
            .binding(4)
            .descriptor_type(sampler)
            .descriptor_count(MAX_REFLECTION_PROBES)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        light_binding(5, vk::DescriptorType::STORAGE_BUFFER),
    ];
    let binding_flags = [
        vk::DescriptorBindingFlags::empty(),
        vk::DescriptorBindingFlags::empty(),
        vk::DescriptorBindingFlags::empty(),
        vk::DescriptorBindingFlags::UPDATE_AFTER_BIND,
        vk::DescriptorBindingFlags::UPDATE_AFTER_BIND,
        vk::DescriptorBindingFlags::empty(),
    ];
    let mut flags_info =
        vk::DescriptorSetLayoutBindingFlagsCreateInfo::default().binding_flags(&binding_flags);
    let info = vk::DescriptorSetLayoutCreateInfo::default()
        .flags(vk::DescriptorSetLayoutCreateFlags::UPDATE_AFTER_BIND_POOL)
        .bindings(&bindings)
        .push_next(&mut flags_info);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "iblSetLayout",
    )
}

/// Set 4 (mesh pipeline): eight screen-space sampled images behind one immutable linear sampler.
/// Sharing the sampler keeps the complete mesh interface within portability devices' per-stage
/// sampler limit while preserving independent image bindings.
fn create_ssao_mesh_layout(
    raw: &ash::Device,
    linear_sampler: vk::Sampler,
) -> Result<vk::DescriptorSetLayout> {
    let sampled_image = vk::DescriptorType::SAMPLED_IMAGE;
    let immutable_linear = [linear_sampler];
    let bindings = [
        light_binding(0, sampled_image),
        light_binding(1, sampled_image),
        light_binding(2, sampled_image),
        light_binding(3, sampled_image),
        light_binding(4, sampled_image),
        light_binding(5, sampled_image),
        light_binding(6, sampled_image),
        light_binding(7, sampled_image), // gi_indirect: the half-res screen-space indirect-diffuse resolve
        vk::DescriptorSetLayoutBinding::default()
            .binding(8)
            .descriptor_type(vk::DescriptorType::SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            .immutable_samplers(&immutable_linear),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "ssaoMeshSetLayout",
    )
}

/// Set 5 (mesh pipeline): the DDGI irradiance + distance sampler set — two fragment-stage
/// combined-image-samplers.
fn create_ddgi_mesh_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let sampler = vk::DescriptorType::COMBINED_IMAGE_SAMPLER;
    let bindings = [light_binding(0, sampler), light_binding(1, sampler)];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "ddgiMeshLayout",
    )
}

/// Set 6 (mesh pipeline, RT only): the TLAS — one fragment-stage acceleration
/// structure.
fn create_rt_mesh_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [light_binding(
        0,
        vk::DescriptorType::ACCELERATION_STRUCTURE_KHR,
    )];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "rtMeshLayout",
    )
}

/// Set 7 (mesh pipeline, RT only): the ReSTIR radiance sampler — one fragment-stage
/// combined-image-sampler.
fn create_restir_mesh_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [light_binding(0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER)];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "restirMeshLayout",
    )
}

/// The clustered-light-culling compute set: params UBO (0) + light list read (1) +
/// cluster lists write (2), all compute-stage.
fn create_cluster_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        compute_binding(0, vk::DescriptorType::UNIFORM_BUFFER),
        compute_binding(1, vk::DescriptorType::STORAGE_BUFFER),
        compute_binding(2, vk::DescriptorType::STORAGE_BUFFER),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "clusterSetLayout",
    )
}

/// The tonemap compute set: the offscreen color as a storage image in GENERAL (0), the per-view grade
/// uniform as a dynamic-offset UBO (1) whose per-frame slice the dispatch selects, and the creative
/// look 3D LUT (2) sampled tetrahedrally after the view transform (an always-bound identity ramp when
/// no look is assigned, so the shader never branches on presence).
fn create_tonemap_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        compute_binding(0, vk::DescriptorType::STORAGE_IMAGE),
        compute_binding(1, vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC),
        compute_binding(2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "tonemapSetLayout",
    )
}

/// The height-fog compute set: the offscreen storage image (0), the fog params UBO (1, a
/// dynamic-offset slice), the scene depth (2), and the sky-view LUT (3) — both combined image
/// samplers.
fn create_fog_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        compute_binding(0, vk::DescriptorType::STORAGE_IMAGE),
        compute_binding(1, vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC),
        compute_binding(2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(3, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        // The integrated froxel volume, sampled trilinearly in `fog.mode == volumetric`. Bound to the
        // fog module's fixed-size integration volume, so this descriptor is always valid (the shader
        // statically references it even in analytic mode, where the branch never samples it).
        compute_binding(4, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        // The aerial-perspective volume (Hillaire 2020), sampled trilinearly when the atmosphere is
        // live + AP is authored. Bound to the fixed-size AP volume, always a valid descriptor (the
        // shader gates the sample on `aerial.x`, so it is untouched when AP is off).
        compute_binding(5, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(6, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(7, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(8, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(9, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "fogSetLayout",
    )
}

/// The FXAA compute set: a sampler source (0) + a storage-image target (1).
fn create_fxaa_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        compute_binding(0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(1, vk::DescriptorType::STORAGE_IMAGE),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "fxaaSetLayout",
    )
}

/// The bloom compute set: a linear-sampled source (0) + a storage-image target (1), plus the
/// lens-dirt mask (2) and the anamorphic streak buffer (3) the composite pass samples. Every
/// pyramid pass — downsample, tent upsample, streak, composite — binds one such set; the
/// non-composite passes point 2/3 at a harmless fallback view (the mask/streak are only sampled in
/// the composite branch), so a single layout serves the whole chain.
fn create_bloom_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        compute_binding(0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(1, vk::DescriptorType::STORAGE_IMAGE),
        compute_binding(2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(3, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "bloomSetLayout",
    )
}

/// The TAA resolve compute set: current/history/motion samplers (0–2), offscreen/history
/// storage images (3–4), and the motion-prepass depth (5) for closest-depth velocity dilation.
fn create_taa_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        compute_binding(0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(1, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(3, vk::DescriptorType::STORAGE_IMAGE),
        compute_binding(4, vk::DescriptorType::STORAGE_IMAGE),
        compute_binding(5, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        // Phase 3 reconstruction robustness: 6 = reactive coverage (input R8), 7 = the previous
        // lock image (display, sampled at the reprojected UV), 8 = this frame's lock image
        // (display, written).
        compute_binding(6, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(7, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(8, vk::DescriptorType::STORAGE_IMAGE),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "taaSetLayout",
    )
}

/// One compute-stage binding of `kind` at `slot`, count 1 — the post-process set
/// shape.
/// The depth-upscale graphics set: one fragment sampler (the input-extent scene depth), sampled
/// per display pixel to fill the display-extent overlay depth.
fn create_depth_upscale_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "depthUpscaleSetLayout",
    )
}

fn compute_binding(slot: u32, kind: vk::DescriptorType) -> vk::DescriptorSetLayoutBinding<'static> {
    vk::DescriptorSetLayoutBinding::default()
        .binding(slot)
        .descriptor_type(kind)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::COMPUTE)
}

/// The general descriptor pool the per-frame + per-view sets allocate against
/// (`FREE_DESCRIPTOR_SET` so freed sets return capacity). Sized for headroom: the
/// bindless count, the per-frame light/instance UBOs/SSBOs, and the per-view
/// post-process storage images.
fn create_descriptor_pool(raw: &ash::Device, rt_supported: bool) -> Result<vk::DescriptorPool> {
    let frames = crate::frame::MAX_FRAMES_IN_FLIGHT as u32;
    let views = VIEW_COUNT;
    // Bloom binds one set per pyramid pass (up to `BLOOM_PASSES_PER_FRAME` per view), and its mip
    // images come from the per-frame-in-flight transient pool, so the sets are allocated per frame
    // slot too — `BLOOM_PASSES_PER_FRAME * frames * views` sets. Each set has three combined-image
    // samplers (source + dirt mask + streak) and one storage image (target).
    let bloom_sets = BLOOM_PASSES_PER_FRAME as u32 * frames * views;
    let mut pool_sizes = vec![
        pool_size(
            // +views for the creative-look 3D LUT (binding 2 of each per-view tonemap set), +1 for the
            // transient look-bake set (a tonemap-layout set allocated + freed per `bake-look`), +3*views
            // for the per-view fog set's depth + sky-view-LUT + froxel-integration samplers (2 + 3 + 4),
            // +frames for the froxel-integration sampler (binding 11) on each frame's light set.
            vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            1024 + 3 * bloom_sets + views + 1 + 3 * views + frames,
        ),
        // +frames for the GDF cascade-params UBO (binding 10) on each frame's light set.
        pool_size(vk::DescriptorType::UNIFORM_BUFFER, 5 * frames + 8),
        // One dynamic-offset grade UBO per view (binding 1 of the tonemap set), +1 for the transient
        // look-bake set, +views for the per-view fog params UBO (binding 1 of the fog set).
        pool_size(
            vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC,
            views + 1 + views,
        ),
        // +8 for the device-shared GDF cull + composite sets (two storage buffers each).
        pool_size(
            vk::DescriptorType::STORAGE_BUFFER,
            8 * frames + 24 + 8 * views,
        ),
        // +(GDF_CASCADES + 1) per frame for each composite set's cascade storage-image array + the
        // lite albedo cache. The per-view budget (29) covers the DFAO and specular-occlusion chains'
        // storage images (each: trace out + blur out + two accum sets = 6, so 12 total) on top of
        // the SSGI/AA sets, plus the TAA lock-write storage image (binding 8).
        pool_size(
            // +1 for the transient look-bake set's storage output image (binding 0), +views for the
            // per-view fog set's offscreen storage image (binding 0).
            vk::DescriptorType::STORAGE_IMAGE,
            48 + 29 * views + bloom_sets + (crate::GDF_CASCADES + 1) * frames + 1 + views,
        ),
        // The mesh screen-space set carries eight sampled images behind one immutable sampler.
        pool_size(vk::DescriptorType::SAMPLED_IMAGE, 8 * views),
        // One immutable sampler descriptor per mesh screen-space set.
        pool_size(vk::DescriptorType::SAMPLER, views),
    ];
    if rt_supported {
        pool_sizes.push(pool_size(
            vk::DescriptorType::ACCELERATION_STRUCTURE_KHR,
            frames + 2 + views,
        ));
    }
    let info = vk::DescriptorPoolCreateInfo::default()
        .flags(
            vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET
                | vk::DescriptorPoolCreateFlags::UPDATE_AFTER_BIND,
        )
        .max_sets(1024 + 8 * frames + 64 + 21 * views + bloom_sets + 1 + views)
        .pool_sizes(&pool_sizes);
    // SAFETY: the ash seam. The pool is owned and freed in teardown.
    checked(
        unsafe { raw.create_descriptor_pool(&info, None) },
        "descriptorPool",
    )
}

/// The bindless set's own pool: `UPDATE_AFTER_BIND`, one set, sized for the full
/// bindless array.
fn create_bindless_pool(raw: &ash::Device) -> Result<vk::DescriptorPool> {
    let pool_sizes = [
        pool_size(
            vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            // Albedo (binding 0) + brick atlas (binding 1) + coverage (binding 3) + the per-height
            // min/max pyramid (binding 4, another `MAX_BINDLESS_TEXTURES` slots).
            2 * MAX_BINDLESS_TEXTURES + 2 * MAX_BINDLESS_SDF,
        ),
        // The brick-indirection array is a separate sampled-image (no sampler) binding.
        pool_size(vk::DescriptorType::SAMPLED_IMAGE, MAX_BINDLESS_SDF),
    ];
    let info = vk::DescriptorPoolCreateInfo::default()
        .flags(vk::DescriptorPoolCreateFlags::UPDATE_AFTER_BIND)
        .max_sets(1)
        .pool_sizes(&pool_sizes);
    // SAFETY: the ash seam. The pool is owned and freed in teardown.
    checked(
        unsafe { raw.create_descriptor_pool(&info, None) },
        "bindlessPool",
    )
}

/// A pool size of `count` descriptors of `kind`.
fn pool_size(kind: vk::DescriptorType, count: u32) -> vk::DescriptorPoolSize {
    vk::DescriptorPoolSize {
        ty: kind,
        descriptor_count: count,
    }
}

/// Allocates the single bindless set from `pool` against `layout`.
fn allocate_bindless_set(
    raw: &ash::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
) -> Result<vk::DescriptorSet> {
    let layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    // SAFETY: the ash seam. One set is allocated from the pool above against the
    // bindless layout; the returned set is freed implicitly when the pool drops.
    let sets = checked(
        unsafe { raw.allocate_descriptor_sets(&info) },
        "allocate bindlessSet",
    )?;
    Ok(sets[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::SurfaceSource;
    use crate::validation_issue_count;

    /// Builds a headless device or skips the test (no Vulkan ICD in this toolbox).
    fn device_or_skip() -> Option<Device> {
        match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => Some(device),
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                None
            }
        }
    }

    /// The slot allocator alone (no GPU): claiming N slots after slot 0 hands out
    /// 1..=N; dropping those into the free-list then claiming N more reuses every
    /// reclaimed slot LIFO and never grows the high-water mark past N+1 — the
    /// bounded-pool invariant. This is the phase's named slot-allocator test, run on
    /// any host (the allocator is GPU-free logic).
    #[test]
    fn slot_allocator_reuses_freed_slots_and_stays_bounded() {
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let mut allocator = SlotAllocator {
            next_index: 0,
            cap: MAX_BINDLESS_TEXTURES,
            free_list: Arc::clone(&free_list),
        };

        // Slot 0 is the default white, claimed first.
        assert_eq!(allocator.claim(), Some(DEFAULT_WHITE_SLOT));

        // Claim five more: the high-water mark grows 1..=5.
        let claimed: Vec<u32> = (0..5).map(|_| allocator.claim().unwrap()).collect();
        assert_eq!(claimed, vec![1, 2, 3, 4, 5]);
        assert_eq!(allocator.next_index, 6);

        // Return them to the free-list (a GpuTexture drop pushes its slot). The order
        // mimics texture drops: push 1..=5.
        {
            let mut free = free_list.lock().unwrap();
            free.extend_from_slice(&claimed);
        }
        assert_eq!(free_list.lock().unwrap().len(), 5);

        // Claim five more: every slot is reused (LIFO, so 5,4,3,2,1) and the
        // high-water mark does NOT grow past 6.
        let reclaimed: Vec<u32> = (0..5).map(|_| allocator.claim().unwrap()).collect();
        assert_eq!(reclaimed, vec![5, 4, 3, 2, 1]);
        assert_eq!(
            allocator.next_index, 6,
            "the free-list reuse kept next_index bounded — no growth past the prior high-water mark"
        );
        assert!(free_list.lock().unwrap().is_empty());

        // The next claim with an empty free-list grows the high-water mark again.
        assert_eq!(allocator.claim(), Some(6));
        assert_eq!(allocator.next_index, 7);
    }

    /// A bounded allocator hands out every slot up to its capacity, then returns `None`
    /// (never an out-of-range index) — the fix for the SDF-array overflow that wrote past
    /// `dstArrayElement`. Freeing a slot lets the next claim reuse it below the cap.
    #[test]
    fn slot_allocator_returns_none_when_full() {
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let mut allocator = SlotAllocator {
            next_index: 0,
            cap: 3,
            free_list: Arc::clone(&free_list),
        };

        assert_eq!(allocator.claim(), Some(0));
        assert_eq!(allocator.claim(), Some(1));
        assert_eq!(allocator.claim(), Some(2));
        // The array is full: every further claim is refused, never an out-of-range slot.
        assert_eq!(allocator.claim(), None);
        assert_eq!(allocator.claim(), None);
        assert_eq!(allocator.next_index, 3);

        // Reclaiming a slot lets the next claim reuse it (still within the cap).
        free_list.lock().unwrap().push(1);
        assert_eq!(allocator.claim(), Some(1));
        assert_eq!(allocator.claim(), None);
    }

    /// Two threads claiming slots concurrently never alias a slot — every handed-out
    /// index across both threads is distinct. Proves the bindless `Mutex` discipline
    /// holds (README §5: the thumbnail worker and the main thread both claim). Run on
    /// any host (GPU-free). The phase's named concurrency test.
    #[test]
    fn concurrent_claims_never_alias_a_slot() {
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        const PER_THREAD: usize = 2000;
        let allocator = Arc::new(Mutex::new(SlotAllocator {
            next_index: 0,
            cap: (2 * PER_THREAD) as u32,
            free_list: Arc::clone(&free_list),
        }));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let allocator = Arc::clone(&allocator);
            handles.push(std::thread::spawn(move || {
                let mut claimed = Vec::with_capacity(PER_THREAD);
                for _ in 0..PER_THREAD {
                    claimed.push(allocator.lock().unwrap().claim().unwrap());
                }
                claimed
            }));
        }

        let mut all: Vec<u32> = Vec::new();
        for handle in handles {
            all.extend(handle.join().expect("worker thread joins"));
        }

        // 4000 claims with no reuse: the high-water mark is exactly 4000, and every
        // index 0..4000 was handed out exactly once (no alias, no gap).
        assert_eq!(all.len(), 2 * PER_THREAD);
        assert_eq!(
            allocator.lock().unwrap().next_index,
            (2 * PER_THREAD) as u32
        );
        all.sort_unstable();
        let expected: Vec<u32> = (0..(2 * PER_THREAD) as u32).collect();
        assert_eq!(
            all, expected,
            "concurrent claims handed out every slot exactly once — no aliasing"
        );
    }

    /// The full descriptor infrastructure builds against a device with the bindless
    /// set created update-after-bind, slot 0 reserved for the default white, and a
    /// validation-clean construct + teardown. Skips when no Vulkan device is present.
    /// This is the phase's GPU-side acceptance: the descriptor wiring is real-GPU
    /// valid (the update-after-bind flag, the partially-bound binding) and the Drop
    /// frees every handle with no validation message.
    #[test]
    fn descriptors_build_and_teardown_is_validation_clean() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();

        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");

        // Slot 0 is the default white: the high-water mark starts at 1, the free-list
        // is empty, and every layout/sampler/set handle is non-null.
        assert_eq!(
            descriptors.texture_count(),
            1,
            "slot 0 (default white) is claimed at init"
        );
        assert_eq!(descriptors.free_count(), 0);
        assert_ne!(descriptors.bindless_set(), vk::DescriptorSet::null());
        assert_ne!(
            descriptors.bindless_set_layout(),
            vk::DescriptorSetLayout::null()
        );
        assert_ne!(descriptors.linear_sampler(), vk::Sampler::null());
        assert_ne!(descriptors.shadow_sampler(), vk::Sampler::null());

        // A claim after init hands out slot 1 (the first uploadable slot), and the
        // free-list reclaim path is wired through `free_list`.
        assert_eq!(descriptors.claim_slot(), Some(1));
        assert_eq!(descriptors.texture_count(), 2);

        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the descriptor infrastructure's construct + teardown must be \
             validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }
}
