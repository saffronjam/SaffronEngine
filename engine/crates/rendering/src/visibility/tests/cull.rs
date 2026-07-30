use super::*;

#[test]
fn cull_frustum_occlusion_and_retest_classify_instances() {
    let device = offscreen_device();
    let before = validation_issue_count();
    {
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("descriptors");
        let visibility = SceneVisibility::new(&device).expect("visibility");
        let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
        let pipeline = pipelines
            .request_scene_visibility(visibility.layout())
            .expect("visibility pso");

        let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
        let mut gpu_scene =
            PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
        gpu_scene.create_world(WORLD).expect("world");

        let material = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                GpuSceneMaterialRecord {
                    table: GpuHandle {
                        index: 1,
                        generation: 1,
                    },
                    source_revision: 1,
                },
            ))
            .expect("material")
        {
            GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let page = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                table: GpuHandle {
                    index: 2,
                    generation: 1,
                },
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
                GpuScenePrototypeRecord {
                    geometry: GpuHandle {
                        index: 3,
                        generation: 1,
                    },
                    materials: std::sync::Arc::from([material]),
                    deformation: None,
                    sdfs: Vec::new().into(),
                    root_page: page,
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
        let aside = create_instance(&mut gpu_scene, prototype, Vec3::new(1000.0, 0.0, 0.0));
        let second = create_instance(&mut gpu_scene, prototype, Vec3::new(0.5, 0.0, 0.0));

        gpu_data.begin_frame(0).expect("gpu data");
        uploader.begin_frame(0).expect("uploader");
        gpu_scene.begin_frame(0).expect("scene");
        let mut graph = RenderGraph::new();
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

        let view = SceneVisibilityView::new(&device, &descriptors, &visibility, 64, 256, 2)
            .expect("view lists");
        let open = hzb_image(&device, 1.0);
        let wall = hzb_image(&device, 0.05);

        let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0)
            * Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let push = |pass_kind: u32, history_valid: u32| SceneVisibilityPush {
            view_proj: view_proj.to_cols_array(),
            prev_view_proj: view_proj.to_cols_array(),
            hzb_extent: [64, 64],
            hzb_mip_count: 7,
            pass_kind,
            history_valid,
            list_capacity: 64,
            reserved: [0; 2],
            reach_min: [0.0; 4],
            reach_max: [0.0; 4],
        };
        let address = (
            address_ubo.handle(),
            0,
            size_of::<crate::GpuSceneAddressBlock>() as u64,
        );

        // Round 1: open pyramid, no history — the two on-screen instances are
        // visible, the far-off one frustum-culls.
        view.write_frame_bindings(&device, &visibility, 0, open.view(), open.view(), address);
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
            &pipeline,
            0,
            hzb_res,
            wind_records_res,
            64,
            push(0, 0),
        );
        one_shot(&device, |cmd| graph.execute(&device, cmd));
        let counters = read_words(&device, view.counters(0), 3);
        assert_eq!(counters[0], 2, "front + second visible");
        assert_eq!(counters[1], 0, "no retest without history");
        assert_eq!(counters[2], 0, "no overflow");
        let mut visible = read_words(&device, view.visible(0), 2);
        visible.sort_unstable();
        let mut expected = vec![front.index, second.index];
        expected.sort_unstable();
        assert_eq!(visible, expected);
        assert!(
            !read_words(&device, view.visible(0), 2).contains(&aside.index),
            "the far-off instance frustum-culls"
        );

        // Round 2 + 3: established instances hit the wall pyramid and wait on the
        // retest list; the retest against the open pyramid merges them back.
        view.write_frame_bindings(&device, &visibility, 0, wall.view(), open.view(), address);
        let mut graph = RenderGraph::new();
        let wall_res = graph.import_image(
            wall.handle(),
            wall.view(),
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::GENERAL,
            None,
        );
        let open_res = graph.import_image(
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
            &pipeline,
            0,
            wall_res,
            wind_records_res,
            64,
            push(0, 1),
        );
        view.add_retest_pass(
            &device,
            &mut graph,
            &pipeline,
            0,
            open_res,
            wind_records_res,
            push(1, 1),
        );
        one_shot(&device, |cmd| graph.execute(&device, cmd));
        let counters = read_words(&device, view.counters(0), 3);
        assert_eq!(counters[1], 2, "both established instances retested");
        assert_eq!(
            counters[0], 2,
            "the open current pyramid merges both survivors"
        );
        assert_eq!(counters[2], 0, "no overflow");

        device.wait_idle().expect("idle");
        drop(view);
        drop(open);
        drop(wall);
        drop(address_ubo);
        drop(pipeline);
        drop(pipelines);
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
