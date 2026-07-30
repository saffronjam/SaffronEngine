use super::*;

/// The representation a cut carries, and how the frame is asked to produce it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Representation {
    /// The refined cut: one record per triangle cluster of the leaf pages.
    TriangleCluster,
    /// The pinned-coarse cut: the root page's aggregate voxel surface as one record.
    AggregateVoxel,
    /// The reconstructed micro-blade field, appended to the stream after the walk.
    MicroBlade,
}

/// Every representation the cooker can put on the cut rasterizes through the *production*
/// depth pre-pass — the übershader's `vertexMainExecutor` over `depthPrepassFragment`,
/// bound with the two descriptor sets and the counted-indirect recorder every depth-family
/// pass uses. One scene and one frame's device state produce all three: the refined cut's
/// triangle clusters, the same hierarchy pinned coarse to its aggregate voxel surface, and
/// the micro-blade field reconstructed from a resident density tile.
///
/// The representation is read back from the record stream, so a run that silently produced
/// a different one fails on the record rather than passing on somebody else's coverage.
#[test]
fn the_depth_prepass_rasterizes_every_cooked_representation() {
    use crate::gpu_scene_upload::{
        GpuArenaUploadRequest, GpuScenePendingUploads, record_pending_global_uploads,
    };
    use crate::page_residency::{PageResidency, PageResidencyBudgets};
    use crate::{
        GlobalGpuTableKind, GpuFieldDirectoryEntry, GpuFieldTileRecord, GpuGeometryRecord,
        GpuPageRecord, GpuRepresentation,
    };

    // The micro tile: a 64x1x64 density grid over one 64 m base cell, dense in the four
    // texels of its corner, so the reconstructed blades occupy a 2 m square of a cell the
    // rest of the scene is nowhere near and the run's coverage is attributable to them.
    const FIELD_CELL_X: i64 = 1;
    const FIELD_DIMS: [u32; 3] = [64, 1, 64];
    const FIELD_TEXELS: [usize; 4] = [0, 1, 64, 65];

    let device = offscreen_device();
    let before = validation_issue_count();
    {
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("descriptors");
        let visibility = SceneVisibility::new(&device).expect("visibility");
        let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
        let cull = pipelines
            .request_scene_visibility(visibility.layout())
            .expect("cull pso");
        let traversal = pipelines
            .request_scene_traversal(visibility.traversal_layout())
            .expect("traversal pso");
        let bin_count = pipelines
            .request_scene_bin_count(visibility.bin_count_layout())
            .expect("bin count pso");
        let bin_scan = pipelines
            .request_scene_bin_seed(visibility.bin_seed_layout())
            .expect("bin scan pso");
        let bin_scatter = pipelines
            .request_scene_bin_scatter(visibility.bin_scatter_layout())
            .expect("bin scatter pso");
        let micro_count = pipelines
            .request_scene_micro_count(visibility.micro_layout())
            .expect("micro count pso");
        let micro_scan = pipelines
            .request_scene_micro_scan(visibility.micro_layout())
            .expect("micro scan pso");
        let micro_scatter = pipelines
            .request_scene_micro_scatter(visibility.micro_layout())
            .expect("micro scatter pso");
        let prepass = pipelines
            .request_depth_prepass_executor()
            .expect("depth prepass pso");

        let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
        let mut pending = GpuScenePendingUploads::default();
        let mut residency = PageResidency::new(PageResidencyBudgets::default());
        let mut gpu_scene =
            PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
        gpu_scene.create_world(WORLD).expect("world");

        // Real quad vertices in the global vertex arena for the BDA pull.
        let hierarchy = cooked_quad();
        let vertices: std::sync::Arc<[saffron_geometry::Vertex]> = {
            use saffron_geometry::Vertex;
            use saffron_geometry::glam::{Vec2, Vec3 as GVec3};
            let vert = |x: f32, y: f32| Vertex {
                position: GVec3::new(x, y, 0.0),
                normal: GVec3::Z,
                uv0: Vec2::new(x, y),
                ..Default::default()
            };
            std::sync::Arc::from([
                vert(0.0, 0.0),
                vert(1.0, 0.0),
                vert(0.0, 1.0),
                vert(1.0, 1.0),
            ])
        };
        let vertex_bytes = (vertices.len() * size_of::<saffron_geometry::Vertex>()) as u32;
        let (vertex_range, _) = gpu_data.vertices.allocate(vertex_bytes, 16).expect("verts");
        pending.upload_arena(GpuArenaUploadRequest::Vertices {
            range: vertex_range,
            data: std::sync::Arc::clone(&vertices),
        });
        let geometry = gpu_data
            .geometries
            .insert(GpuGeometryRecord {
                vertices: vertex_range,
                indices: crate::GpuArenaRange::default(),
                clusters: crate::GpuArenaRange::default(),
                parts: crate::GpuArenaRange::default(),
                voxels: crate::GpuArenaRange::default(),
                submeshes: crate::GpuArenaRange::default(),
                flags: 0,
                vertex_stride: size_of::<saffron_geometry::Vertex>() as u32,
                index_stride: 4,
                reserved: 0,
            })
            .expect("geometry");
        pending.stage_record(GlobalGpuTableKind::Geometry, geometry);

        // The shared blade-template index block: the corners every micro-blade draw
        // replays out of the pages arena.
        pending.upload_arena(GpuArenaUploadRequest::PageBytes {
            range: gpu_data.micro_blade_template,
            data: crate::micro_blade_template_indices()
                .iter()
                .flat_map(|index| index.to_le_bytes())
                .collect(),
        });

        // Every cooked page resident: register, load, publish (parents first). Both the
        // refined and the pinned-coarse cut then come from residency alone.
        let mut device_pages = Vec::new();
        for page in &hierarchy.pages {
            let parent = page
                .dependency
                .map(|dependency| device_pages[dependency as usize]);
            let handle = gpu_data
                .page_table
                .insert(GpuPageRecord {
                    parent: parent.unwrap_or(GpuHandle::INVALID),
                    dependencies: crate::GpuArenaRange::default(),
                    byte_offset: 0,
                    byte_length: 0,
                    resident_generation: 0,
                    flags: if page.guaranteed_root {
                        crate::GPU_PAGE_FLAG_GUARANTEED_ROOT
                    } else {
                        0
                    },
                    reserved: 0,
                })
                .expect("page record");
            pending.stage_record(GlobalGpuTableKind::Page, handle);
            residency.register_page(handle, parent, page.guaranteed_root);
            residency.demand(handle, 1);
            device_pages.push(handle);
        }
        let mut payloads = std::collections::HashMap::new();
        for page in &hierarchy.pages {
            let mut payload = crate::build_page_payload(&hierarchy, page.id).expect("payload");
            for (index, cook_child) in payload.child_pages.clone().iter().enumerate() {
                payload
                    .patch_child(index, device_pages[*cook_child as usize])
                    .expect("patch");
            }
            payloads.insert(device_pages[page.id as usize], payload.bytes);
        }
        for handle in residency.take_load_requests(64) {
            residency.complete_load(handle, payloads[&handle].clone());
        }
        residency
            .publish_ready(&mut gpu_data, &mut pending)
            .expect("publish all");

        let root_cook = hierarchy
            .pages
            .iter()
            .find(|page| page.guaranteed_root)
            .expect("root page")
            .id;

        // The material's parameter block: the depth pre-pass fragment reads it to decide
        // whether the surface needs a canonical-coverage sample, so it must be real
        // rather than whatever the arena held.
        let (parameters, _) = gpu_data
            .material_parameters
            .allocate(1, 1)
            .expect("material params");
        pending.upload_arena(GpuArenaUploadRequest::MaterialParams {
            range: parameters,
            data: Box::new(crate::MaterialParamsData::default()),
        });
        let material_class = crate::GpuMaterialClass::new(
            saffron_material::AlphaClassification::Masked,
            crate::GpuSidedness::Single,
            saffron_material::SurfaceModel::Standard,
            crate::GpuTransparency::Opaque,
            false,
        );
        let resident_material = gpu_data
            .materials
            .insert(crate::GpuMaterialTableRecord {
                base_color_texture: GpuHandle::INVALID,
                normal_texture: GpuHandle::INVALID,
                coverage: GpuHandle::INVALID,
                parameter_index: parameters.first,
                material_class,
                shader_index: 0,
                flags: 0,
                proxy_albedo: 0,
                occupancy: 1.0,
            })
            .expect("resident material");
        pending.stage_record(GlobalGpuTableKind::Material, resident_material);
        let scene_material = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                GpuSceneMaterialRecord {
                    table: resident_material,
                    source_revision: 1,
                },
            ))
            .expect("material")
        {
            GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let scene_root = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                table: device_pages[root_cook as usize],
                parent: None,
                source_generation: 1,
                flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
            }))
            .expect("scene page")
        {
            GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let prototype = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                GpuScenePrototypeRecord {
                    geometry,
                    materials: std::sync::Arc::from([scene_material]),
                    deformation: None,
                    sdfs: Vec::new().into(),
                    root_page: scene_root,
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
        let instance = create_instance(&mut gpu_scene, prototype, Vec3::ZERO);

        // The resident micro field: the tile's dense texels plus the one-entry directory the
        // micro pass dispatches over. The anchoring instance is the scene's own — blade
        // positions are world-space, so the field rides an identity placement.
        let sample_count = FIELD_DIMS[0] * FIELD_DIMS[1] * FIELD_DIMS[2];
        let mut tile_bytes = bytemuck::bytes_of(&GpuFieldTileRecord {
            cell: [FIELD_CELL_X, 0, 0],
            dims: FIELD_DIMS,
            sample_count,
            seed: [0x5eed_1234, 0x9e37_79b9, 0, 0],
            attribute_count: 0,
            reserved: 0,
        })
        .to_vec();
        let mut density = vec![0_u16; sample_count as usize];
        for texel in FIELD_TEXELS {
            density[texel] = u16::MAX;
        }
        tile_bytes.extend_from_slice(bytemuck::cast_slice(&density));
        let (tile_range, _) = gpu_data
            .fields
            .allocate(tile_bytes.len() as u32, 16)
            .expect("field tile");
        pending.upload_arena(GpuArenaUploadRequest::Fields {
            range: tile_range,
            data: tile_bytes,
        });
        let directory = [GpuFieldDirectoryEntry {
            instance,
            tile_offset: tile_range.first,
            predicted: 4,
        }];
        let (directory_range, _) = gpu_data
            .fields
            .allocate(size_of_val(&directory) as u32, 16)
            .expect("field directory");
        pending.upload_arena(GpuArenaUploadRequest::Fields {
            range: directory_range,
            data: bytemuck::cast_slice(&directory).to_vec(),
        });

        gpu_data.begin_frame(0).expect("gpu data");
        uploader.begin_frame(0).expect("uploader");
        gpu_scene.begin_frame(0).expect("scene");
        let mut graph = RenderGraph::new();
        record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
            .expect("drain pending");
        uploader
            .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
            .expect("record");
        one_shot(&device, |cmd| graph.execute(&device, cmd));

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
        let address_ubo = Buffer::new(
            device.resources(),
            size_of::<crate::GpuSceneAddressBlock>() as u64,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )
        .expect("address ubo");
        // SAFETY: HOST_VISIBLE + MAPPED, written before any submit that reads it.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(&block).as_ptr(),
                address_ubo.mapped_ptr(),
                size_of::<crate::GpuSceneAddressBlock>(),
            );
        }
        let address = (
            address_ubo.handle(),
            0,
            size_of::<crate::GpuSceneAddressBlock>() as u64,
        );

        let view = SceneVisibilityView::new(&device, &descriptors, &visibility, 64, 256, 2)
            .expect("view lists");
        let open = hzb_image(&device, 1.0);
        view.write_frame_bindings(&device, &visibility, 0, open.view(), open.view(), address);
        let instance_set = production_instance_set(&descriptors, &gpu_data, &view, 0, address);
        let wind_buffer = wind_records_buffer(&device);
        let pages_buffer = gpu_data.pages.buffer();
        let (buckets, table) = build_executor_buckets(&[(0, material_class.bits())], 256, false);
        view.write_bucket_table(0, &table);

        let mut coverage = Vec::new();
        for representation in [
            Representation::TriangleCluster,
            Representation::AggregateVoxel,
            Representation::MicroBlade,
        ] {
            // The blade run frames the field's corner of cell FIELD_CELL_X, which leaves the
            // quad outside the frustum: the cut is then empty and every drawn texel came
            // from the reconstructed field.
            let (eye, target) = if representation == Representation::MicroBlade {
                let centre = FIELD_CELL_X as f32 * 64.0 + 1.0;
                (Vec3::new(centre, 0.4, 3.0), Vec3::new(centre, 0.3, 1.0))
            } else {
                (Vec3::new(0.5, 0.5, 5.0), Vec3::new(0.5, 0.5, 0.0))
            };
            let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.05, 100.0)
                * Mat4::look_at_rh(eye, target, Vec3::Y);
            let cut = match representation {
                // Refine to the leaves: every child payload is resident, so a zero
                // threshold walks to the triangle clusters.
                Representation::TriangleCluster => SCENE_CUT_FORCE_FINE,
                Representation::AggregateVoxel => SCENE_CUT_FORCE_COARSE,
                Representation::MicroBlade => SCENE_CUT_AUTO,
            };

            let depth = depth_target(&device);
            let mut graph = RenderGraph::new();
            let hzb_res = graph.import_image(
                open.handle(),
                open.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::GENERAL,
                None,
            );
            let wind_records_res = graph.import_buffer(wind_buffer.handle(), None);
            view.add_cull_pass(
                &device,
                &mut graph,
                &cull,
                0,
                hzb_res,
                wind_records_res,
                64,
                SceneVisibilityPush {
                    view_proj: view_proj.to_cols_array(),
                    prev_view_proj: view_proj.to_cols_array(),
                    hzb_extent: [64, 64],
                    hzb_mip_count: 7,
                    pass_kind: SCENE_VISIBILITY_PASS_CULL,
                    history_valid: 0,
                    list_capacity: 64,
                    reserved: [0; 2],
                    reach_min: [0.0; 4],
                    reach_max: [0.0; 4],
                },
            );
            view.add_traversal_pass(
                &device,
                &mut graph,
                &traversal,
                0,
                SceneTraversalPush {
                    view_proj: view_proj.to_cols_array(),
                    eye: eye.to_array(),
                    proj_scale: 100_000.0,
                    error_threshold_px: 0.0,
                    record_capacity: 256,
                    list_capacity: 64,
                    survivor: 0,
                    displaced_records: 0,
                    transition_frames: 0,
                    frame_stamp: 0,
                    representation_override: cut,
                    node_cull: 1,
                    demand_only: 0,
                    view_class: SceneViewClass::Camera.ordinal(),
                },
            );
            if representation == Representation::MicroBlade {
                // The interaction field is a graph declaration only: the address block carries
                // a zero field address, which the sampler reads as no displacement.
                let interaction_res = graph.import_buffer(wind_buffer.handle(), None);
                view.add_micro_field_passes(
                    &device,
                    &mut graph,
                    (&micro_count, &micro_scan, &micro_scatter),
                    0,
                    gpu_data.micro_candidates.handle(),
                    interaction_res,
                    crate::SceneMicroFieldPush {
                        view_proj: view_proj.to_cols_array(),
                        eye: eye.to_array(),
                        max_distance: 96.0,
                        directory_offset: directory_range.first,
                        directory_count: 1,
                        record_capacity: 256,
                        candidate_capacity: crate::SCENE_MICRO_CANDIDATE_CAPACITY,
                        frame_base: 0,
                        reserved: [0; 3],
                        wind_dir_speed_gust: [1.0, 0.0, 0.0, 0.0],
                        wind_params: [0.0; 4],
                        wind_octaves: 0,
                        wind_seed: 0,
                        wind_time_current: 0.0,
                        wind_time_previous: 0.0,
                        wind_sources: 0,
                        wind_source_count: 0,
                        wind_reserved: 0,
                    },
                );
            }
            view.add_binning_passes(
                &device,
                &mut graph,
                (&bin_count, &bin_scan, &bin_scatter),
                0,
                false,
                (
                    crate::MICRO_BLADE_INDEX_COUNT,
                    gpu_data.micro_blade_template.first / 4,
                ),
                None,
            );
            add_depth_prepass(
                &device,
                &mut graph,
                "depth-prepass",
                &prepass,
                &depth,
                (descriptors.bindless_set(), instance_set),
                view_proj,
                view.executor_draw_inputs(0, 256, vk::Buffer::null()),
                pages_buffer,
                &buckets,
            );
            one_shot(&device, |cmd| graph.execute(&device, cmd));

            let counters = read_words(&device, view.counters(0), 24);
            let records = counters[SCENE_VISIBILITY_COUNTER_RECORDS] as usize;
            assert_eq!(
                counters[SCENE_VISIBILITY_COUNTER_RECORD_OVERFLOW], 0,
                "{representation:?}: no record or bucket pressure"
            );
            assert!(records > 0, "{representation:?}: the cut emits records");
            let record_words = read_words(
                &device,
                view.records(0),
                records * size_of::<crate::GpuDrawRecord>() / 4,
            );
            let emitted: &[crate::GpuDrawRecord] =
                bytemuck::cast_slice(bytemuck::cast_slice::<u32, u8>(&record_words));
            let expected = match representation {
                Representation::TriangleCluster => GpuRepresentation::TriangleCluster,
                Representation::AggregateVoxel => GpuRepresentation::AggregateVoxel,
                Representation::MicroBlade => GpuRepresentation::MicroBlade,
            } as u32;
            assert!(
                emitted
                    .iter()
                    .all(|record| record.representation == expected),
                "{representation:?}: every record on the cut carries the representation \
                 the run asked for (saw {:?})",
                emitted
                    .iter()
                    .map(|record| record.representation)
                    .collect::<Vec<_>>()
            );
            // The per-representation counter the same pass increments, exact by construction:
            // the coarse cut is one aggregate-voxel record, and every blade the field
            // reconstructed became a record.
            match representation {
                Representation::AggregateVoxel => assert_eq!(
                    counters[SCENE_VISIBILITY_COUNTER_VOXEL_RECORDS], 1,
                    "the pinned-coarse cut is exactly one aggregate-voxel record"
                ),
                Representation::MicroBlade => assert_eq!(
                    counters[SCENE_VISIBILITY_COUNTER_MICRO_CANDIDATES], records as u32,
                    "every reconstructed blade candidate became a record"
                ),
                Representation::TriangleCluster => {}
            }
            let written = count_written_depth(&device, &depth);
            coverage.push((representation, records, written));
            device.wait_idle().expect("idle after the run");
            drop(depth);
        }

        // Each representation reached the framebuffer through the one production PSO. The
        // quad fills the same screen patch whether the cut is refined or pinned coarse; the
        // reconstructed field is a handful of upright slivers, so its floor is smaller and
        // its ceiling is what separates grass from a wall.
        let [(_, _, cluster), (_, _, voxel), (_, blade_records, blades)] = coverage[..] else {
            panic!("one run per representation");
        };
        assert!(
            cluster > 100 && voxel > 100,
            "the refined and pinned-coarse cuts both cover the quad's screen patch \
             (clusters {cluster}, voxels {voxel})"
        );
        assert!(
            blades > 32 && blades * 4 < 64 * 64,
            "the reconstructed field's {blade_records} blades cover slivers, not the frame \
             ({blades} texels)"
        );

        device.wait_idle().expect("idle");
        descriptors.free_sets(&[instance_set]);
        drop(view);
        drop(open);
        drop(wind_buffer);
        drop(address_ubo);
        drop(cull);
        drop(traversal);
        drop(bin_count);
        drop(bin_scan);
        drop(bin_scatter);
        drop(micro_count);
        drop(micro_scan);
        drop(micro_scatter);
        drop(prepass);
        drop(pipelines);
        drop(residency);
        drop(gpu_scene);
        drop(uploader);
        drop(gpu_data);
        drop(visibility);
        drop(descriptors);
    }
    device.wait_idle().expect("idle before teardown");
    drop(device);
    assert_eq!(validation_issue_count(), before);
}
