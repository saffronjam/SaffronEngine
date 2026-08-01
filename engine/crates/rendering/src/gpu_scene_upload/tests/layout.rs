use super::*;

#[test]
fn shader_resolves_the_scene_chain_through_buffer_addresses() {
    use crate::compute_dispatch::{ComputeBuffer, run_compute};
    let Some(device) = device_or_skip() else {
        return;
    };
    let device = std::sync::Arc::new(device);
    let before = validation_issue_count();
    {
        let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
        let mut gpu_scene =
            PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
        gpu_scene.create_world(WORLD).expect("world");

        let material = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                GpuSceneMaterialRecord {
                    table: device_handle(7),
                    source_revision: 0x1_0000_002B,
                },
            ))
            .expect("material")
        {
            GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let page = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                table: device_handle(9),
                parent: None,
                source_generation: 3,
                flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
            }))
            .expect("page")
        {
            GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let prototype = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                crate::GpuScenePrototypeRecord {
                    geometry: device_handle(20),
                    materials: std::sync::Arc::from([material]),
                    deformation: None,
                    sdfs: Vec::new().into(),
                    root_page: page,
                    page_bounds: std::sync::Arc::from([]),
                    bounds: [1.0, 2.0, 3.0, 4.5],
                    source_generation: 5,
                    flags: 0,
                    mechanics: [0; 4],
                },
            ))
            .expect("prototype")
        {
            GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let current = Mat4::from_translation(Vec3::new(1.5, 2.5, 3.5));
        let previous = Mat4::from_translation(Vec3::new(4.0, 5.0, 6.0));
        let transform = GpuSceneDynamicTransform::new(current, previous).expect("transform");
        let instance = match gpu_scene
            .apply_world_delta(
                WORLD,
                GpuSceneWorldDelta::CreateInstance(crate::GpuSceneInstanceRecord {
                    prototype,
                    transform: GpuSceneTransform::Dynamic(transform),
                    material_overrides: std::sync::Arc::from([GpuSceneMaterialOverride {
                        slot: 0,
                        material,
                    }]),
                    deformation: None,
                    source_generation: 6,
                    flags: 0,
                    combination: 0,
                    vegetation: None,
                }),
            )
            .expect("instance")
        {
            GpuSceneWorldDeltaResult::InstanceCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        gpu_scene
            .apply_world_delta(
                WORLD,
                GpuSceneWorldDelta::CreateLight(GpuSceneLightRecord {
                    light: GpuLight {
                        position_range: Vec4::new(1.0, 2.0, 3.0, 4.0),
                        color_intensity: Vec4::new(0.5, 0.25, 0.125, 8.5),
                        direction_type: Vec4::ZERO,
                        spot_cos: Vec4::new(0.0, 0.0, 1.0, 0.0),
                    },
                    source_revision: 21,
                }),
            )
            .expect("light");

        gpu_data.begin_frame(0).expect("gpu data begin");
        uploader.begin_frame(0).expect("uploader begin");
        gpu_scene.begin_frame(0).expect("scene begin");
        let mut graph = RenderGraph::new();
        uploader
            .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
            .expect("record");
        run_graph(&device, &mut graph);

        let block = uploader.build_address_block(
            &device,
            &gpu_data,
            WORLD,
            0,
            (0, 0),
            0,
            0,
            0,
            crate::DisplacedFrameAddresses::default(),
            (0, 0),
            0,
        );
        assert!(block.instances != 0 && block.scene_prototypes != 0);
        let outputs = run_compute(
            std::sync::Arc::clone(&device),
            "gpu_scene_test",
            vec![
                ComputeBuffer::zeroed(24 * size_of::<u32>()),
                ComputeBuffer::from_bytes(bytemuck::bytes_of(&block).to_vec()),
            ],
            [1, 1, 1],
        )
        .expect("dispatch");
        let words: Vec<u32> = outputs[0]
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();

        assert_eq!(
            words[0],
            instance.raw().generation,
            "instance header generation"
        );
        assert_eq!(words[1], 1, "instance slot occupied");
        assert_eq!(words[2], prototype.raw().index);
        assert_eq!(words[3], prototype.raw().generation);
        assert_eq!(words[4], GPU_SCENE_TRANSFORM_DYNAMIC);
        let cols = current.to_cols_array();
        assert_eq!(words[5], cols[0].to_bits());
        assert_eq!(words[6], cols[1].to_bits());
        assert_eq!(words[7], cols[2].to_bits());
        assert_eq!(words[8], previous.to_cols_array()[15].to_bits());
        assert_eq!(words[9], 1, "one override element");
        assert_eq!(words[10], 20, "prototype geometry index");
        assert_eq!(words[11], 1, "prototype geometry generation");
        assert_eq!(words[12], 4.5_f32.to_bits(), "prototype bounds radius");
        assert_eq!(words[13], 1, "one prototype material element");
        assert_eq!(words[14], material.raw().index);
        assert_eq!(words[15], material.raw().generation);
        assert_eq!(words[16], 7, "material target index");
        assert_eq!(words[17], 1, "material target generation");
        assert_eq!(words[18], 0x2B, "material revision low word");
        assert_eq!(words[19], 0x1, "material revision high word");
        assert_eq!(words[20], 0, "override slot");
        assert_eq!(words[21], material.raw().index, "override material");
        assert_eq!(words[22], 8.5_f32.to_bits(), "light intensity");
        assert_eq!(words[23], 21, "light revision low word");

        device.wait_idle().expect("idle");
        drop(gpu_scene);
        drop(uploader);
        drop(gpu_data);
    }
    drop(device);
    assert_eq!(validation_issue_count(), before);
}

#[test]
fn resident_table_strides_lock_the_slang_pointer_constants() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
    assert_eq!(
        gpu_data.geometries.slot_stride(),
        80,
        "GPU_GEOMETRY_TABLE_STRIDE"
    );
    assert_eq!(
        gpu_data.materials.slot_stride(),
        64,
        "GPU_MATERIAL_TABLE_STRIDE"
    );
    assert_eq!(
        gpu_data.textures.slot_stride(),
        48,
        "GPU_TEXTURE_TABLE_STRIDE"
    );
    assert_eq!(
        gpu_data.coverage.slot_stride(),
        64,
        "GPU_COVERAGE_TABLE_STRIDE"
    );
    assert_eq!(
        gpu_data.page_table.slot_stride(),
        64,
        "GPU_PAGE_TABLE_STRIDE"
    );
    assert_eq!(gpu_data.sdfs.slot_stride(), 96, "GPU_SDF_TABLE_STRIDE");
    drop(gpu_data);
    drop(device);
}

#[test]
fn ray_candidate_classification_matches_the_cpu_classifier() {
    use crate::compute_dispatch::{ComputeBuffer, run_compute};
    use crate::{
        CoverageSourceKind, GpuCoverageRecord, GpuMaterialClass, GpuMaterialTableRecord,
        GpuSidedness, GpuSubmeshRecord, GpuTransparency, classify_canonical_coverage,
    };
    use saffron_geometry::glam::Vec2;
    use saffron_material::{AlphaClassification, SurfaceModel};

    let Some(device) = device_or_skip() else {
        return;
    };
    let device = std::sync::Arc::new(device);
    let before = validation_issue_count();
    {
        let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
        let mut pending = GpuScenePendingUploads::default();
        let mut gpu_scene =
            PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
        gpu_scene.create_world(WORLD).expect("world");

        // Resident geometry: a unit quad (two triangles, one submesh each) with
        // binary-exact positions/UVs so GPU interpolation matches CPU f32 arithmetic
        // bit-for-bit.
        let vert = |x: f32, y: f32| saffron_geometry::Vertex {
            position: Vec3::new(x, y, 0.0),
            uv0: Vec2::new(x, y),
            ..Default::default()
        };
        let vertices: Arc<[saffron_geometry::Vertex]> = Arc::from([
            vert(0.0, 0.0),
            vert(1.0, 0.0),
            vert(0.0, 1.0),
            vert(1.0, 1.0),
        ]);
        let indices: Arc<[u32]> = Arc::from([0_u32, 1, 2, 1, 3, 2]);
        let vertex_bytes = (vertices.len() * size_of::<saffron_geometry::Vertex>()) as u32;
        let (vertex_range, _) = gpu_data.vertices.allocate(vertex_bytes, 16).expect("verts");
        let (index_range, _) = gpu_data
            .indices
            .allocate((indices.len() * 4) as u32, 4)
            .expect("indices");
        let (submesh_range, _) = gpu_data.submesh_table.allocate(2, 1).expect("submeshes");
        let geometry = gpu_data
            .geometries
            .insert(crate::GpuGeometryRecord {
                vertices: vertex_range,
                indices: index_range,
                clusters: GpuArenaRange::default(),
                parts: GpuArenaRange::default(),
                voxels: GpuArenaRange::default(),
                submeshes: submesh_range,
                flags: 0,
                vertex_stride: size_of::<saffron_geometry::Vertex>() as u32,
                index_stride: 4,
                reserved: 0,
            })
            .expect("geometry");
        pending.stage_record(GlobalGpuTableKind::Geometry, geometry);
        pending.upload_arena(GpuArenaUploadRequest::Vertices {
            range: vertex_range,
            data: Arc::clone(&vertices),
        });
        pending.upload_arena(GpuArenaUploadRequest::Indices {
            range: index_range,
            data: Arc::clone(&indices),
        });
        pending.upload_arena(GpuArenaUploadRequest::Submeshes {
            range: submesh_range,
            data: vec![
                GpuSubmeshRecord {
                    first_index: 0,
                    index_count: 3,
                    material_slot: 0,
                    reserved: 0,
                },
                GpuSubmeshRecord {
                    first_index: 3,
                    index_count: 3,
                    material_slot: 1,
                    reserved: 0,
                },
            ],
        });

        // Two resident materials: a masked albedo-alpha default (slot 0) and a
        // thin-sheet canonical-probability override target (for slot 1).
        const MASKED_SALT: [u32; 2] = [0x1234_5678, 0x9abc_def0];
        const CANONICAL_SALT: [u32; 2] = [7, 9];
        let cov_masked = gpu_data
            .coverage
            .insert(GpuCoverageRecord {
                texture: GpuHandle::INVALID,
                cutoff: 0.5,
                classification: AlphaClassification::Masked as u32,
                source_kind: CoverageSourceKind::AlbedoAlpha as u32,
                omm_policy: 0,
                hash_salt: MASKED_SALT,
                source_extent: [8, 8],
                omm_thresholds: 0,
                reserved: 0,
            })
            .expect("masked coverage");
        pending.stage_record(GlobalGpuTableKind::Coverage, cov_masked);
        let cov_canonical = gpu_data
            .coverage
            .insert(GpuCoverageRecord {
                texture: GpuHandle::INVALID,
                cutoff: 0.25,
                classification: AlphaClassification::Masked as u32,
                source_kind: CoverageSourceKind::Texture as u32,
                omm_policy: 0,
                hash_salt: CANONICAL_SALT,
                source_extent: [16, 16],
                omm_thresholds: 0,
                reserved: 0,
            })
            .expect("canonical coverage");
        pending.stage_record(GlobalGpuTableKind::Coverage, cov_canonical);

        let (params_masked, _) = gpu_data
            .material_parameters
            .allocate(1, 1)
            .expect("masked params");
        let mut masked_block = MaterialParamsData::zeroed();
        masked_block.base_color = Vec4::new(1.0, 1.0, 1.0, 0.75);
        pending.upload_arena(GpuArenaUploadRequest::MaterialParams {
            range: params_masked,
            data: Box::new(masked_block),
        });
        let (params_canonical, _) = gpu_data
            .material_parameters
            .allocate(1, 1)
            .expect("canonical params");
        let mut canonical_block = MaterialParamsData::zeroed();
        canonical_block.base_color = Vec4::new(1.0, 1.0, 1.0, 0.5);
        pending.upload_arena(GpuArenaUploadRequest::MaterialParams {
            range: params_canonical,
            data: Box::new(canonical_block),
        });

        let mat_masked = gpu_data
            .materials
            .insert(GpuMaterialTableRecord {
                base_color_texture: GpuHandle::INVALID,
                normal_texture: GpuHandle::INVALID,
                coverage: cov_masked,
                parameter_index: params_masked.first,
                material_class: GpuMaterialClass::new(
                    AlphaClassification::Masked,
                    GpuSidedness::Single,
                    SurfaceModel::Standard,
                    GpuTransparency::Opaque,
                    false,
                ),
                shader_index: 0,
                flags: 0,
                proxy_albedo: 0,
                occupancy: 1.0,
            })
            .expect("masked material");
        pending.stage_record(GlobalGpuTableKind::Material, mat_masked);
        let mat_canonical = gpu_data
            .materials
            .insert(GpuMaterialTableRecord {
                base_color_texture: GpuHandle::INVALID,
                normal_texture: GpuHandle::INVALID,
                coverage: cov_canonical,
                parameter_index: params_canonical.first,
                material_class: GpuMaterialClass::new(
                    AlphaClassification::Masked,
                    GpuSidedness::Double,
                    SurfaceModel::ThinSheetFoliage,
                    GpuTransparency::Opaque,
                    false,
                ),
                shader_index: 0,
                flags: 0,
                proxy_albedo: 0,
                occupancy: 1.0,
            })
            .expect("canonical material");
        pending.stage_record(GlobalGpuTableKind::Material, mat_canonical);

        // Scene chain: two prototype default material references plus a per-instance
        // override on slot 1, so the candidate resolve exercises both lookups.
        let scene_material = |scene: &mut PersistentGpuScene, table, revision| match scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                GpuSceneMaterialRecord {
                    table,
                    source_revision: revision,
                },
            ))
            .expect("scene material")
        {
            GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let scene_mat_default = scene_material(&mut gpu_scene, mat_masked, 1);
        let scene_mat_shadowed = scene_material(&mut gpu_scene, mat_masked, 2);
        let scene_mat_override = scene_material(&mut gpu_scene, mat_canonical, 3);
        let page = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                table: device_handle(9),
                parent: None,
                source_generation: 1,
                flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
            }))
            .expect("page")
        {
            GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let prototype = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                crate::GpuScenePrototypeRecord {
                    geometry,
                    materials: std::sync::Arc::from([scene_mat_default, scene_mat_shadowed]),
                    deformation: None,
                    sdfs: Vec::new().into(),
                    root_page: page,
                    page_bounds: std::sync::Arc::from([]),
                    bounds: [0.5, 0.5, 0.0, 1.0],
                    source_generation: 1,
                    flags: 0,
                    mechanics: [0; 4],
                },
            ))
            .expect("prototype")
        {
            GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let transform =
            GpuSceneDynamicTransform::new(Mat4::IDENTITY, Mat4::IDENTITY).expect("transform");
        let instance = match gpu_scene
            .apply_world_delta(
                WORLD,
                GpuSceneWorldDelta::CreateInstance(crate::GpuSceneInstanceRecord {
                    prototype,
                    transform: GpuSceneTransform::Dynamic(transform),
                    material_overrides: std::sync::Arc::from([GpuSceneMaterialOverride {
                        slot: 1,
                        material: scene_mat_override,
                    }]),
                    deformation: None,
                    source_generation: 1,
                    flags: 0,
                    combination: 0,
                    vegetation: None,
                }),
            )
            .expect("instance")
        {
            GpuSceneWorldDeltaResult::InstanceCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };

        gpu_data.begin_frame(0).expect("gpu data begin");
        uploader.begin_frame(0).expect("uploader begin");
        gpu_scene.begin_frame(0).expect("scene begin");
        let mut graph = RenderGraph::new();
        record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
            .expect("drain pending");
        uploader
            .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
            .expect("record");
        run_graph(&device, &mut graph);

        // A cluster-composed structure resolves through its own tables: the candidate's geometry
        // index is a cluster ordinal and its primitive is cluster-local, so neither addresses the
        // shared index stream. The two clusters here sit in the OPPOSITE order to the submeshes
        // they came from, which is what the cooked cache-optimized cluster order looks like and
        // what a resolver reading the submesh slice directly gets wrong.
        let cluster_records = [
            crate::ClusterResolutionRecord {
                submesh_element: 1,
                first_corner: 0,
                triangle_count: 1,
                reserved: 0,
            },
            crate::ClusterResolutionRecord {
                submesh_element: 0,
                first_corner: 3,
                triangle_count: 1,
                reserved: 0,
            },
        ];
        let cluster_corners: [u32; 6] = [1, 3, 2, 0, 1, 2];
        let device_buffer = |bytes: &[u8]| {
            crate::Buffer::from_slice_with_usage(
                device.resources(),
                bytes,
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
            )
            .expect("device buffer")
        };
        let cluster_record_buffer = device_buffer(bytemuck::cast_slice(&cluster_records));
        let cluster_corner_buffer = device_buffer(bytemuck::cast_slice(&cluster_corners));

        // The per-frame identity table the packer writes and `gpuSceneRayInstance` reads back.
        let ray_instances = [
            crate::GpuRayInstanceRecord {
                instance_slot: instance.raw().index,
                first_submesh: 0,
                cluster_count: 0,
                reserved: 0,
                cluster_records: 0,
                cluster_corners: 0,
            },
            crate::GpuRayInstanceRecord {
                instance_slot: instance.raw().index,
                first_submesh: 1,
                cluster_count: 0,
                reserved: 0,
                cluster_records: 0,
                cluster_corners: 0,
            },
            crate::GpuRayInstanceRecord {
                instance_slot: crate::RT_UNMIRRORED_INSTANCE,
                first_submesh: 0,
                cluster_count: 0,
                reserved: 0,
                cluster_records: 0,
                cluster_corners: 0,
            },
            crate::GpuRayInstanceRecord {
                instance_slot: instance.raw().index,
                first_submesh: 0,
                cluster_count: 2,
                reserved: 0,
                cluster_records: device.buffer_device_address(cluster_record_buffer.handle()),
                cluster_corners: device.buffer_device_address(cluster_corner_buffer.handle()),
            },
        ];
        let ray_instance_buffer = device_buffer(bytemuck::cast_slice(&ray_instances));
        let ray_instance_addresses = (
            device.buffer_device_address(ray_instance_buffer.handle()),
            ray_instances.len() as u32,
        );

        const PHASE: u32 = 5;
        let block = uploader.build_address_block(
            &device,
            &gpu_data,
            WORLD,
            0,
            (0, 0),
            0,
            0,
            0,
            crate::DisplacedFrameAddresses::default(),
            ray_instance_addresses,
            PHASE,
        );

        // (ray instance, geometry index, primitive, sampled alpha, barycentrics). Each candidate
        // names its identity by the `instanceCustomIndex` the packer wrote, which is a row of the
        // table above; a triangle build then names its submesh by `firstSubmesh + geometryIndex`
        // and its primitive within that submesh's own index slice.
        let cases: [(u32, u32, u32, f32, [f32; 2]); 10] = [
            (0, 0, 0, 0.625, [0.25, 0.5]),
            (0, 1, 0, 0.375, [0.5, 0.25]),
            (2, 0, 0, 0.5, [0.25, 0.25]),
            (0, 0, 99, 0.5, [0.25, 0.25]),
            (1, 0, 0, 0.375, [0.5, 0.25]),
            (1, 1, 0, 0.5, [0.25, 0.25]),
            (4, 0, 0, 0.5, [0.25, 0.25]),
            (3, 0, 0, 0.375, [0.5, 0.25]),
            (3, 1, 0, 0.625, [0.25, 0.5]),
            (3, 2, 0, 0.5, [0.25, 0.25]),
        ];
        let mut case_bytes = Vec::with_capacity(cases.len() * 32);
        for (ray_instance, geometry, primitive, sampled, barycentrics) in cases {
            for word in [
                ray_instance,
                primitive,
                sampled.to_bits(),
                geometry,
                barycentrics[0].to_bits(),
                barycentrics[1].to_bits(),
                0,
                0,
            ] {
                case_bytes.extend_from_slice(&word.to_le_bytes());
            }
        }
        const WORDS: usize = 20;
        let outputs = run_compute(
            std::sync::Arc::clone(&device),
            "gpu_scene_candidate_test",
            vec![
                ComputeBuffer::zeroed(cases.len() * WORDS * size_of::<u32>()),
                ComputeBuffer::from_bytes(bytemuck::bytes_of(&block).to_vec()),
                ComputeBuffer::from_bytes(case_bytes),
            ],
            [cases.len() as u32, 1, 1],
        )
        .expect("dispatch");
        let words: Vec<u32> = outputs[0]
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        let case = |index: usize| &words[index * WORDS..(index + 1) * WORDS];

        // Case 0: primitive 0 (v0/v1/v2), weights (0.25, 0.25, 0.5) → uv/anchor
        // (0.25, 0.5); the slot-0 default masked albedo-alpha material.
        let masked_salt = u64::from(MASKED_SALT[0]) | (u64::from(MASKED_SALT[1]) << 32);
        let expected = classify_canonical_coverage(
            0.625,
            [0.25, 0.5],
            [0.25, 0.5, 0.0],
            CoverageSourceKind::AlbedoAlpha,
            AlphaClassification::Masked,
            0.75,
            [8, 8],
            masked_salt,
            PHASE,
            0.5,
            0.0,
            false,
            false,
        );
        let case0 = case(0);
        assert_eq!(case0[16], instance.raw().index, "case 0 reads its identity");
        assert_eq!(case0[17], 0, "case 0 span start");
        assert_eq!(case0[18], 0, "case 0 is a triangle build");
        assert_eq!(case0[0], 1, "case 0 resolves");
        assert_eq!(case0[1], 0.25_f32.to_bits(), "case 0 uv.x");
        assert_eq!(case0[2], 0.5_f32.to_bits(), "case 0 uv.y");
        assert_eq!(case0[3], 0.25_f32.to_bits(), "case 0 anchor.x");
        assert_eq!(case0[4], 0.5_f32.to_bits(), "case 0 anchor.y");
        assert_eq!(case0[5], 0.0_f32.to_bits(), "case 0 anchor.z");
        assert_eq!(case0[6], CoverageSourceKind::AlbedoAlpha as u32);
        assert_eq!(case0[7], AlphaClassification::Masked as u32);
        assert_eq!(case0[8], 8, "case 0 extent.x");
        assert_eq!(case0[9], 8, "case 0 extent.y");
        assert_eq!(case0[10], 0, "case 0 standard surface model");
        assert_eq!(case0[11], 0.75_f32.to_bits(), "case 0 baseColorAlpha");
        assert_eq!(case0[12], expected.alpha.to_bits(), "case 0 alpha");
        assert_eq!(case0[13], u32::from(expected.covered), "case 0 covered");
        assert_eq!(case0[14], 0.5_f32.to_bits(), "case 0 cutoff");
        assert_eq!(case0[15], MASKED_SALT[0], "case 0 salt low word");

        // Case 1: geometry 1, primitive 0 (v1/v3/v2), weights (0.25, 0.5, 0.25) → uv/anchor
        // (0.75, 0.75); the slot-1 override thin-sheet canonical material.
        let canonical_salt = u64::from(CANONICAL_SALT[0]) | (u64::from(CANONICAL_SALT[1]) << 32);
        let expected = classify_canonical_coverage(
            0.375,
            [0.75, 0.75],
            [0.75, 0.75, 0.0],
            CoverageSourceKind::Texture,
            AlphaClassification::Masked,
            0.5,
            [16, 16],
            canonical_salt,
            PHASE,
            0.25,
            0.0,
            true,
            false,
        );
        let case1 = case(1);
        assert_eq!(case1[0], 1, "case 1 resolves");
        assert_eq!(case1[1], 0.75_f32.to_bits(), "case 1 uv.x");
        assert_eq!(case1[2], 0.75_f32.to_bits(), "case 1 uv.y");
        assert_eq!(case1[3], 0.75_f32.to_bits(), "case 1 anchor.x");
        assert_eq!(case1[4], 0.75_f32.to_bits(), "case 1 anchor.y");
        assert_eq!(case1[6], CoverageSourceKind::Texture as u32);
        assert_eq!(case1[8], 16, "case 1 extent.x");
        assert_eq!(case1[10], 1, "case 1 canonical probability");
        assert_eq!(case1[11], 0.5_f32.to_bits(), "case 1 baseColorAlpha");
        assert_eq!(case1[12], expected.alpha.to_bits(), "case 1 alpha");
        assert_eq!(case1[13], u32::from(expected.covered), "case 1 covered");
        assert_eq!(case1[14], 0.25_f32.to_bits(), "case 1 cutoff");
        assert_eq!(case1[15], CANONICAL_SALT[0], "case 1 salt low word");

        // These do not resolve; the ray verdict falls back to covered.
        for (index, label) in [
            (2, "unmirrored identity"),
            (3, "out-of-range primitive"),
            (5, "span start past the submesh table"),
            (6, "ray instance past the table bound"),
            (9, "geometry index past the cluster count"),
        ] {
            let row = case(index);
            assert_eq!(row[0], 0, "{label} stays unresolved");
            assert_eq!(row[12], 1.0_f32.to_bits(), "{label} alpha");
            assert_eq!(row[13], 1, "{label} counts as covered");
        }
        // The two unresolved-identity cases fail for different reasons, and the identity words
        // separate them: row 2 is a mirrored table read of the unmirrored sentinel, row 6 is
        // past `rayInstanceCount` and never read at all — both surface the sentinel, which is
        // exactly why the resolver must treat the sentinel as unresolvable.
        assert_eq!(
            case(2)[16],
            crate::RT_UNMIRRORED_INSTANCE,
            "the unmirrored sentinel comes back from the table"
        );
        assert_eq!(
            case(6)[16],
            crate::RT_UNMIRRORED_INSTANCE,
            "an index at the table bound reads as unmirrored rather than out of bounds"
        );

        // Case 4: geometry index 0 rebased by a span start of one resolves the SAME submesh
        // case 1 reached through geometry index 1 — the assembly-prototype path, where a
        // structure's geometry 0 is its span's first submesh rather than the family's. A
        // resolver that ignored the span start would answer with case 0's default material
        // instead, so this pins the rebase rather than the arithmetic around it.
        let case4 = case(4);
        assert_eq!(case4[0], 1, "case 4 resolves");
        assert_eq!(case4[17], 1, "case 4's identity carries the span start");
        assert_eq!(
            case4[1], case1[1],
            "case 4 uv.x matches the rebased submesh"
        );
        assert_eq!(case4[6], CoverageSourceKind::Texture as u32);
        assert_eq!(case4[10], 1, "case 4 canonical probability");
        assert_eq!(case4[14], 0.25_f32.to_bits(), "case 4 cutoff");
        assert_eq!(case4[15], CANONICAL_SALT[0], "case 4 salt low word");

        // Cases 7 and 8: the cluster-composed structure. Cluster ordinal 0 carries submesh 1's
        // triangle and ordinal 1 carries submesh 0's, so a resolver that read `firstSubmesh +
        // geometryIndex` and the submesh's own index slice — which is what a triangle build
        // means — would answer each with the other's material and UV.
        let case7 = case(7);
        assert_eq!(case7[18], 2, "case 7's identity is cluster-composed");
        assert_eq!(case7[19], 1, "case 7 carries a corner stream");
        assert_eq!(case7[0], 1, "case 7 resolves");
        assert_eq!(
            case7[1], case1[1],
            "cluster 0 lands on submesh 1's triangle"
        );
        assert_eq!(case7[2], case1[2], "cluster 0 uv.y");
        assert_eq!(
            case7[6],
            CoverageSourceKind::Texture as u32,
            "cluster 0 reads submesh 1's overridden material"
        );
        assert_eq!(case7[14], 0.25_f32.to_bits(), "cluster 0 cutoff");
        let case8 = case(8);
        assert_eq!(case8[0], 1, "case 8 resolves");
        assert_eq!(
            case8[1], case0[1],
            "cluster 1 lands on submesh 0's triangle"
        );
        assert_eq!(case8[2], case0[2], "cluster 1 uv.y");
        assert_eq!(
            case8[6],
            CoverageSourceKind::AlbedoAlpha as u32,
            "cluster 1 reads submesh 0's default material"
        );
        assert_eq!(case8[14], 0.5_f32.to_bits(), "cluster 1 cutoff");

        device.wait_idle().expect("idle");
        drop(ray_instance_buffer);
        drop(cluster_corner_buffer);
        drop(cluster_record_buffer);
        drop(gpu_scene);
        drop(uploader);
        drop(gpu_data);
    }
    drop(device);
    assert_eq!(validation_issue_count(), before);
}
