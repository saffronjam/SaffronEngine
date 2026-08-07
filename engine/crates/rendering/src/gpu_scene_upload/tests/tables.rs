use super::*;

#[test]
fn scene_records_reach_their_device_tables_byte_exact() {
    let Some(mut harness) = harness() else {
        return;
    };
    let before = validation_issue_count();

    let material = match harness
        .gpu_scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
            GpuSceneMaterialRecord {
                table: device_handle(7),
                source_revision: 11,
            },
        ))
        .expect("material")
    {
        GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
        other => panic!("unexpected {other:?}"),
    };
    let page = match harness
        .gpu_scene
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
    let prototype = match harness
        .gpu_scene
        .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
            crate::GpuScenePrototypeRecord {
                geometry: device_handle(20),
                materials: std::sync::Arc::from([material]),
                deformation: None,
                sdfs: Vec::new().into(),
                root_page: page,
                page_bounds: std::sync::Arc::from([]),
                bounds: [1.0, 2.0, 3.0, 4.0],
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
    let current = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
    let previous = Mat4::from_translation(Vec3::new(4.0, 5.0, 6.0));
    let transform = GpuSceneDynamicTransform::new(current, previous).expect("transform");
    let instance = match harness
        .gpu_scene
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
    let light_data = GpuLight {
        position_range: Vec4::new(1.0, 2.0, 3.0, 4.0),
        color_intensity: Vec4::new(0.5, 0.25, 0.125, 8.0),
        direction_type: Vec4::ZERO,
        spot_cos: Vec4::new(0.0, 0.0, 1.0, 0.0),
    };
    harness
        .gpu_scene
        .apply_world_delta(
            WORLD,
            GpuSceneWorldDelta::CreateLight(GpuSceneLightRecord {
                light: light_data,
                source_revision: 21,
            }),
        )
        .expect("light");

    harness.begin(0);
    let stats = harness.record_and_run(0);
    assert!(stats.records >= 5, "all created records staged");
    assert!(!stats.budget_exhausted);

    let materials_desc = harness.uploader.descriptors(&harness.device).materials;
    let bytes = read_device_buffer(&harness.device, materials_desc.buffer, materials_desc.range);
    let slot = slot_bytes(&bytes, materials_desc.slot_stride, material.raw().index);
    let header: &GpuTableSlotHeader = bytemuck::from_bytes(&slot[..16]);
    assert_eq!(header.occupied, 1);
    assert_eq!(header.generation, material.raw().generation);
    let body: &GpuSceneReferenceGpuRecord = bytemuck::from_bytes(&slot[16..32]);
    assert_eq!(body.target, device_handle(7));
    assert_eq!(body.source_revision, 11);

    let proto_desc = harness.uploader.descriptors(&harness.device).prototypes;
    let bytes = read_device_buffer(&harness.device, proto_desc.buffer, proto_desc.range);
    let slot = slot_bytes(&bytes, proto_desc.slot_stride, prototype.raw().index);
    let body: &GpuScenePrototypeGpuRecord =
        bytemuck::from_bytes(&slot[16..16 + size_of::<GpuScenePrototypeGpuRecord>()]);
    assert_eq!(body.geometry, device_handle(20));
    assert_eq!(body.bounds, [1.0, 2.0, 3.0, 4.0]);
    assert_eq!(body.root_page, page.raw());
    assert_eq!(body.material_range.count, 1);
    let material_elements = read_device_buffer(
        &harness.device,
        harness.gpu_data.prototype_materials.buffer(),
        (u64::from(body.material_range.first) + 1) * size_of::<GpuHandle>() as u64,
    );
    let element: &GpuHandle = bytemuck::from_bytes(
        &material_elements[body.material_range.first as usize * size_of::<GpuHandle>()..]
            [..size_of::<GpuHandle>()],
    );
    assert_eq!(*element, material.raw());

    let world_desc = harness
        .uploader
        .world_descriptors(&harness.device, WORLD)
        .expect("world tables");
    let bytes = read_device_buffer(
        &harness.device,
        world_desc.instances.buffer,
        world_desc.instances.range,
    );
    let slot = slot_bytes(
        &bytes,
        world_desc.instances.slot_stride,
        instance.raw().index,
    );
    let body: &GpuSceneInstanceGpuRecord =
        bytemuck::from_bytes(&slot[16..16 + size_of::<GpuSceneInstanceGpuRecord>()]);
    assert_eq!(body.prototype, prototype.raw());
    assert_eq!(body.transform_kind, GPU_SCENE_TRANSFORM_DYNAMIC);
    assert_eq!(body.transform[..16], current.to_cols_array());
    assert_eq!(body.transform[16..], previous.to_cols_array());
    assert_eq!(body.material_overrides.count, 1);

    let bytes = read_device_buffer(
        &harness.device,
        world_desc.lights.buffer,
        world_desc.lights.range,
    );
    let slot = slot_bytes(&bytes, world_desc.lights.slot_stride, 0);
    let body: &GpuSceneLightGpuRecord =
        bytemuck::from_bytes(&slot[16..16 + size_of::<GpuSceneLightGpuRecord>()]);
    assert_eq!(body.light, light_data);
    assert_eq!(body.source_revision, 21);

    harness
        .gpu_scene
        .apply_world_delta(WORLD, GpuSceneWorldDelta::RemoveInstance(instance))
        .expect("remove");
    let stats = harness.record_and_run(0);
    assert_eq!(stats.records, 1, "only the tombstone re-uploads");
    let bytes = read_device_buffer(
        &harness.device,
        world_desc.instances.buffer,
        world_desc.instances.range,
    );
    let slot = slot_bytes(
        &bytes,
        world_desc.instances.slot_stride,
        instance.raw().index,
    );
    let header: &GpuTableSlotHeader = bytemuck::from_bytes(&slot[..16]);
    assert_eq!(header.occupied, 0, "tombstoned slot reads unoccupied");

    harness.finish();
    assert_eq!(validation_issue_count(), before);
}

#[test]
fn storage_growth_preserves_existing_slots() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let before = validation_issue_count();
    {
        let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let mut storage: GpuSceneTableStorage<SceneInstanceTable> =
            GpuSceneTableStorage::new(&device, 176, 1).expect("storage");
        let capacity = storage.storage.capacity();
        gpu_data.begin_frame(0).expect("begin");

        storage.ensure_slots(1).expect("slot 0");
        let mut graph = RenderGraph::new();
        let marker = [0xA5_u8; 176];
        storage
            .stage_slot(&mut gpu_data.uploads, 0, 0, 1, 1, &marker)
            .expect("stage slot 0")
            .enqueue(&mut graph, &device, "seed slot 0");
        run_graph(&device, &mut graph);

        let grown_slots = capacity + 8;
        storage.ensure_slots(grown_slots).expect("grow slots");
        let mut graph = RenderGraph::new();
        let growth = storage.prepare_growth(&device).expect("prepare growth");
        let growth = growth.expect("capacity exceeded requires growth");
        growth.enqueue(&mut graph, &device, "grow");
        let last = u32::try_from(grown_slots - 1).expect("slot index");
        storage
            .stage_slot(&mut gpu_data.uploads, 0, last, 2, 1, &[0x5A_u8; 176])
            .expect("stage last")
            .enqueue(&mut graph, &device, "seed last");
        run_graph(&device, &mut graph);

        let stride = storage.slot_stride();
        let bytes = read_device_buffer(&device, storage.storage.buffer(), grown_slots * stride);
        let first = slot_bytes(&bytes, stride, 0);
        assert!(
            first[16..16 + 176].iter().all(|byte| *byte == 0xA5),
            "growth preserved the seeded slot"
        );
        let tail = slot_bytes(&bytes, stride, last);
        assert!(tail[16..16 + 176].iter().all(|byte| *byte == 0x5A));
        device.wait_idle().expect("idle");
    }
    drop(device);
    // Recreate a device to flush validation counters deterministically is unnecessary;
    // the counter is process-wide.
    assert_eq!(validation_issue_count(), before);
}
