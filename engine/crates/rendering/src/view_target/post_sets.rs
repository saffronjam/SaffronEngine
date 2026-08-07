//! Descriptor writes for the post chain's per-view sets: the colour grade, fog, atmosphere
//! LUTs, and the creative-look table.

use super::*;

impl ViewTarget {
    /// Writes `uniform` into frame slot `frame`'s slice of the grade UBO (persistently mapped). The
    /// tonemap dispatch then binds the set with a `frame * grade_ubo_stride` dynamic offset, so this
    /// frame's grade is read without a descriptor rewrite. Called each frame before the tonemap pass.
    pub fn write_grade(&mut self, frame: usize, uniform: &crate::GradeUniform) {
        let offset = self.grade_ubo_stride as usize * frame;
        let src = bytemuck::bytes_of(uniform);
        let dst = self.grade_ubo.mapped_bytes().expect("grade UBO is MAPPED");
        dst[offset..offset + src.len()].copy_from_slice(src);
    }

    /// The dynamic offset for frame slot `frame`'s grade-UBO slice — supplied to the tonemap set bind.
    #[must_use]
    pub fn grade_ubo_offset(&self, frame: usize) -> u32 {
        (self.grade_ubo_stride * frame as u64) as u32
    }

    /// Writes `params` into frame slot `frame`'s slice of the fog UBO (persistently mapped). The fog
    /// dispatch then binds the set with a `frame * fog_ubo_stride` dynamic offset. Called each frame
    /// before the fog pass.
    pub(crate) fn write_fog(&mut self, frame: usize, params: &crate::renderer::FogParams) {
        let offset = self.fog_ubo_stride as usize * frame;
        let src = bytemuck::bytes_of(params);
        let dst = self.fog_ubo.mapped_bytes().expect("fog UBO is MAPPED");
        dst[offset..offset + src.len()].copy_from_slice(src);
    }

    /// The dynamic offset for frame slot `frame`'s fog-UBO slice — supplied to the fog set bind.
    #[must_use]
    pub fn fog_ubo_offset(&self, frame: usize) -> u32 {
        (self.fog_ubo_stride * frame as u64) as u32
    }

    /// Writes the sky-view LUT `view` into binding 3 of the fog set with `sampler`. Called once at
    /// renderer init (the LUT image is allocated once and reused across bakes), so this binding
    /// persists across resizes (the fog set is allocated once, never reallocated).
    pub fn write_fog_sky_lut(&self, device: &Device, sampler: vk::Sampler, view: vk::ImageView) {
        let info = [vk::DescriptorImageInfo {
            sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.fog_set)
            .dst_binding(3)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info);
        // SAFETY: the ash seam. The set + view + sampler outlive the call; single-threaded at the
        // (idle) build point, so no in-flight command buffer references this set.
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
    }

    /// Writes the froxel integration volume `view` into binding 4 of the fog set with `sampler` (the
    /// volumetric composite's trilinear sample). The volume is fixed-size and never reallocated, so
    /// this binding persists across resizes; the composite reads it only in `fog.mode == volumetric`.
    pub fn write_fog_integration(
        &self,
        device: &Device,
        sampler: vk::Sampler,
        view: vk::ImageView,
    ) {
        let info = [vk::DescriptorImageInfo {
            sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.fog_set)
            .dst_binding(4)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info);
        // SAFETY: the ash seam. The set + view + sampler outlive the call; single-threaded at the
        // (idle) build point, so no in-flight command buffer references this set.
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
    }

    /// Writes the aerial-perspective volume `view` into binding 5 of the fog set with `sampler` (the
    /// composite's trilinear AP sample). The volume is fixed-size and never reallocated, so this binding
    /// persists across resizes; the composite reads it only when the atmosphere is live + AP authored.
    pub fn write_fog_aerial(&self, device: &Device, sampler: vk::Sampler, view: vk::ImageView) {
        let info = [vk::DescriptorImageInfo {
            sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.fog_set)
            .dst_binding(5)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info);
        // SAFETY: the ash seam. The set + view + sampler outlive the call; single-threaded at the
        // (idle) build point, so no in-flight command buffer references this set.
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
    }

    /// Writes the atmosphere transmittance and multiscatter LUTs into fog bindings 6 and 7.
    pub fn write_fog_atmosphere_luts(
        &self,
        device: &Device,
        sampler: vk::Sampler,
        transmittance: vk::ImageView,
        multi_scatter: vk::ImageView,
    ) {
        let infos = [transmittance, multi_scatter].map(|view| {
            [vk::DescriptorImageInfo {
                sampler,
                image_view: view,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            }]
        });
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(self.fog_set)
                .dst_binding(6)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&infos[0]),
            vk::WriteDescriptorSet::default()
                .dst_set(self.fog_set)
                .dst_binding(7)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&infos[1]),
        ];
        unsafe { device.raw().update_descriptor_sets(&writes, &[]) };
    }

    /// Writes the creative-look 3D LUT `view` into binding 2 of the tonemap set with `sampler`. Called
    /// at view build with the identity default and rewritten (idled) when a creative look is assigned.
    /// The set is allocated once and never reallocated, so this binding persists across resizes.
    pub fn write_tonemap_lut(&self, device: &Device, sampler: vk::Sampler, view: vk::ImageView) {
        let info = [vk::DescriptorImageInfo {
            sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.tonemap_set)
            .dst_binding(2)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info);
        // SAFETY: the ash seam. The set + view + sampler outlive the call; single-threaded at the
        // (idle) build/assign point, so no in-flight command buffer references this set.
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
    }
}
