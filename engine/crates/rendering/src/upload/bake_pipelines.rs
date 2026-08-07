use std::path::Path;
use std::sync::Arc;

use ash::vk;

use crate::resources::{DeviceResources, Image3D};
use crate::{Error, Result, checked};

/// The push-constant block the three SDF bake shaders share (64 bytes, std430). The
/// `misc` lane carries the per-pass selector (jump step / parity / init-vs-prop / final
/// parity); see each shader's `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct BakePush {
    /// `xyz` fine voxel dims, `w` triangle count.
    pub(super) dims: [u32; 4],
    /// `xyz` grid lower corner (local), `w` the `R16_SNORM` encode clamp (`max_dist`).
    pub(super) bounds_min: [f32; 4],
    /// `xyz` per-axis cell size, `w` the winding sign (orients the distance sign).
    pub(super) cell: [f32; 4],
    /// Per-pass selector (`x` step / final parity, `y` parity, `z` mode).
    pub(super) misc: [i32; 4],
}

const _: () = assert!(size_of::<BakePush>() == 64, "BakePush must be 64 bytes");

/// The transient seed / work 3D images one SDF bake dispatches over: the seed-key scatter
/// target and the two jump-flood ping-pong seed buffers. All live only for the bake;
/// dropping frees them.
pub(super) struct BakeImages {
    pub(super) seed_key: Image3D,
    pub(super) seed_a: Image3D,
    pub(super) seed_b: Image3D,
}

/// The two GPU jump-flood bake compute pipelines (voxelize → JFA) + the shared
/// descriptor-set layout, pipeline layout, and a small descriptor pool. Owned by an
/// [`Uploader`] (built in [`Uploader::new`]) so the bake runs on the upload path —
/// including the thumbnail worker's own uploader — not the renderer's frame pipeline cache.
/// The sign pass is on the host (see [`sign_field`]).
pub(super) struct BakePipelines {
    resources: Arc<DeviceResources>,
    pub(super) set_layout: vk::DescriptorSetLayout,
    pub(super) pipeline_layout: vk::PipelineLayout,
    pub(super) pool: vk::DescriptorPool,
    pub(super) voxelize: vk::Pipeline,
    pub(super) jfa: vk::Pipeline,
}

impl BakePipelines {
    /// Builds the bake descriptor layout (2 storage buffers + 3 storage images), the
    /// pipeline layout (the 64-byte [`BakePush`]), a small `FREE_DESCRIPTOR_SET` pool, and
    /// the two compute pipelines from the runtime shader dir.
    pub(super) fn new(resources: &Arc<DeviceResources>) -> Result<Self> {
        let raw = resources.device();
        let buffer = |b: u32| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        };
        let image = |b: u32| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        };
        let bindings = [buffer(0), buffer(1), image(2), image(3), image(4)];
        let layout_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        // SAFETY: the ash seam. The bindings outlive the call; the layout is freed in `Drop`.
        let set_layout = checked(
            unsafe { raw.create_descriptor_set_layout(&layout_info, None) },
            "sdf bake set layout",
        )?;

        let set_layouts = [set_layout];
        let push = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .offset(0)
            .size(size_of::<BakePush>() as u32)];
        let pl_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push);
        // SAFETY: the ash seam. The layout owns the set layout reference for the call.
        let pipeline_layout = match checked(
            unsafe { raw.create_pipeline_layout(&pl_info, None) },
            "sdf bake pipeline layout",
        ) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. The set layout was created above; free it once.
                unsafe { raw.destroy_descriptor_set_layout(set_layout, None) };
                return Err(err);
            }
        };

        let pool_sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_BUFFER,
                descriptor_count: 16,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_IMAGE,
                descriptor_count: 32,
            },
        ];
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
            .max_sets(8)
            .pool_sizes(&pool_sizes);
        // SAFETY: the ash seam. The pool is freed in `Drop`.
        let pool = match checked(
            unsafe { raw.create_descriptor_pool(&pool_info, None) },
            "sdf bake pool",
        ) {
            Ok(pool) => pool,
            Err(err) => {
                // SAFETY: the ash seam. The layouts created above are freed once.
                unsafe {
                    raw.destroy_pipeline_layout(pipeline_layout, None);
                    raw.destroy_descriptor_set_layout(set_layout, None);
                }
                return Err(err);
            }
        };

        let build = (|| -> Result<(vk::Pipeline, vk::Pipeline)> {
            let dir = crate::pipelines::resolve_shader_dir();
            let voxelize = compute_pipeline(raw, &dir, "sdf_voxelize.spv", pipeline_layout)?;
            let jfa = match compute_pipeline(raw, &dir, "sdf_jfa.spv", pipeline_layout) {
                Ok(p) => p,
                Err(err) => {
                    // SAFETY: the ash seam. Free the voxelize pipeline before the error.
                    unsafe { raw.destroy_pipeline(voxelize, None) };
                    return Err(err);
                }
            };
            Ok((voxelize, jfa))
        })();
        let (voxelize, jfa) = match build {
            Ok(pipelines) => pipelines,
            Err(err) => {
                // SAFETY: the ash seam. Free the pool + layouts on a pipeline-build failure.
                unsafe {
                    raw.destroy_descriptor_pool(pool, None);
                    raw.destroy_pipeline_layout(pipeline_layout, None);
                    raw.destroy_descriptor_set_layout(set_layout, None);
                }
                return Err(err);
            }
        };

        Ok(Self {
            resources: Arc::clone(resources),
            set_layout,
            pipeline_layout,
            pool,
            voxelize,
            jfa,
        })
    }

    /// Allocates one bake descriptor set from the pool (freed by the caller after the bake).
    pub(super) fn allocate_set(&self, raw: &ash::Device) -> Result<vk::DescriptorSet> {
        let layouts = [self.set_layout];
        let info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.pool)
            .set_layouts(&layouts);
        // SAFETY: the ash seam. The layout outlives the call; the set is freed after the bake.
        let sets = checked(
            unsafe { raw.allocate_descriptor_sets(&info) },
            "allocate sdf bake set",
        )?;
        Ok(sets[0])
    }

    /// Writes the geometry buffers + the three transient images into the bake set.
    pub(super) fn write_set(
        &self,
        raw: &ash::Device,
        set: vk::DescriptorSet,
        positions: vk::Buffer,
        indices: vk::Buffer,
        images: &BakeImages,
    ) {
        let buf = |b: vk::Buffer| {
            [vk::DescriptorBufferInfo {
                buffer: b,
                offset: 0,
                range: vk::WHOLE_SIZE,
            }]
        };
        let img = |v: vk::ImageView| {
            [vk::DescriptorImageInfo {
                sampler: vk::Sampler::null(),
                image_view: v,
                image_layout: vk::ImageLayout::GENERAL,
            }]
        };
        let pos = buf(positions);
        let idx = buf(indices);
        let key = img(images.seed_key.view());
        let sa = img(images.seed_a.view());
        let sb = img(images.seed_b.view());
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&pos),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&idx),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&key),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&sa),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(4)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&sb),
        ];
        // SAFETY: the ash seam. The set + all referenced resources outlive the call.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };
    }
}

impl Drop for BakePipelines {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The bundle keeps the device alive; each handle is freed once.
        unsafe {
            let raw = self.resources.device();
            raw.destroy_pipeline(self.voxelize, None);
            raw.destroy_pipeline(self.jfa, None);
            raw.destroy_pipeline_layout(self.pipeline_layout, None);
            raw.destroy_descriptor_pool(self.pool, None);
            raw.destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}

/// Loads `<dir>/<file>` SPIR-V and builds a compute pipeline (entry `computeMain`) against
/// `layout`. The shader module is freed after pipeline creation.
pub(super) fn compute_pipeline(
    raw: &ash::Device,
    dir: &Path,
    file: &str,
    layout: vk::PipelineLayout,
) -> Result<vk::Pipeline> {
    let path = dir.join(file);
    let bytes = std::fs::read(&path)
        .map_err(|err| Error::ShaderLoad(format!("cannot read '{}': {err}", path.display())))?;
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return Err(Error::ShaderLoad(format!(
            "invalid SPIR-V size for '{}' ({} bytes)",
            path.display(),
            bytes.len()
        )));
    }
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let module_info = vk::ShaderModuleCreateInfo::default().code(&words);
    // SAFETY: the ash seam. The code slice outlives the call; the module is freed below.
    let module = checked(
        unsafe { raw.create_shader_module(&module_info, None) },
        "create_shader_module (sdf bake)",
    )?;
    let stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(module)
        .name(c"computeMain");
    let info = [vk::ComputePipelineCreateInfo::default()
        .stage(stage)
        .layout(layout)];
    // SAFETY: the ash seam. The create-info outlives the call.
    let created = unsafe { raw.create_compute_pipelines(vk::PipelineCache::null(), &info, None) };
    // SAFETY: the ash seam. The module is consumed by creation; free it now.
    unsafe { raw.destroy_shader_module(module, None) };
    match created {
        Ok(pipelines) => Ok(pipelines[0]),
        Err((_, result)) => Err(Error::Vk {
            context: "create_compute_pipelines (sdf bake)",
            result,
        }),
    }
}
