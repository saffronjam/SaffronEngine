use super::*;

#[test]
fn pending_queue_drains_records_arenas_and_tombstones() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let before = validation_issue_count();
    {
        let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let mut pending = GpuScenePendingUploads::default();
        gpu_data.begin_frame(0).expect("begin");

        let vertex_count = 3_u32;
        let vertex_bytes = vertex_count * size_of::<saffron_geometry::Vertex>() as u32;
        let (vertex_range, _) = gpu_data.vertices.allocate(vertex_bytes, 16).expect("verts");
        let geometry = gpu_data
            .geometries
            .insert(crate::GpuGeometryRecord {
                vertices: vertex_range,
                indices: GpuArenaRange::default(),
                clusters: GpuArenaRange::default(),
                parts: GpuArenaRange::default(),
                voxels: GpuArenaRange::default(),
                submeshes: GpuArenaRange::default(),
                flags: 0,
                vertex_stride: size_of::<saffron_geometry::Vertex>() as u32,
                index_stride: 4,
                reserved: 0,
            })
            .expect("geometry");
        let vertices: Arc<[saffron_geometry::Vertex]> = std::iter::repeat_n(
            saffron_geometry::Vertex {
                position: Vec3::new(7.0, 8.0, 9.0),
                ..Default::default()
            },
            vertex_count as usize,
        )
        .collect();
        pending.stage_record(GlobalGpuTableKind::Geometry, geometry);
        pending.upload_arena(GpuArenaUploadRequest::Vertices {
            range: vertex_range,
            data: Arc::clone(&vertices),
        });

        let mut graph = RenderGraph::new();
        record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
            .expect("drain");
        assert!(
            pending.is_empty(),
            "the drain consumes every queued request"
        );
        run_graph(&device, &mut graph);

        let desc = gpu_data.geometries.descriptor(&device);
        let bytes = read_device_buffer(&device, desc.buffer, desc.range);
        let slot = slot_bytes(&bytes, desc.slot_stride, geometry.index);
        let header: &GpuTableSlotHeader = bytemuck::from_bytes(&slot[..16]);
        assert_eq!(header.occupied, 1);
        let body: &crate::GpuGeometryRecord =
            bytemuck::from_bytes(&slot[16..16 + size_of::<crate::GpuGeometryRecord>()]);
        assert_eq!(body.vertices, vertex_range);

        let arena_bytes = read_device_buffer(
            &device,
            gpu_data.vertices.buffer(),
            u64::from(vertex_range.first) + u64::from(vertex_bytes),
        );
        let uploaded = &arena_bytes[vertex_range.first as usize..];
        assert_eq!(
            uploaded,
            bytemuck::cast_slice::<saffron_geometry::Vertex, u8>(&vertices),
            "vertex bytes reach their arena range exactly"
        );

        pending.retire_record(GlobalGpuTableKind::Geometry, geometry);
        let mut graph = RenderGraph::new();
        record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
            .expect("drain retire");
        run_graph(&device, &mut graph);
        let bytes = read_device_buffer(&device, desc.buffer, desc.range);
        let slot = slot_bytes(&bytes, desc.slot_stride, geometry.index);
        let header: &GpuTableSlotHeader = bytemuck::from_bytes(&slot[..16]);
        assert_eq!(header.occupied, 0, "the retirement tombstones the slot");

        device.wait_idle().expect("idle");
    }
    drop(device);
    assert_eq!(validation_issue_count(), before);
}

#[test]
fn page_payload_bytes_reach_the_page_arena_byte_exact() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let before = validation_issue_count();
    {
        let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let mut pending = GpuScenePendingUploads::default();
        let payload: Vec<u8> = (0..192_u32).flat_map(|word| word.to_le_bytes()).collect();
        let (range, _) = gpu_data
            .pages
            .allocate(payload.len() as u32, 16)
            .expect("page range");
        pending.upload_arena(GpuArenaUploadRequest::PageBytes {
            range,
            data: payload.clone(),
        });
        let mut graph = RenderGraph::new();
        record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
            .expect("drain");
        run_graph(&device, &mut graph);

        let offset = gpu_data.pages.byte_offset(range).expect("offset");
        let bytes = read_device_buffer(
            &device,
            gpu_data.pages.buffer(),
            offset + payload.len() as u64,
        );
        assert_eq!(
            &bytes[offset as usize..],
            payload.as_slice(),
            "page payload bytes reach their arena range exactly"
        );
        device.wait_idle().expect("idle");
    }
    drop(device);
    assert_eq!(validation_issue_count(), before);
}
