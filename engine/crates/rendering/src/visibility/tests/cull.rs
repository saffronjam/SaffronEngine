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

        let prototype = unit_bounds_prototype(&mut gpu_scene);
        let front = create_instance(&mut gpu_scene, prototype, Vec3::ZERO, 0);
        let aside = create_instance(&mut gpu_scene, prototype, Vec3::new(1000.0, 0.0, 0.0), 0);
        let second = create_instance(&mut gpu_scene, prototype, Vec3::new(0.5, 0.0, 0.0), 0);

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
        let wind_buffer = wind_records_buffer(&device, &[]);
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
        let mut expected = vec![front.raw().index, second.raw().index];
        expected.sort_unstable();
        assert_eq!(visible, expected);
        assert!(
            !read_words(&device, view.visible(0), 2).contains(&aside.raw().index),
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
        let wind_buffer = wind_records_buffer(&device, &[]);
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

/// A displaced instance is culled against its COOKED bounds, which describe the undisplaced
/// surface — so the cull has to add the frame's relief bound or geometry that pokes outside the
/// base sphere disappears at the screen edge. The instance here sits far enough off-axis that its
/// cooked sphere is provably outside the frustum and only the amplitude reaches back in, which is
/// what makes the assertion sensitive to the inflation alone.
#[test]
fn a_displaced_instance_is_culled_against_its_inflated_bounds() {
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
        let prototype = unit_bounds_prototype(&mut gpu_scene);
        // Just outside: at 5 units from the eye the frustum half-width is 2.73, and the unit
        // sphere's most-inside corner projects to NDC x = 1.37.
        let edge = create_instance(&mut gpu_scene, prototype, Vec3::new(5.0, 0.0, 0.0), 0);

        gpu_data.begin_frame(0).expect("gpu data");
        uploader.begin_frame(0).expect("uploader");
        gpu_scene.begin_frame(0).expect("scene");
        let mut graph = RenderGraph::new();
        uploader
            .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
            .expect("record");
        one_shot(&device, |cmd| graph.execute(&device, cmd));

        let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0)
            * Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let push = SceneVisibilityPush {
            view_proj: view_proj.to_cols_array(),
            prev_view_proj: view_proj.to_cols_array(),
            hzb_extent: [64, 64],
            hzb_mip_count: 7,
            pass_kind: 0,
            history_valid: 0,
            list_capacity: 64,
            reserved: [0; 2],
            reach_min: [0.0; 4],
            reach_max: [0.0; 4],
        };
        let open = hzb_image(&device, 1.0);

        // Two rounds over the same scene: no displaced-row table, then one naming the instance
        // with a 3-unit local relief. Only the table differs.
        for (rows, expect_visible) in [
            (Vec::new(), 0_u32),
            (
                vec![crate::DisplacedRow {
                    slot: edge.raw().index,
                    row: 0,
                    local_amplitude: 3.0,
                    reserved: 0,
                }],
                1,
            ),
        ] {
            let row_buffer = displaced_rows_buffer(&device, &rows);
            let displaced = crate::DisplacedFrameAddresses {
                rows: if rows.is_empty() {
                    0
                } else {
                    device.buffer_device_address(row_buffer.handle())
                },
                row_count: rows.len() as u32,
                ..Default::default()
            };
            let block = uploader.build_address_block(
                &device,
                &gpu_data,
                WORLD,
                0,
                (0, 0),
                0,
                0,
                0,
                displaced,
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
            let lists = SceneVisibilityView::new(&device, &descriptors, &visibility, 64, 256, 2)
                .expect("view lists");
            lists.write_frame_bindings(
                &device,
                &visibility,
                0,
                open.view(),
                open.view(),
                (
                    address_ubo.handle(),
                    0,
                    size_of::<crate::GpuSceneAddressBlock>() as u64,
                ),
            );
            let mut graph = RenderGraph::new();
            let hzb_res = graph.import_image(
                open.handle(),
                open.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::GENERAL,
                None,
            );
            let wind_buffer = wind_records_buffer(&device, &[]);
            let wind_records_res = graph.import_buffer(wind_buffer.handle(), None);
            lists.add_cull_pass(
                &device,
                &mut graph,
                &pipeline,
                0,
                hzb_res,
                wind_records_res,
                64,
                push,
            );
            one_shot(&device, |cmd| graph.execute(&device, cmd));
            let counters = read_words(&device, lists.counters(0), 3);
            assert_eq!(
                counters[0],
                expect_visible,
                "displaced rows {}: the cooked sphere is outside the frustum and the relief \
                 bound is what reaches back in",
                rows.len()
            );
            device.wait_idle().expect("idle");
            let mut lists = lists;
            lists.free_sets(&descriptors);
            drop(lists);
            drop(address_ubo);
            drop(row_buffer);
        }

        device.wait_idle().expect("idle");
        drop(open);
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
