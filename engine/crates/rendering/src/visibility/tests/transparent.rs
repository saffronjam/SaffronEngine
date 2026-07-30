use super::*;

#[test]
fn transparent_records_sort_back_to_front() {
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
        let keys = pipelines
            .request_transparent_keys(visibility.transparent_keys_layout())
            .expect("keys pso");
        let histogram = pipelines
            .request_radix_histogram(visibility.radix_histogram_layout())
            .expect("histogram pso");
        let scan = pipelines
            .request_radix_scan(visibility.radix_scan_layout())
            .expect("scan pso");
        let scatter = pipelines
            .request_radix_scatter(visibility.radix_scatter_layout())
            .expect("scatter pso");
        let reorder = pipelines
            .request_transparent_reorder(visibility.transparent_reorder_layout())
            .expect("reorder pso");

        let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
        let mut pending = GpuScenePendingUploads::default();
        let mut residency = PageResidency::new(PageResidencyBudgets::default());
        let mut gpu_scene =
            PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
        gpu_scene.create_world(WORLD).expect("world");

        // An alpha-blended resident material so leaf clusters bin transparent.
        let resident_material = gpu_data
            .materials
            .insert(GpuMaterialTableRecord {
                base_color_texture: GpuHandle::INVALID,
                normal_texture: GpuHandle::INVALID,
                coverage: GpuHandle::INVALID,
                parameter_index: 0,
                material_class: crate::GpuMaterialClass::new(
                    saffron_material::AlphaClassification::Opaque,
                    crate::GpuSidedness::Single,
                    saffron_material::SurfaceModel::Standard,
                    crate::GpuTransparency::AlphaBlended,
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
                    geometry: GpuHandle {
                        index: 3,
                        generation: 1,
                    },
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
        // Three instances at increasing distance from the camera at z = 5.
        let near = create_instance(&mut gpu_scene, prototype, Vec3::ZERO);
        let middle = create_instance(&mut gpu_scene, prototype, Vec3::new(0.0, 0.0, -3.0));
        let far = create_instance(&mut gpu_scene, prototype, Vec3::new(0.0, 0.0, -6.0));

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
        let camera_view =
            Mat4::look_at_rh(Vec3::new(0.5, 0.5, 5.0), Vec3::new(0.5, 0.5, 0.0), Vec3::Y);
        let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0) * camera_view;
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
                eye: [0.5, 0.5, 5.0],
                proj_scale: 100_000.0,
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
        // The view matrix's third row measures view-space depth.
        let row2 = camera_view.row(2);
        // The blend material's two representation buckets, in bucket-key order —
        // the reorder writes one masked full-length slice per bucket.
        let material_class = crate::GpuMaterialClass::new(
            saffron_material::AlphaClassification::Opaque,
            crate::GpuSidedness::Single,
            saffron_material::SurfaceModel::Standard,
            crate::GpuTransparency::AlphaBlended,
            false,
        );
        let blend_keys: Vec<u32> = [
            crate::GpuRepresentation::TriangleCluster as u32,
            crate::GpuRepresentation::AggregateVoxel as u32,
        ]
        .iter()
        .map(|representation| {
            (representation << crate::GPU_PSO_REPRESENTATION_SHIFT)
                | (material_class.bits() << crate::GPU_PSO_MATERIAL_SHIFT)
        })
        .collect();
        view.add_transparent_sort_passes(
            &device,
            &mut graph,
            TransparentSortPipelines {
                keys: &keys,
                histogram: &histogram,
                scan: &scan,
                scatter: &scatter,
                reorder: &reorder,
            },
            0,
            [row2.x, row2.y, row2.z, row2.w],
            &blend_keys,
        );
        one_shot(&device, |cmd| graph.execute(&device, cmd));

        let counters = read_words(&device, view.counters(0), 8);
        let record_count = counters[3] as usize;
        let pair_count = counters[5] as usize;
        assert!(record_count >= 3, "each instance emits at least one record");
        assert_eq!(
            pair_count, record_count,
            "every alpha-blended record collects a sort pair"
        );
        assert_eq!(counters[4], 0, "no overflow");

        let record_words = read_words(
            &device,
            view.records(0),
            record_count * size_of::<crate::GpuDrawRecord>() / 4,
        );
        let records: &[crate::GpuDrawRecord] = bytemuck::cast_slice(&record_words);
        // Both bucket slices, full length: exactly one bucket owns each sorted
        // slot with a live draw; the other masks it to a zero draw.
        let command_words = read_words(&device, view.transparent_commands(0), 2 * 256 * 5);
        let order: Vec<u32> = (0..pair_count)
            .map(|slot| {
                let live: Vec<&[u32]> = (0..2)
                    .map(|group| {
                        let base = (group * 256 + slot) * 5;
                        &command_words[base..base + 5]
                    })
                    .filter(|command| command[0] != 0)
                    .collect();
                assert_eq!(live.len(), 1, "exactly one bucket owns slot {slot}");
                records[live[0][4] as usize].instance.index
            })
            .collect();

        // Back-to-front: every far record precedes every middle record, which
        // precede every near record.
        let position = |slot: u32| order.iter().position(|entry| *entry == slot);
        let last = |slot: u32| order.iter().rposition(|entry| *entry == slot);
        let far_last = last(far.index).expect("far drawn");
        let middle_first = position(middle.index).expect("middle drawn");
        let middle_last = last(middle.index).expect("middle drawn");
        let near_first = position(near.index).expect("near drawn");
        assert!(
            far_last < middle_first,
            "far draws before middle: {order:?}"
        );
        assert!(
            middle_last < near_first,
            "middle draws before near: {order:?}"
        );

        device.wait_idle().expect("idle");
        drop(view);
        drop(open);
        drop(address_ubo);
        drop(cull);
        drop(traversal);
        drop(keys);
        drop(histogram);
        drop(scan);
        drop(scatter);
        drop(reorder);
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
