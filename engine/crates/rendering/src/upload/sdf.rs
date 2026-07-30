use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::Vec3;
use saffron_geometry::{GridDesc, Sdf};
use vk_mem::Alloc;

use super::Uploader;
use super::barriers::transition_image;
use super::staging::StagingBuffer;
use crate::descriptors::Descriptors;
use crate::resources::{GpuSdf, GpuSdfParts};
use crate::{Error, Result, checked};

impl Uploader {
    /// Uploads a sparse SDST v3 [`Sdf`] as three device-local `Texture3D`s — the mipped
    /// `R16_SNORM` brick atlas (`mip_count` prefiltered levels), the `R32_UINT` brick
    /// indirection volume, and the `R16_SNORM` coarse coverage volume — claims one slot in the
    /// bindless SDF arrays of `descriptors`, writes all three views at that slot (bindings
    /// 1 + 2 + 3), and wraps them as a [`GpuSdf`] owning the slot + the v3 brick metadata.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ZeroSizedImage`] for an empty field or [`Error::Vk`] for a failing
    /// Vulkan/VMA call; allocated resources are freed before return on error.
    pub fn upload_sdf(&self, descriptors: &Descriptors, sdf: &Sdf) -> Result<Arc<GpuSdf>> {
        let h = sdf.header;
        if h.dims.contains(&0) || h.indirection_dims.contains(&0) || h.coverage_dims.contains(&0) {
            return Err(Error::ZeroSizedImage);
        }
        // The brick atlas carries `mip_count` prefiltered levels (finest first): each level's
        // band-limited distance feeds the cone-footprint mip-select. Build the per-level data +
        // extents the upload copies into the mipped image's subresources.
        let mip_count = h.mip_count.max(1);
        let mip_data: Vec<Vec<i16>> = (0..mip_count)
            .map(|m| sdf.atlas_image_data_mip(m))
            .collect();
        let atlas_mips: Vec<(vk::Extent3D, &[u8])> = (0..mip_count)
            .map(|m| {
                let [w, hh, d] = sdf.atlas_image_dims_mip(m);
                (
                    vk::Extent3D {
                        width: w,
                        height: hh,
                        depth: d,
                    },
                    bytemuck::cast_slice(&mip_data[m as usize]),
                )
            })
            .collect();
        let (atlas_image, atlas_view, atlas_alloc) =
            self.create_and_upload_sdf_image(vk::Format::R16_SNORM, &atlas_mips)?;
        drop(atlas_mips);

        let [ix, iy, iz] = h.indirection_dims;
        let indir = match self.create_and_upload_sdf_image(
            vk::Format::R32_UINT,
            &[(
                vk::Extent3D {
                    width: ix,
                    height: iy,
                    depth: iz,
                },
                bytemuck::cast_slice(&sdf.indirection),
            )],
        ) {
            Ok(parts) => parts,
            Err(err) => {
                // SAFETY: the ash/VMA seam. The atlas image+view were created above and not
                // yet owned by a `GpuSdf`; free them once on this error path.
                unsafe {
                    self.raw().destroy_image_view(atlas_view, None);
                }
                self.destroy_image(atlas_image, atlas_alloc);
                return Err(err);
            }
        };
        let (indirection_image, indirection_view, indirection_alloc) = indir;

        let [cx, cy, cz] = h.coverage_dims;
        let coverage = match self.create_and_upload_sdf_image(
            vk::Format::R16_SNORM,
            &[(
                vk::Extent3D {
                    width: cx,
                    height: cy,
                    depth: cz,
                },
                bytemuck::cast_slice(&sdf.coverage),
            )],
        ) {
            Ok(parts) => parts,
            Err(err) => {
                // SAFETY: the ash/VMA seam. The atlas + indirection were created above and not
                // yet owned by a `GpuSdf`; free both once on this error path.
                unsafe {
                    let raw = self.raw();
                    raw.destroy_image_view(atlas_view, None);
                    raw.destroy_image_view(indirection_view, None);
                }
                self.destroy_image(atlas_image, atlas_alloc);
                self.destroy_image(indirection_image, indirection_alloc);
                return Err(err);
            }
        };
        let (coverage_image, coverage_view, coverage_alloc) = coverage;

        let Some(index) = descriptors.claim_sdf_slot() else {
            tracing::warn!(
                "SDF bindless array full ({}), field skipped",
                descriptors.sdf_capacity()
            );
            // SAFETY: the ash/VMA seam. All three images+views were created above and not
            // yet owned by a `GpuSdf`; free them once on this array-full path.
            unsafe {
                let raw = self.raw();
                raw.destroy_image_view(atlas_view, None);
                raw.destroy_image_view(indirection_view, None);
                raw.destroy_image_view(coverage_view, None);
            }
            self.destroy_image(atlas_image, atlas_alloc);
            self.destroy_image(indirection_image, indirection_alloc);
            self.destroy_image(coverage_image, coverage_alloc);
            return Err(Error::BindlessFull("per-mesh SDF"));
        };
        descriptors.write_sdf_texture(atlas_view, indirection_view, coverage_view, index);

        let field = GpuSdf::from_parts(
            &self.resources,
            GpuSdfParts {
                atlas_image,
                atlas_view,
                atlas_alloc,
                indirection_image,
                indirection_view,
                indirection_alloc,
                coverage_image,
                coverage_view,
                coverage_alloc,
                bindless_index: index,
                bounds_min: Vec3::from(h.bounds_min),
                bounds_max: Vec3::from(h.bounds_max),
                max_dist: h.max_dist,
                occupancy_unorm: h.occupancy_unorm,
                proxy_albedo: h.proxy_albedo,
                voxel_dims: h.dims,
                indirection_dims: h.indirection_dims,
                atlas_bricks: h.atlas_bricks,
                mip_count,
            },
            descriptors.sdf_free_list(),
        );
        Ok(Arc::new(field))
    }

    /// Creates a device-local sampled 3D image of `format` with `mips.len()` prefiltered mip
    /// levels, uploads each level's bytes into its subresource through one staging copy, and
    /// leaves every level `SHADER_READ_ONLY_OPTIMAL`, returning the image, its `TYPE_3D` view
    /// (spanning all levels), and allocation. The shared shape of the SDST v3 atlas (3 mips),
    /// indirection (1 mip), and coverage (1 mip) uploads — each `mips` entry is one level's
    /// `(extent, bytes)`, finest first.
    fn create_and_upload_sdf_image(
        &self,
        format: vk::Format,
        mips: &[(vk::Extent3D, &[u8])],
    ) -> Result<(vk::Image, vk::ImageView, vk_mem::Allocation)> {
        let base_extent = mips[0].0;
        let mip_levels = mips.len() as u32;

        // One staging buffer holds every mip level's bytes, concatenated; record one
        // buffer→image copy per level at its byte offset and mip subresource.
        let total: usize = mips.iter().map(|(_, b)| b.len()).sum();
        let mut staging = StagingBuffer::new(self.allocator(), (total as vk::DeviceSize).max(4))?;
        let mut offsets: Vec<vk::DeviceSize> = Vec::with_capacity(mips.len());
        {
            let dst = staging.mapped_slice();
            let mut cursor = 0usize;
            for (_, bytes) in mips {
                offsets.push(cursor as vk::DeviceSize);
                dst[cursor..cursor + bytes.len()].copy_from_slice(bytes);
                cursor += bytes.len();
            }
        }
        staging.flush();

        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_3D)
            .format(format)
            .extent(base_extent)
            .mip_levels(mip_levels)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the image is owned by the caller
        // (the returned `GpuSdf`, or freed on a later failure).
        let (image, allocation) = checked(
            unsafe { self.allocator().create_image(&info, &alloc_info) },
            "vmaCreateImage (sdf)",
        )?;

        let recorded = self.with_one_off_commands("create_and_upload_sdf_image", |cmd| {
            // SAFETY: the ash seam. The image/staging buffer outlive the submit-wait.
            unsafe {
                let raw = self.raw();
                transition_image(
                    raw,
                    cmd,
                    image,
                    mip_levels,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                for (level, ((extent, _), &offset)) in mips.iter().zip(offsets.iter()).enumerate() {
                    let region = vk::BufferImageCopy::default()
                        .buffer_offset(offset)
                        .image_subresource(vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: level as u32,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .image_extent(*extent);
                    raw.cmd_copy_buffer_to_image(
                        cmd,
                        staging.handle(),
                        image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &[region],
                    );
                }
                transition_image(
                    raw,
                    cmd,
                    image,
                    mip_levels,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER
                        | vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
            }
        });
        drop(staging);
        if let Err(err) = recorded {
            self.destroy_image(image, allocation);
            return Err(err);
        }

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_3D)
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: mip_levels,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the 3D image just uploaded.
        let view = match unsafe { self.raw().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                self.destroy_image(image, allocation);
                return Err(Error::Vk {
                    context: "create_image_view (sdf)",
                    result,
                });
            }
        };
        Ok((image, view, allocation))
    }

    /// Uploads the "empty space" default SDF (a one-brick all-`+max` SDST v2 field) and
    /// seeds it into *every* SDF bindless slot (bindings 1 + 2), returning the [`GpuSdf`] the
    /// renderer holds for its lifetime.
    ///
    /// The bindless SDF arrays are partially bound, but the übershader declares them whole and
    /// lavapipe faults on an unbound slot even one the shader never samples (undefined behaviour
    /// on real hardware). The default field's single brick is empty (every voxel saturates to
    /// `+max`), so an instance that resolves to it contributes no occlusion.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`]/[`Error::ZeroSizedImage`] for a failing Vulkan/VMA call during
    /// the SDF upload.
    pub fn upload_default_sdf(&self, descriptors: &Descriptors) -> Result<Arc<GpuSdf>> {
        let grid = GridDesc {
            dims: [8, 8, 8],
            bounds_min: Vec3::splat(-0.5),
            bounds_max: Vec3::splat(0.5),
            max_dist: 1.0,
        };
        let dense = vec![i16::MAX; 8 * 8 * 8];
        let sdf = Sdf::from_dense_field(&grid, &dense);
        let field = self.upload_sdf(descriptors, &sdf)?;
        descriptors.seed_all_sdf_textures(
            field.atlas_view(),
            field.indirection_view(),
            field.coverage_view(),
        );
        Ok(field)
    }
}
