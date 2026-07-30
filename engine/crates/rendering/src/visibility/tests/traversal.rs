use super::*;

#[test]
fn traversal_emits_cut_records_and_requests_missing_children() {
    use crate::gpu_scene_upload::{GpuScenePendingUploads, record_pending_global_uploads};
    use crate::page_residency::{PageResidency, PageResidencyBudgets};
    use crate::{GlobalGpuTableKind, GpuMaterialTableRecord, GpuPageRecord, GpuRepresentation};

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

        let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
        let mut pending = GpuScenePendingUploads::default();
        let mut residency = PageResidency::new(PageResidencyBudgets::default());
        let mut gpu_scene =
            PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
        gpu_scene.create_world(WORLD).expect("world");

        // Resident material (for the psoBin material class) and the cooked page
        // hierarchy's resident page records.
        let resident_material = gpu_data
            .materials
            .insert(GpuMaterialTableRecord {
                base_color_texture: GpuHandle::INVALID,
                normal_texture: GpuHandle::INVALID,
                coverage: GpuHandle::INVALID,
                parameter_index: 0,
                material_class: crate::GpuMaterialClass::new(
                    saffron_material::AlphaClassification::Masked,
                    crate::GpuSidedness::Single,
                    saffron_material::SurfaceModel::Standard,
                    crate::GpuTransparency::Opaque,
                    false,
                ),
                shader_index: 0,
                flags: 0,
                proxy_albedo: 0,
                occupancy: 1.0,
            })
            .expect("resident material");
        pending.stage_record(GlobalGpuTableKind::Material, resident_material);

        let hierarchy = cooked_quad();
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
            device_pages.push(handle);
        }
        let root_cook = hierarchy
            .pages
            .iter()
            .find(|page| page.guaranteed_root)
            .expect("root page")
            .id;
        let root_handle = device_pages[root_cook as usize];

        // Publish ONLY the root payload; its children stay unresident.
        let mut payload = crate::build_page_payload(&hierarchy, root_cook).expect("root payload");
        for (index, cook_child) in payload.child_pages.clone().iter().enumerate() {
            payload
                .patch_child(index, device_pages[*cook_child as usize])
                .expect("patch");
        }
        for handle in residency.take_load_requests(16) {
            if handle == root_handle {
                residency.complete_load(handle, payload.bytes.clone());
            }
        }
        residency
            .publish_ready(&mut gpu_data, &mut pending)
            .expect("publish root");

        // Scene chain: material -> prototype (root page) -> one instance at origin.
        let material = match gpu_scene
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
                table: root_handle,
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
                    geometry: GpuHandle {
                        index: 3,
                        generation: 1,
                    },
                    materials: std::sync::Arc::from([material]),
                    deformation: None,
                    sdfs: Vec::new().into(),
                    root_page: scene_root,
                    page_bounds: std::sync::Arc::from([]),
                    bounds: [0.0, 0.0, 0.0, 1.0],
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
        let front = create_instance(&mut gpu_scene, prototype, Vec3::ZERO);

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
        let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0)
            * Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        view.write_frame_bindings(&device, &visibility, 0, open.view(), open.view(), address);

        // Cull + traverse with a zero threshold: the root wants to refine, its
        // children are missing, so it emits itself and requests every child.
        let mut graph = RenderGraph::new();
        let hzb_res = graph.import_image(
            open.handle(),
            open.view(),
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::GENERAL,
            None,
        );
        let wind_buffer = wind_records_buffer(&device);
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
                eye: [0.0, 0.0, 5.0],
                proj_scale: 1000.0,
                error_threshold_px: 0.0,
                record_capacity: 256,
                list_capacity: 64,
                survivor: 0,
                displaced_records: 0,
                transition_frames: 0,
                frame_stamp: 0,
                representation_override: SCENE_CUT_AUTO,
                node_cull: 1,
                demand_only: 0,
                view_class: SceneViewClass::Camera.ordinal(),
            },
        );
        one_shot(&device, |cmd| graph.execute(&device, cmd));

        let counters = read_words(&device, view.counters(0), 8);
        assert_eq!(counters[0], 1, "the instance is visible");
        assert_eq!(counters[3], 1, "the resident root emits one record");
        assert_eq!(counters[4], 0, "no record overflow");
        let record_words = read_words(
            &device,
            view.records(0),
            size_of::<crate::GpuDrawRecord>() / 4,
        );
        let record: crate::GpuDrawRecord =
            *bytemuck::from_bytes(bytemuck::cast_slice(&record_words));
        assert_eq!(record.content_index, root_handle.index);
        assert_eq!(
            record.representation,
            GpuRepresentation::AggregateVoxel as u32,
            "the cooked root is the aggregate voxel node"
        );
        assert_eq!(record.instance.index, front.index);

        let requested = uploader.drain_page_requests(0);
        let root_node = &hierarchy.nodes[hierarchy
            .pages
            .iter()
            .find(|page| page.id == root_cook)
            .expect("root")
            .node as usize];
        assert_eq!(
            requested.requests.len(),
            root_node.children.len(),
            "every missing child page is requested exactly once"
        );
        for child in &root_node.children {
            let child_page = hierarchy.nodes[*child as usize].page;
            assert!(
                requested.requests.iter().any(|(slot, class)| {
                    *slot == device_pages[child_page as usize].index
                        && *class == SceneViewClass::Camera
                }),
                "child page {child_page} requested, priced as the camera's"
            );
        }

        device.wait_idle().expect("idle");
        drop(view);
        drop(open);
        drop(address_ubo);
        drop(cull);
        drop(traversal);
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

/// The flip-node state machine: a threshold flip crossfades parent ↔ children
/// over `transition_frames` frames — both sides emit with complementary
/// transition words, the phase advances once per frame, and the cut settles with
/// zero transitioning records afterward, in both directions.
#[test]
fn representation_flip_crossfades_and_settles_in_both_directions() {
    use crate::gpu_scene_upload::{GpuScenePendingUploads, record_pending_global_uploads};
    use crate::page_residency::{PageResidency, PageResidencyBudgets};
    use crate::{GlobalGpuTableKind, GpuMaterialTableRecord, GpuPageRecord};

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

        let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
        let mut pending = GpuScenePendingUploads::default();
        let mut residency = PageResidency::new(PageResidencyBudgets::default());
        let mut gpu_scene =
            PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
        gpu_scene.create_world(WORLD).expect("world");

        let resident_material = gpu_data
            .materials
            .insert(GpuMaterialTableRecord {
                base_color_texture: GpuHandle::INVALID,
                normal_texture: GpuHandle::INVALID,
                coverage: GpuHandle::INVALID,
                parameter_index: 0,
                material_class: crate::GpuMaterialClass::new(
                    saffron_material::AlphaClassification::Masked,
                    crate::GpuSidedness::Single,
                    saffron_material::SurfaceModel::Standard,
                    crate::GpuTransparency::Opaque,
                    false,
                ),
                shader_index: 0,
                flags: 0,
                proxy_albedo: 0,
                occupancy: 1.0,
            })
            .expect("resident material");
        pending.stage_record(GlobalGpuTableKind::Material, resident_material);

        // Every cooked page resident so the walk can refine and coarsen freely.
        let hierarchy = cooked_quad();
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
        let root_children = hierarchy.nodes[hierarchy
            .pages
            .iter()
            .find(|page| page.id == root_cook)
            .expect("root")
            .node as usize]
            .children
            .len();
        assert!(root_children > 0, "the cooked quad root must have children");

        let material = match gpu_scene
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
                    geometry: GpuHandle {
                        index: 3,
                        generation: 1,
                    },
                    materials: std::sync::Arc::from([material]),
                    deformation: None,
                    sdfs: Vec::new().into(),
                    root_page: scene_root,
                    page_bounds: std::sync::Arc::from([]),
                    bounds: [0.0, 0.0, 0.0, 1.0],
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
        create_instance(&mut gpu_scene, prototype, Vec3::ZERO);

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
        let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0)
            * Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        view.write_frame_bindings(&device, &visibility, 0, open.view(), open.view(), address);

        // One simulated frame: clear + cull + traverse at the given refinement
        // threshold with a 3-frame crossfade, then read back the counter words.
        let frames = 3_u32;
        let simulate = |stamp: u32, threshold: f32| -> Vec<u32> {
            let mut graph = RenderGraph::new();
            let hzb_res = graph.import_image(
                open.handle(),
                open.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::GENERAL,
                None,
            );
            let wind_buffer = wind_records_buffer(&device);
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
                    eye: [0.0, 0.0, 5.0],
                    proj_scale: 1000.0,
                    error_threshold_px: threshold,
                    record_capacity: 256,
                    list_capacity: 64,
                    survivor: 0,
                    displaced_records: 0,
                    transition_frames: frames,
                    frame_stamp: stamp,
                    representation_override: SCENE_CUT_AUTO,
                    node_cull: 1,
                    demand_only: 0,
                    view_class: SceneViewClass::Camera.ordinal(),
                },
            );
            one_shot(&device, |cmd| graph.execute(&device, cmd));
            read_words(&device, view.counters(0), 12)
        };

        // Settled on the root, then flip to the children: the flip frame and the
        // two after it emit both sides transitioning; the fourth frame settles.
        let settled_root = simulate(1, 1.0e9);
        assert_eq!(settled_root[3], 1, "the settled far cut is the root alone");
        assert_eq!(settled_root[10], 0, "no crossfade while settled");
        let flip_down = simulate(2, 0.0);
        assert!(flip_down[10] > 1, "both sides emit during the refine flip");
        assert_eq!(
            flip_down[10], flip_down[3],
            "every record is transitioning on the flip frame"
        );
        for stamp in 3..=(frames + 1) {
            let mid = simulate(stamp, 0.0);
            assert!(mid[10] > 0, "the crossfade spans {frames} frames");
        }
        let settled_children = simulate(frames + 2, 0.0);
        assert_eq!(settled_children[10], 0, "the refine crossfade settles");
        assert!(
            settled_children[3] >= root_children as u32,
            "the settled near cut is the children"
        );

        // Flip back: the coarsen crossfade emits both sides, then settles on the
        // root alone.
        let flip_up = simulate(frames + 3, 1.0e9);
        assert!(flip_up[10] > 1, "both sides emit during the coarsen flip");
        for stamp in (frames + 4)..=(2 * frames + 2) {
            let mid = simulate(stamp, 1.0e9);
            assert!(mid[10] > 0, "the coarsen crossfade spans {frames} frames");
        }
        let resettled = simulate(2 * frames + 3, 1.0e9);
        assert_eq!(resettled[10], 0, "the coarsen crossfade settles");
        assert_eq!(resettled[3], 1, "the settled far cut is the root again");
        for words in [&settled_root, &flip_down, &settled_children, &resettled] {
            assert_eq!(words[4] & SCENE_TRANSITION_PRESSURE, 0, "no table pressure");
            assert_eq!(
                words[4] & SCENE_TRAVERSAL_OVERFLOW_RECORDS,
                0,
                "no record overflow"
            );
        }

        device.wait_idle().expect("idle");
        drop(view);
        drop(open);
        drop(address_ubo);
        drop(cull);
        drop(traversal);
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
