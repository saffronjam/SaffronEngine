use std::collections::HashSet;
use std::mem::{align_of, offset_of, size_of};

use saffron_spatial::UnitInterval;

use super::*;

#[test]
fn handle_reuse_waits_for_every_frame_slot_and_bumps_generation() {
    let mut table = ImmutableGpuTable::default();
    let first = table.insert(7_u32).unwrap();
    assert_eq!(table.retire(first), Some(7));
    assert!(table.get(first).is_none());

    table.begin_frame(0).unwrap();
    let second = table.insert(8_u32).unwrap();
    assert_ne!(second.index, first.index);

    for frame in 1..MAX_FRAMES_IN_FLIGHT {
        table.begin_frame(frame).unwrap();
    }
    let reused = table.insert(9_u32).unwrap();
    assert_eq!(reused.index, first.index);
    assert_ne!(reused.generation, first.generation);
    assert!(table.get(first).is_none());
    assert_eq!(table.get(reused), Some(&9));
}

#[test]
fn range_reuse_waits_for_every_frame_slot_and_coalesces() {
    let mut ranges = GpuRangeAllocator::default();
    let first = ranges.allocate(4, 1).unwrap();
    let second = ranges.allocate(6, 1).unwrap();
    ranges.retire(first).unwrap();
    ranges.retire(second).unwrap();
    ranges.begin_frame(0).unwrap();
    assert_eq!(ranges.allocate(10, 1).unwrap().first, 10);

    for frame in 1..MAX_FRAMES_IN_FLIGHT {
        ranges.begin_frame(frame).unwrap();
    }
    assert_eq!(
        ranges.allocate(10, 1).unwrap(),
        GpuArenaRange {
            first: 0,
            count: 10
        }
    );
}

#[test]
fn aligned_range_allocation_splits_and_reuses_gaps() {
    let mut ranges = GpuRangeAllocator::default();
    assert_eq!(ranges.allocate(3, 1).unwrap().first, 0);
    assert_eq!(ranges.allocate(2, 8).unwrap().first, 8);
    assert_eq!(ranges.required_capacity(), 10);
    assert_eq!(ranges.allocate(5, 1).unwrap().first, 3);
}

#[test]
fn odd_byte_ranges_reserve_nonoverlapping_copy_aligned_spans() {
    let mut ranges = GpuRangeAllocator::default();
    let mut allocate = |count| {
        let (reserved, alignment) = physical_range_layout(count, 1, 1).unwrap();
        let logical = ranges
            .allocate_reserved(count, reserved, alignment)
            .unwrap();
        let physical = ranges.reserved_range(logical).unwrap();
        (logical, physical)
    };
    let (one, one_physical) = allocate(1);
    let (three, three_physical) = allocate(3);
    let (five, five_physical) = allocate(5);
    assert_eq!((one.first, one.count, one_physical.count), (0, 1, 4));
    assert_eq!((three.first, three.count, three_physical.count), (4, 3, 4));
    assert_eq!((five.first, five.count, five_physical.count), (8, 5, 8));
    assert_eq!(ranges.required_capacity(), 16);
}

#[test]
fn range_retirement_rejects_duplicate_or_partial_allocations() {
    let mut ranges = GpuRangeAllocator::default();
    let range = ranges.allocate(8, 4).unwrap();
    assert!(
        ranges
            .retire(GpuArenaRange {
                first: range.first,
                count: range.count - 1,
            })
            .is_err()
    );
    ranges.retire(range).unwrap();
    assert!(ranges.retire(range).is_err());
}

#[test]
fn invalid_frame_slots_are_rejected_without_shifting() {
    let mut table = ImmutableGpuTable::<u32>::default();
    assert!(table.begin_frame(MAX_FRAMES_IN_FLIGHT).is_err());
    let mut ranges = GpuRangeAllocator::default();
    assert!(ranges.begin_frame(MAX_FRAMES_IN_FLIGHT).is_err());
}

#[test]
fn exhausted_generation_is_never_reused() {
    let mut table = ImmutableGpuTable::default();
    let handle = table.insert(7_u32).unwrap();
    table.slots[handle.index as usize].generation = u32::MAX;
    let exhausted = GpuHandle {
        index: handle.index,
        generation: u32::MAX,
    };
    assert_eq!(table.retire(exhausted), Some(7));
    for frame in 0..MAX_FRAMES_IN_FLIGHT {
        table.begin_frame(frame).unwrap();
    }
    assert_ne!(table.insert(8).unwrap().index, exhausted.index);
}

#[test]
fn upload_planning_preserves_cursor_across_same_frame_growth() {
    let first = plan_upload(0, 16, 12, 4).unwrap();
    assert_eq!(first.offset, 0);
    assert_eq!(first.required_capacity, 16);
    let second = plan_upload(first.end, first.required_capacity, 12, 16).unwrap();
    assert_eq!(second.offset, 16);
    assert!(second.required_capacity >= 28);
    let third = plan_upload(second.end, second.required_capacity, 5, 8).unwrap();
    assert_eq!(third.offset, 32);
    assert!(third.end > second.end);
}

#[test]
fn pso_bin_dimensions_round_trip_without_aliasing() {
    let representations = [
        GpuRepresentation::TriangleCluster,
        GpuRepresentation::AggregateVoxel,
        GpuRepresentation::MicroBlade,
        GpuRepresentation::DisplacedMicro,
    ];
    let coverage = [
        AlphaClassification::Opaque,
        AlphaClassification::Masked,
        AlphaClassification::Transmissive,
    ];
    let sidedness = [GpuSidedness::Single, GpuSidedness::Double];
    let surface_models = [SurfaceModel::Standard, SurfaceModel::ThinSheetFoliage];
    let transparency = [GpuTransparency::Opaque, GpuTransparency::AlphaBlended];
    let deformation = [GpuDeformation::Rigid, GpuDeformation::Deformed];
    let passes = [
        GpuPassClass::Depth,
        GpuPassClass::Main,
        GpuPassClass::Motion,
        GpuPassClass::Shadow,
        GpuPassClass::GBuffer,
        GpuPassClass::Transparent,
        GpuPassClass::Selection,
        GpuPassClass::Preview,
        GpuPassClass::RayTracing,
        GpuPassClass::WireDebug,
    ];
    let mut bits = HashSet::new();
    let mut material_bits = HashSet::new();
    for representation in representations {
        for coverage in coverage {
            for sidedness in sidedness {
                for surface_model in surface_models {
                    for transparency in transparency {
                        let material = GpuMaterialClass::new(
                            coverage,
                            sidedness,
                            surface_model,
                            transparency,
                            false,
                        );
                        material_bits.insert(material.bits());
                        assert_eq!(GpuMaterialClass::from_bits(material.bits()), Some(material));
                        for deformation in deformation {
                            for pass in passes {
                                let bin =
                                    GpuPsoBin::new(representation, material, deformation, pass);
                                assert!(bits.insert(bin.bits()));
                                assert_eq!(GpuPsoBin::from_bits(bin.bits()), Some(bin));
                                assert_eq!(bin.representation(), representation);
                                assert_eq!(bin.coverage(), coverage);
                                assert_eq!(bin.sidedness(), sidedness);
                                assert_eq!(bin.surface_model(), surface_model);
                                assert_eq!(bin.transparency(), transparency);
                                assert_eq!(bin.deformation(), deformation);
                                assert_eq!(bin.pass(), pass);
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(bits.len(), 4 * 3 * 2 * 2 * 2 * 2 * 10);
    assert_eq!(material_bits.len(), 3 * 2 * 2 * 2);
    assert_eq!(GpuMaterialClass::from_bits(3), None);
    // Coverage 3 is not a classification, and pass class 10 is past the vocabulary.
    assert_eq!(GpuPsoBin::from_bits(3 << GPU_PSO_COVERAGE_SHIFT), None);
    assert_eq!(GpuPsoBin::from_bits(1 << 15), None);
    assert_eq!(GpuPsoBin::from_bits(10 << GPU_PSO_PASS_SHIFT), None);
}

#[test]
fn gpu_abi_layouts_are_byte_locked() {
    assert_eq!((size_of::<GpuHandle>(), align_of::<GpuHandle>()), (8, 8));
    assert_eq!(offset_of!(GpuHandle, index), 0);
    assert_eq!(offset_of!(GpuHandle, generation), 4);
    assert_eq!(
        (size_of::<GpuArenaRange>(), align_of::<GpuArenaRange>()),
        (8, 8)
    );
    assert_eq!(offset_of!(GpuArenaRange, first), 0);
    assert_eq!(offset_of!(GpuArenaRange, count), 4);
    assert_eq!(
        (
            size_of::<GpuTableSlotHeader>(),
            align_of::<GpuTableSlotHeader>()
        ),
        (16, 4)
    );
    assert_eq!(offset_of!(GpuTableSlotHeader, generation), 0);
    assert_eq!(offset_of!(GpuTableSlotHeader, occupied), 4);
    assert_eq!(offset_of!(GpuTableSlotHeader, reserved), 8);
    assert_eq!(
        (
            size_of::<GpuPrototypeRecord>(),
            align_of::<GpuPrototypeRecord>()
        ),
        (64, 16)
    );
    assert_eq!(offset_of!(GpuPrototypeRecord, geometry), 0);
    assert_eq!(offset_of!(GpuPrototypeRecord, material_range), 8);
    assert_eq!(offset_of!(GpuPrototypeRecord, skeleton), 16);
    assert_eq!(offset_of!(GpuPrototypeRecord, root_page), 24);
    assert_eq!(offset_of!(GpuPrototypeRecord, bounds), 32);
    assert_eq!(offset_of!(GpuPrototypeRecord, flags), 48);
    assert_eq!(offset_of!(GpuPrototypeRecord, reserved), 52);
    assert_eq!(
        (
            size_of::<GpuGeometryRecord>(),
            align_of::<GpuGeometryRecord>()
        ),
        (64, 8)
    );
    assert_eq!(offset_of!(GpuGeometryRecord, vertices), 0);
    assert_eq!(offset_of!(GpuGeometryRecord, indices), 8);
    assert_eq!(offset_of!(GpuGeometryRecord, clusters), 16);
    assert_eq!(offset_of!(GpuGeometryRecord, parts), 24);
    assert_eq!(offset_of!(GpuGeometryRecord, voxels), 32);
    assert_eq!(offset_of!(GpuGeometryRecord, submeshes), 40);
    assert_eq!(
        (
            size_of::<GpuSubmeshRecord>(),
            align_of::<GpuSubmeshRecord>()
        ),
        (16, 8)
    );
    assert_eq!(
        (
            size_of::<GpuMaterialTableRecord>(),
            align_of::<GpuMaterialTableRecord>()
        ),
        (48, 8)
    );
    assert_eq!(offset_of!(GpuMaterialTableRecord, base_color_texture), 0);
    assert_eq!(offset_of!(GpuMaterialTableRecord, normal_texture), 8);
    assert_eq!(offset_of!(GpuMaterialTableRecord, coverage), 16);
    assert_eq!(offset_of!(GpuMaterialTableRecord, parameter_index), 24);
    assert_eq!(offset_of!(GpuMaterialTableRecord, material_class), 28);
    assert_eq!(offset_of!(GpuMaterialTableRecord, shader_index), 32);
    assert_eq!(offset_of!(GpuMaterialTableRecord, proxy_albedo), 40);
    assert_eq!(offset_of!(GpuMaterialTableRecord, occupancy), 44);
    assert_eq!(offset_of!(GpuMaterialTableRecord, flags), 36);
    assert_eq!(
        (
            size_of::<GpuTextureTableRecord>(),
            align_of::<GpuTextureTableRecord>()
        ),
        (32, 4)
    );
    assert_eq!(offset_of!(GpuTextureTableRecord, descriptor_index), 0);
    assert_eq!(offset_of!(GpuTextureTableRecord, width), 4);
    assert_eq!(offset_of!(GpuTextureTableRecord, height), 8);
    assert_eq!(offset_of!(GpuTextureTableRecord, mip_count), 12);
    assert_eq!(offset_of!(GpuTextureTableRecord, flags), 16);
    assert_eq!(offset_of!(GpuTextureTableRecord, reserved), 20);
    assert_eq!(
        (
            size_of::<GpuCoverageRecord>(),
            align_of::<GpuCoverageRecord>()
        ),
        (48, 8)
    );
    assert_eq!(offset_of!(GpuCoverageRecord, texture), 0);
    assert_eq!(offset_of!(GpuCoverageRecord, classification), 12);
    assert_eq!(offset_of!(GpuCoverageRecord, omm_policy), 20);
    assert_eq!(offset_of!(GpuCoverageRecord, source_extent), 32);
    assert_eq!(offset_of!(GpuCoverageRecord, reserved), 44);
    assert_eq!(
        (
            size_of::<GpuSkeletonRecord>(),
            align_of::<GpuSkeletonRecord>()
        ),
        (32, 8)
    );
    assert_eq!(offset_of!(GpuSkeletonRecord, joints), 0);
    assert_eq!(offset_of!(GpuSkeletonRecord, inverse_binds), 8);
    assert_eq!(offset_of!(GpuSkeletonRecord, deformation), 16);
    assert_eq!(offset_of!(GpuSkeletonRecord, flags), 24);
    assert_eq!(offset_of!(GpuSkeletonRecord, reserved), 28);
    assert_eq!(
        (size_of::<GpuPageRecord>(), align_of::<GpuPageRecord>()),
        (40, 8)
    );
    assert_eq!(offset_of!(GpuPageRecord, parent), 0);
    assert_eq!(offset_of!(GpuPageRecord, dependencies), 8);
    assert_eq!(offset_of!(GpuPageRecord, byte_offset), 16);
    assert_eq!(offset_of!(GpuPageRecord, byte_length), 24);
    assert_eq!(offset_of!(GpuPageRecord, resident_generation), 28);
    assert_eq!(offset_of!(GpuPageRecord, flags), 32);
    assert_eq!(offset_of!(GpuPageRecord, reserved), 36);
    assert_eq!(
        (size_of::<GpuDrawRecord>(), align_of::<GpuDrawRecord>()),
        (64, 8)
    );
    assert_eq!((size_of::<GpuPsoBin>(), align_of::<GpuPsoBin>()), (4, 4));
    assert_eq!(
        (
            size_of::<GpuMaterialClass>(),
            align_of::<GpuMaterialClass>()
        ),
        (4, 4)
    );
    assert_eq!(offset_of!(GpuMaterialClass, 0), 0);
    assert_eq!(offset_of!(GpuGeometryRecord, flags), 48);
    assert_eq!(offset_of!(GpuGeometryRecord, vertex_stride), 52);
    assert_eq!(offset_of!(GpuGeometryRecord, index_stride), 56);
    assert_eq!(offset_of!(GpuGeometryRecord, reserved), 60);
    assert_eq!(offset_of!(GpuCoverageRecord, cutoff), 8);
    assert_eq!(offset_of!(GpuCoverageRecord, source_kind), 16);
    assert_eq!(offset_of!(GpuCoverageRecord, hash_salt), 24);
    assert_eq!(offset_of!(GpuCoverageRecord, omm_thresholds), 40);
    assert_eq!(
        (
            size_of::<GpuSkeletonJointRecord>(),
            align_of::<GpuSkeletonJointRecord>()
        ),
        (8, 8)
    );
    assert_eq!(offset_of!(GpuSkeletonJointRecord, parent), 0);
    assert_eq!(offset_of!(GpuSkeletonJointRecord, flags), 4);
    assert_eq!(
        (
            size_of::<GpuInverseBindRecord>(),
            align_of::<GpuInverseBindRecord>()
        ),
        (64, 16)
    );
    assert_eq!(offset_of!(GpuInverseBindRecord, matrix), 0);
    assert_eq!(
        (
            size_of::<GpuDeformationProviderRecord>(),
            align_of::<GpuDeformationProviderRecord>()
        ),
        (16, 16)
    );
    assert_eq!(offset_of!(GpuDeformationProviderRecord, provider_mask), 0);
    assert_eq!(offset_of!(GpuDeformationProviderRecord, first_parameter), 4);
    assert_eq!(offset_of!(GpuDeformationProviderRecord, parameter_count), 8);
    assert_eq!(offset_of!(GpuDeformationProviderRecord, flags), 12);
    assert_eq!(offset_of!(GpuDrawRecord, geometry), 0);
    assert_eq!(offset_of!(GpuDrawRecord, material), 8);
    assert_eq!(offset_of!(GpuDrawRecord, instance), 16);
    assert_eq!(offset_of!(GpuDrawRecord, deformation), 24);
    assert_eq!(offset_of!(GpuDrawRecord, pso_bin), 48);
    assert_eq!(offset_of!(GpuDrawRecord, content_index), 32);
    assert_eq!(offset_of!(GpuDrawRecord, part), 36);
    assert_eq!(offset_of!(GpuDrawRecord, representation), 40);
    assert_eq!(offset_of!(GpuDrawRecord, source_generation), 44);
    assert_eq!(offset_of!(GpuDrawRecord, transition), 52);
    assert_eq!(offset_of!(GpuDrawRecord, cluster_state), 56);
    assert_eq!(offset_of!(GpuDrawRecord, reserved), 60);
}

#[test]
fn slang_global_gpu_abi_is_locked_to_rust_constants_and_records() {
    let source = include_str!("../../../../assets/shaders/global_gpu_data.slang");
    for declaration in [
        format!("GLOBAL_GPU_DATA_ABI_VERSION = {GLOBAL_GPU_DATA_ABI_VERSION}u"),
        format!("GPU_PSO_REPRESENTATION_SHIFT = {GPU_PSO_REPRESENTATION_SHIFT}u"),
        format!(
            "GPU_REPRESENTATION_DISPLACED_MICRO = {}u",
            GpuRepresentation::DisplacedMicro as u32
        ),
        format!("GPU_PSO_MATERIAL_SHIFT = {GPU_PSO_MATERIAL_SHIFT}u"),
        format!("GPU_PSO_COVERAGE_SHIFT = {GPU_PSO_COVERAGE_SHIFT}u"),
        format!("GPU_PSO_SIDEDNESS_SHIFT = {GPU_PSO_SIDEDNESS_SHIFT}u"),
        format!("GPU_PSO_SURFACE_MODEL_SHIFT = {GPU_PSO_SURFACE_MODEL_SHIFT}u"),
        format!("GPU_PSO_TRANSPARENCY_SHIFT = {GPU_PSO_TRANSPARENCY_SHIFT}u"),
        format!("GPU_PSO_DEFORMATION_SHIFT = {GPU_PSO_DEFORMATION_SHIFT}u"),
        format!("GPU_PSO_PASS_SHIFT = {GPU_PSO_PASS_SHIFT}u"),
        format!("GPU_MATERIAL_COVERAGE_SHIFT = {GPU_MATERIAL_COVERAGE_SHIFT}u"),
        format!("GPU_MATERIAL_SIDEDNESS_SHIFT = {GPU_MATERIAL_SIDEDNESS_SHIFT}u"),
        format!("GPU_MATERIAL_SURFACE_MODEL_SHIFT = {GPU_MATERIAL_SURFACE_MODEL_SHIFT}u"),
        format!("GPU_MATERIAL_TRANSPARENCY_SHIFT = {GPU_MATERIAL_TRANSPARENCY_SHIFT}u"),
    ] {
        assert!(source.contains(&declaration), "missing `{declaration}`");
    }
    for (record, expected_body) in [
        ("GpuHandle", "public uint index; public uint generation;"),
        ("GpuArenaRange", "public uint first; public uint count;"),
        (
            "GpuTableSlotHeader",
            "public uint generation; public uint occupied; public uint2 reserved;",
        ),
        (
            "GpuPrototypeRecord",
            "public GpuHandle geometry; public GpuArenaRange materialRange; public GpuHandle skeleton; public GpuHandle rootPage; public float4 bounds; public uint flags; public uint3 reserved;",
        ),
        (
            "GpuGeometryRecord",
            "public GpuArenaRange vertices; public GpuArenaRange indices; public GpuArenaRange clusters; public GpuArenaRange parts; public GpuArenaRange voxels; public GpuArenaRange submeshes; public uint flags; public uint vertexStride; public uint indexStride; public uint reserved;",
        ),
        (
            "GpuMaterialTableRecord",
            "public GpuHandle baseColorTexture; public GpuHandle normalTexture; public GpuHandle coverage; public uint parameterIndex; public uint materialClass; public uint shaderIndex; public uint flags; public uint proxyAlbedo; public float occupancy;",
        ),
        (
            "GpuSdfTableRecord",
            "public float4 localMin; public float4 localMax; public uint4 voxelDims; public uint4 indirectionDims; public uint4 atlasBricks;",
        ),
        (
            "GpuTextureTableRecord",
            "public uint descriptorIndex; public uint width; public uint height; public uint mipCount; public uint flags; public uint reserved0; public uint reserved1; public uint reserved2;",
        ),
        (
            "GpuCoverageRecord",
            "public GpuHandle texture; public float cutoff; public uint classification; public uint sourceKind; public uint ommPolicy; public uint2 hashSalt; public uint2 sourceExtent; public uint ommThresholds; public uint reserved;",
        ),
        (
            "GpuSkeletonRecord",
            "public GpuArenaRange joints; public GpuArenaRange inverseBinds; public GpuArenaRange deformation; public uint flags; public uint reserved;",
        ),
        (
            "GpuSkeletonJointRecord",
            "public uint parent; public uint flags;",
        ),
        ("GpuInverseBindRecord", "public float4x4 matrix;"),
        (
            "GpuDeformationProviderRecord",
            "public uint providerMask; public uint firstParameter; public uint parameterCount; public uint flags;",
        ),
        (
            "GpuPageRecord",
            "public GpuHandle parent; public GpuArenaRange dependencies; public uint64_t byteOffset; public uint byteLength; public uint residentGeneration; public uint flags; public uint reserved;",
        ),
        (
            "GpuDrawRecord",
            "public GpuHandle geometry; public GpuHandle material; public GpuHandle instance; public GpuHandle deformation; public uint contentIndex; public uint part; public uint representation; public uint sourceGeneration; public uint psoBin; public uint transition; public uint clusterState; public uint reserved;",
        ),
    ] {
        let marker = format!("public struct {record}");
        let declaration = source
            .split_once(&marker)
            .unwrap_or_else(|| panic!("missing `{marker}`"))
            .1;
        let body = declaration
            .split_once('{')
            .expect("Slang record opening brace")
            .1
            .split_once('}')
            .expect("Slang record closing brace")
            .0
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(body, expected_body, "Slang fields for {record}");
    }
    assert!(source.contains("gpuHandleMatches(GpuHandle handle"));
    assert!(source.contains("header.occupied != 0u && header.generation == handle.generation"));
    assert!(source.contains("composeGpuMaterialClass("));
    assert!(source.contains("composeGpuPsoBin("));
    for semantic in [
        "GPU_REPRESENTATION_TRIANGLE_CLUSTER = 0u",
        "GPU_REPRESENTATION_AGGREGATE_VOXEL = 1u",
        "GPU_DEFORMATION_RIGID = 0u",
        "GPU_DEFORMATION_COMMON_OUTPUT = 1u",
        "GPU_PASS_WIRE_DEBUG = 9u",
    ] {
        assert!(source.contains(semantic));
    }
}

#[test]
fn coverage_record_is_derived_from_canonical_metadata() {
    let metadata = CoverageMipMetadata {
        reference_cutoff: UnitInterval::from_bits(32_768),
        source_extent: [1024, 512],
        spatial_hash_salt: 0x0123_4567_89ab_cdef,
        classification: AlphaClassification::Masked,
        mip_hashes: vec![[7; 32]],
    };
    let record = GpuCoverageRecord::from_metadata(
        GpuHandle::INVALID,
        &CoverageSource::AlbedoAlpha,
        &metadata,
        OpacityMicromapDerivation::default(),
    );
    assert_eq!(record.classification, 1);
    assert_eq!(record.source_kind, 0);
    assert_eq!(record.hash_salt, [0x89ab_cdef, 0x0123_4567]);
    assert_eq!(record.source_extent, metadata.source_extent);
}

#[test]
fn packed_handle_round_trips() {
    let handle = GpuHandle {
        index: 0x89ab_cdef,
        generation: 0x0123_4567,
    };
    assert_eq!(GpuHandle::from_packed(handle.packed()), handle);
}

#[test]
fn arena_bda_byte_offsets_are_exact_and_checked() {
    assert_eq!(arena_byte_offset(0, 48).unwrap(), 0);
    assert_eq!(arena_byte_offset(1, 48).unwrap(), 48);
    assert_eq!(arena_byte_offset(0x0123_4567, 64).unwrap(), 0x48d1_59c0);
    assert!(arena_byte_offset(u32::MAX, u64::MAX).is_err());
}

#[test]
fn arena_growth_and_reclamation_preserve_live_addresses_and_generations() {
    let device = match Device::new(&crate::SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(error) => {
            eprintln!("skipping: no Vulkan device obtainable ({error})");
            return;
        }
    };
    let before = crate::validation_issue_count();
    let mut arena = GlobalGpuArena::<VertexArena>::new(&device, 16, 1).unwrap();
    let first = arena.allocate(8, 1).unwrap().0;
    let second = arena.allocate(8, 1).unwrap().0;
    assert_eq!((first.first, second.first), (0, 8));

    let original_buffer = arena.buffer();
    let original_address = arena.address(&device);
    let growth_count = u32::try_from(arena.capacity()).unwrap() + 1;
    let (_, needs_growth) = arena.allocate(growth_count, 1).unwrap();
    assert!(needs_growth);
    let growth = arena
        .prepare_growth(&device)
        .unwrap()
        .expect("the range exceeds the original arena");
    assert_eq!(growth.source, original_buffer);
    assert_eq!(growth.destination, arena.buffer());
    assert_eq!(growth.size, growth.source_size);
    assert!(growth.destination_size > growth.source_size);
    assert_eq!(arena.retired_allocation_count(), 1);
    if device.capabilities.buffer_device_address {
        assert_ne!(original_address, 0);
        assert_eq!(
            device.buffer_device_address(growth.source),
            original_address
        );
        assert_ne!(arena.address(&device), original_address);
    }

    arena.retire(first).unwrap();
    arena.retire(second).unwrap();
    let before_fences = arena.allocate(16, 1).unwrap().0;
    assert_ne!(before_fences.first, first.first);

    let mut handles = ImmutableGpuTable::default();
    let retired_handle = handles.insert(7_u32).unwrap();
    assert_eq!(handles.retire(retired_handle), Some(7));
    assert_ne!(handles.insert(8).unwrap().index, retired_handle.index);

    for frame_slot in 0..MAX_FRAMES_IN_FLIGHT {
        if frame_slot + 1 < MAX_FRAMES_IN_FLIGHT && device.capabilities.buffer_device_address {
            assert_eq!(
                device.buffer_device_address(growth.source),
                original_address
            );
        }
        arena.begin_frame(frame_slot).unwrap();
        handles.begin_frame(frame_slot).unwrap();
        let expected_retired = usize::from(frame_slot + 1 < MAX_FRAMES_IN_FLIGHT);
        assert_eq!(arena.retired_allocation_count(), expected_retired);
        if frame_slot + 1 < MAX_FRAMES_IN_FLIGHT {
            assert_ne!(handles.insert(9_u32).unwrap().index, retired_handle.index);
        }
    }

    let compacted = arena.allocate(16, 1).unwrap().0;
    assert_eq!(compacted.first, first.first);
    let reused_handle = handles.insert(10_u32).unwrap();
    assert_eq!(reused_handle.index, retired_handle.index);
    assert_ne!(reused_handle.generation, retired_handle.generation);
    assert!(handles.get(retired_handle).is_none());

    drop(arena);
    device.wait_idle().expect("idle before teardown");
    drop(device);
    assert_eq!(crate::validation_issue_count(), before);
}
