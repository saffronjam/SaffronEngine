//! The device-global descriptor-set layouts, the samplers, and the two pools they allocate
//! from. Every layout is immutable after init.

use super::*;

/// The linear repeat sampler: linear min/mag/mip, repeat address, no LOD clamp.
pub(super) fn create_linear_sampler(raw: &ash::Device, max_anisotropy: f32) -> Result<vk::Sampler> {
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
pub(super) fn create_shadow_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
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
pub(super) fn create_minmax_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
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
pub(super) fn create_sdf_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
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
pub(super) fn create_bindless_layout(
    raw: &ash::Device,
    texture_capacity: u32,
    sdf_capacity: u32,
) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(texture_capacity)
            // FRAGMENT for the übershader's material sampling + COMPUTE for the `displace` pre-pass,
            // which samples the height (and vector-displacement) map from this same bindless array.
            .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(sdf_capacity)
            // The per-mesh brick atlas. COMPUTE-only: the GDF composite (`gdf_composite`) and the
            // DDGI ray trace's near field (`sdf::sampleField`) are the only consumers. The
            // fragment lighting path reads the composited GDF clipmap (set 1 / 9-10), not the
            // per-mesh bricks.
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(2)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .descriptor_count(sdf_capacity)
            // The brick indirection volume, read by integer texel `Load` in the same two compute
            // consumers as the atlas (the GDF composite + the DDGI trace near field).
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(3)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(sdf_capacity)
            // The coarse coverage volume (one texel per brick), sampled for the empty-space
            // march leap + early-out — same two compute consumers as the atlas.
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(4)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(texture_capacity)
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
pub(super) fn create_light_layout(
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
        shadow_binding(13),         // virtual-shadow physical atlas (immutable compare sampler)
        // Porous-occupancy cascade volumes (binding 14): the aggregate density the GDF
        // consumers march through. FRAGMENT for the übershader's reflection occlusion,
        // COMPUTE for the DDGI trace + DFAO cones.
        vk::DescriptorSetLayoutBinding::default()
            .binding(14)
            .descriptor_type(sampler)
            .descriptor_count(crate::GDF_CASCADES)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::COMPUTE),
        // The occluder scatter's meta words (binding 15): the GPU-produced instance
        // count the DDGI near-field march bounds itself by. COMPUTE-only, like the
        // instance list it counts.
        vk::DescriptorSetLayoutBinding::default()
            .binding(15)
            .descriptor_type(storage)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
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
pub(super) fn light_binding(
    slot: u32,
    kind: vk::DescriptorType,
) -> vk::DescriptorSetLayoutBinding<'static> {
    vk::DescriptorSetLayoutBinding::default()
        .binding(slot)
        .descriptor_type(kind)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::COMPUTE)
}

/// Set 2: the material-parameter arena (vertex and fragment), the GPU-scene address block, and
/// the executor's record + command streams.
///
/// `mesh_shader` widens the stage flags of the executor-facing bindings so the mesh executor can
/// read them; naming a stage the device does not support is invalid, so the flag must come from
/// [`crate::Capabilities::mesh_shader`] rather than being assumed.
pub(super) fn instance_layout_bindings(
    mesh_shader: bool,
) -> [vk::DescriptorSetLayoutBinding<'static>; 4] {
    let executor_stages = if mesh_shader {
        vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::MESH_EXT
    } else {
        vk::ShaderStageFlags::VERTEX
    };
    let scene_stages = executor_stages | vk::ShaderStageFlags::FRAGMENT;
    let storage = vk::DescriptorType::STORAGE_BUFFER;
    [
        vk::DescriptorSetLayoutBinding::default()
            .binding(2)
            .descriptor_type(storage)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
        // The GPU-scene address block: every table's buffer device address for this frame,
        // read wherever the übershader family resolves persistent scene records.
        vk::DescriptorSetLayoutBinding::default()
            .binding(3)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .descriptor_count(1)
            .stage_flags(scene_stages),
        // The active view's semantic record stream the executor vertex path indexes.
        vk::DescriptorSetLayoutBinding::default()
            .binding(4)
            .descriptor_type(storage)
            .descriptor_count(1)
            .stage_flags(executor_stages),
        // The binner's indexed command stream. The indexed executor consumes it as draw
        // arguments; the mesh executor reads the same words as data, recovering its draw from
        // `SV_DrawIndex` and its triangle block from the group id.
        vk::DescriptorSetLayoutBinding::default()
            .binding(5)
            .descriptor_type(storage)
            .descriptor_count(1)
            .stage_flags(executor_stages),
    ]
}

pub(super) fn create_instance_layout(
    raw: &ash::Device,
    mesh_shader: bool,
) -> Result<vk::DescriptorSetLayout> {
    let bindings = instance_layout_bindings(mesh_shader);
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
pub(super) fn create_ibl_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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
pub(super) fn create_ssao_mesh_layout(
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
pub(super) fn create_ddgi_mesh_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let sampler = vk::DescriptorType::COMBINED_IMAGE_SAMPLER;
    let bindings = [light_binding(0, sampler), light_binding(1, sampler)];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "ddgiMeshLayout",
    )
}

/// Set 6 (mesh pipeline, RT only): the top-level structure — one fragment-stage
/// acceleration structure.
///
/// The descriptor TYPE is a per-device variant: a partitioned structure binds as
/// `PARTITIONED_ACCELERATION_STRUCTURE_NV` rather than `ACCELERATION_STRUCTURE_KHR`. The
/// shader declaration is identical either way — only the layout distinguishes them — which
/// is why this is one binding with two types rather than two bindings.
pub(super) fn create_rt_mesh_layout(
    raw: &ash::Device,
    partitioned: bool,
) -> Result<vk::DescriptorSetLayout> {
    let bindings = [light_binding(
        0,
        if partitioned {
            crate::vk_nv_ptlas::DESCRIPTOR_TYPE_PARTITIONED_ACCELERATION_STRUCTURE_NV
        } else {
            vk::DescriptorType::ACCELERATION_STRUCTURE_KHR
        },
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
pub(super) fn create_restir_mesh_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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
pub(super) fn create_cluster_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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
pub(super) fn create_tonemap_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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
pub(super) fn create_fog_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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
pub(super) fn create_fxaa_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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
pub(super) fn create_bloom_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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
pub(super) fn create_taa_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        compute_binding(0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(1, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        compute_binding(3, vk::DescriptorType::STORAGE_IMAGE),
        compute_binding(4, vk::DescriptorType::STORAGE_IMAGE),
        compute_binding(5, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
        // 6 = reactive coverage (input R8), 7 = the previous lock image (display, sampled at the
        // reprojected UV), 8 = this frame's lock image (display, written).
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
pub(super) fn create_depth_upscale_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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

pub(super) fn compute_binding(
    slot: u32,
    kind: vk::DescriptorType,
) -> vk::DescriptorSetLayoutBinding<'static> {
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
pub(super) fn create_descriptor_pool(
    raw: &ash::Device,
    rt_supported: bool,
    partitioned: bool,
) -> Result<vk::DescriptorPool> {
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
            // +2*views for the HZB copy sets' depth sampler (two pyramids per view),
            // +2*frames*views for the visibility cull/retest sets' pyramid sampler,
            // +GDF_CASCADES*frames for the porous-occupancy volumes (binding 14) on each
            // frame's light set.
            1024 + 3 * bloom_sets
                + views
                + 1
                + 3 * views
                + frames
                + 2 * views
                + 2 * frames * views
                + crate::GDF_CASCADES * frames,
        ),
        // +frames for the GDF cascade-params UBO (binding 10) on each frame's light set,
        // +views for the GPU-scene address block (binding 6) on each view's ReSTIR
        // resolve set, +5*frames*views for the visibility chain's per-frame-slot
        // address-block bindings.
        pool_size(
            vk::DescriptorType::UNIFORM_BUFFER,
            5 * frames + 8 + views + 5 * frames * views,
        ),
        // One dynamic-offset grade UBO per view (binding 1 of the tonemap set), +1 for the transient
        // look-bake set, +views for the per-view fog params UBO (binding 1 of the fog set).
        pool_size(
            vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC,
            views + 1 + views,
        ),
        // +8 for the device-shared GDF cull + composite sets (two storage buffers each),
        // +21*frames*views for the visibility chain's per-frame-slot sets (cull/retest
        // 4 each, traversal 3, bin count/scan 3 each, bin scatter 4).
        // +2*frames for the instance sets' executor record + command streams.
        pool_size(
            vk::DescriptorType::STORAGE_BUFFER,
            8 * frames + 24 + 8 * views + 21 * frames * views + 2 * frames,
        ),
        // +(GDF_CASCADES + 1) per frame for each composite set's cascade storage-image array + the
        // lite albedo cache. The per-view budget (29) covers the DFAO and specular-occlusion chains'
        // storage images (each: trace out + blur out + two accum sets = 6, so 12 total) on top of
        // the SSGI/AA sets, plus the TAA lock-write storage image (binding 8).
        pool_size(
            // +1 for the transient look-bake set's storage output image (binding 0), +views for the
            // per-view fog set's offscreen storage image (binding 0).
            vk::DescriptorType::STORAGE_IMAGE,
            // +2*views*(2*HZB_MAX_MIPS) for the HZB build sets (two pyramids per view,
            // one storage write per mip plus one storage read per reduce).
            48 + 29 * views
                + bloom_sets
                + (2 * crate::GDF_CASCADES + 1) * frames
                + 1
                + views
                + 2 * views * (2 * crate::HZB_MAX_MIPS as u32),
        ),
        // The mesh screen-space set carries eight sampled images behind one immutable sampler.
        pool_size(vk::DescriptorType::SAMPLED_IMAGE, 8 * views),
        // One immutable sampler descriptor per mesh screen-space set.
        pool_size(vk::DescriptorType::SAMPLER, views),
    ];
    if rt_supported {
        pool_sizes.push(pool_size(
            if partitioned {
                crate::vk_nv_ptlas::DESCRIPTOR_TYPE_PARTITIONED_ACCELERATION_STRUCTURE_NV
            } else {
                vk::DescriptorType::ACCELERATION_STRUCTURE_KHR
            },
            frames + 2 + views,
        ));
    }
    let info = vk::DescriptorPoolCreateInfo::default()
        .flags(
            vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET
                | vk::DescriptorPoolCreateFlags::UPDATE_AFTER_BIND,
        )
        .max_sets(
            1024 + 8 * frames
                + 64
                + 21 * views
                + bloom_sets
                + 1
                + views
                + 2 * views * crate::HZB_MAX_MIPS as u32
                + 7 * frames * views,
        )
        .pool_sizes(&pool_sizes);
    // SAFETY: the ash seam. The pool is owned and freed in teardown.
    checked(
        unsafe { raw.create_descriptor_pool(&info, None) },
        "descriptorPool",
    )
}

/// The bindless set's own pool: `UPDATE_AFTER_BIND`, one set, sized for the full
/// bindless array.
pub(super) fn create_bindless_pool(
    raw: &ash::Device,
    texture_capacity: u32,
    sdf_capacity: u32,
) -> Result<vk::DescriptorPool> {
    let pool_sizes = [
        pool_size(
            vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            // Albedo (binding 0) + brick atlas (binding 1) + coverage (binding 3) + the per-height
            // min/max pyramid (binding 4, another `texture_capacity` slots).
            2 * texture_capacity + 2 * sdf_capacity,
        ),
        // The brick-indirection array is a separate sampled-image (no sampler) binding.
        pool_size(vk::DescriptorType::SAMPLED_IMAGE, sdf_capacity),
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
pub(super) fn pool_size(kind: vk::DescriptorType, count: u32) -> vk::DescriptorPoolSize {
    vk::DescriptorPoolSize {
        ty: kind,
        descriptor_count: count,
    }
}

/// Allocates the single bindless set from `pool` against `layout`.
pub(super) fn allocate_bindless_set(
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
