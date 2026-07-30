//! The partial-construction guard: holds the handles created so far and frees exactly those
//! if a later step fails, so a failed bring-up leaks nothing.

use super::*;

/// Holds the partially-built handles during [`Descriptors::new`] so a mid-init
/// failure frees what was already created (each `?` short-circuits to this `Drop`).
/// On success, the `take_*` methods move every handle out (clearing the field) so the
/// `Drop` frees nothing.
pub(super) struct Partial<'a> {
    pub(super) resources: &'a Arc<DeviceResources>,
    pub(super) linear_sampler: Option<vk::Sampler>,
    pub(super) shadow_sampler: Option<vk::Sampler>,
    pub(super) sdf_sampler: Option<vk::Sampler>,
    pub(super) minmax_sampler: Option<vk::Sampler>,
    pub(super) bindless_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) light_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) instance_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) ibl_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) ssao_mesh_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) ddgi_mesh_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) rt_mesh_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) restir_mesh_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) cluster_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) tonemap_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) fog_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) fxaa_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) bloom_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) taa_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) depth_upscale_set_layout: Option<vk::DescriptorSetLayout>,
    pub(super) descriptor_pool: Option<vk::DescriptorPool>,
    pub(super) bindless_pool: Option<vk::DescriptorPool>,
}

/// Generates the `take_<field>` accessor (moves the handle out, leaving `None` so the
/// `Drop` skips it) for every owned handle in [`Partial`].
macro_rules! partial_take {
    ($($take:ident => $field:ident: $ty:ty),+ $(,)?) => {
        $(
            pub(super) fn $take(&mut self) -> $ty {
                self.$field.take().expect("partial handle built before take")
            }
        )+
    };
}

impl<'a> Partial<'a> {
    pub(super) fn new(resources: &'a Arc<DeviceResources>) -> Self {
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
