use super::*;

/// A host-visible, device-addressable scratch buffer seeded with `bytes` and declaring `usage`
/// on top of the storage + device-address pair every arena needs.
fn addressed_bytes(device: &Device, bytes: &[u8], usage: vk::BufferUsageFlags) -> Buffer {
    let buffer = Buffer::new(
        device.resources(),
        (bytes.len() as u64).max(16),
        usage | vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        &vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        },
    )
    .expect("addressed scratch");
    // SAFETY: HOST_VISIBLE + MAPPED, written before any submit that reads it.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.mapped_ptr(), bytes.len());
    }
    buffer
}

fn addressed_words(device: &Device, words: &[u32]) -> Buffer {
    addressed_bytes(
        device,
        bytemuck::cast_slice(words),
        vk::BufferUsageFlags::empty(),
    )
}

/// A displaced instance draws its amplification row through the same binned cut as every
/// other representation: the traversal replaces its base-hierarchy walk with one
/// `DISPLACED_MICRO` record carrying the arena row, the binner turns that record into a
/// counted-indirect command lifted from the row's draw seed, and the executor rasterizes it
/// out of the arena's own vertex and index streams.
///
/// The same scene walked with `displaced_records = 0` emits the base cut instead, which is
/// what a view reading the undisplaced surface (a shadow page, the reach walk) gets. The two
/// walks rasterize into their own depth targets from geometry of deliberately different size,
/// so which stream each one actually read is visible in the coverage.
#[test]
fn displaced_instances_draw_through_the_binned_cut() {
    use crate::gpu_scene_upload::{
        GpuArenaUploadRequest, GpuScenePendingUploads, record_pending_global_uploads,
    };
    use crate::page_residency::{PageResidency, PageResidencyBudgets};
    use crate::{
        GlobalGpuTableKind, GpuGeometryRecord, GpuMaterialTableRecord, GpuPageRecord,
        GpuRepresentation,
    };
    use saffron_geometry::Vertex;
    use saffron_geometry::glam::{Vec2, Vec3 as GVec3};

    // The arena row the instance owns, and the draw seed the amplification chain's finalize
    // kernel would have written for it (indexCount, instanceCount, firstIndex, vertexOffset,
    // firstInstance — the binner overwrites the last with the record index). Row 0's seed is
    // zero, so a lookup that returned the wrong row draws nothing.
    //
    // The slice bases are non-zero on purpose: the arena's leading indices are a degenerate
    // decoy and its leading micro-vertices sit behind the eye, so a draw that ignored either
    // base rasterizes no texel at all.
    const ROW: u32 = 1;
    const ARENA_FIRST_INDEX: u32 = 8;
    const ARENA_VERTEX_BASE: u32 = 4;
    const SEED: [u32; 10] = [0, 0, 0, 0, 0, 6, 1, ARENA_FIRST_INDEX, ARENA_VERTEX_BASE, 0];

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
        let bin_seed = pipelines
            .request_scene_bin_seed(visibility.bin_seed_layout())
            .expect("bin seed pso");
        let bin_scatter = pipelines
            .request_scene_bin_scatter(visibility.bin_scatter_layout())
            .expect("bin scatter pso");
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

        let material_class = crate::GpuMaterialClass::new(
            saffron_material::AlphaClassification::Opaque,
            crate::GpuSidedness::Single,
            saffron_material::SurfaceModel::Standard,
            crate::GpuTransparency::Opaque,
            false,
        );
        // The depth pre-pass fragment reads the material's parameter block to decide whether
        // the surface needs a canonical-coverage sample, so it must be real rather than
        // whatever the arena happened to hold.
        let (parameters, _) = gpu_data
            .material_parameters
            .allocate(1, 1)
            .expect("material params");
        pending.upload_arena(GpuArenaUploadRequest::MaterialParams {
            range: parameters,
            data: Box::new(crate::MaterialParamsData::default()),
        });
        let resident_material = gpu_data
            .materials
            .insert(GpuMaterialTableRecord {
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

        // The undisplaced surface: the cooked quad's base vertices in the global arena, spanning
        // one unit at the origin — a small patch of the target next to the arena's full-frame one.
        let hierarchy = cooked_quad();
        let base_vertices: std::sync::Arc<[Vertex]> = {
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
        let vertex_bytes = (base_vertices.len() * size_of::<Vertex>()) as u32;
        let (vertex_range, _) = gpu_data.vertices.allocate(vertex_bytes, 16).expect("verts");
        pending.upload_arena(GpuArenaUploadRequest::Vertices {
            range: vertex_range,
            data: std::sync::Arc::clone(&base_vertices),
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
                vertex_stride: size_of::<Vertex>() as u32,
                index_stride: 4,
                reserved: 0,
            })
            .expect("geometry");
        pending.stage_record(GlobalGpuTableKind::Geometry, geometry);

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
                    geometry,
                    materials: std::sync::Arc::from([material]),
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
        let instance = create_instance(&mut gpu_scene, prototype, Vec3::ZERO, 0);

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

        // The frame's amplification arena as the traversal, the binner and the executor see it.
        //
        // Micro-vertices are 48-byte `Vertex` records (position 0, normal 12, uv 24, tangent 32) —
        // the layout `gpuSceneDisplacedVertex` reads through scalar pointers. Slots 0..4 sit behind
        // the eye and slots 4..8 carry the amplified surface, which overfills the target: a draw
        // reading the wrong slice covers nothing, and one reading the undisplaced surface covers a
        // small patch, so coverage alone separates the three outcomes.
        let micro_vertex = |x: f32, y: f32, z: f32| Vertex {
            position: GVec3::new(x, y, z),
            normal: GVec3::Z,
            uv0: Vec2::new(x, y),
            ..Default::default()
        };
        let micro: Vec<Vertex> = vec![
            micro_vertex(0.0, 0.0, 60.0),
            micro_vertex(1.0, 0.0, 60.0),
            micro_vertex(0.0, 1.0, 60.0),
            micro_vertex(1.0, 1.0, 60.0),
            micro_vertex(-4.0, -4.0, 0.0),
            micro_vertex(4.0, -4.0, 0.0),
            micro_vertex(-4.0, 4.0, 0.0),
            micro_vertex(4.0, 4.0, 0.0),
        ];
        let mut arena_indices = vec![0_u32; ARENA_FIRST_INDEX as usize];
        arena_indices.extend_from_slice(&[0, 1, 3, 0, 3, 2]);
        let vertices = addressed_bytes(
            &device,
            bytemuck::cast_slice(&micro),
            vk::BufferUsageFlags::empty(),
        );
        let indices = addressed_bytes(
            &device,
            bytemuck::cast_slice(&arena_indices),
            vk::BufferUsageFlags::INDEX_BUFFER,
        );
        let rows = addressed_words(&device, &[instance.raw().index, ROW]);
        let seeds = addressed_words(&device, &SEED);
        let vertex_address = device.buffer_device_address(vertices.handle());
        let displaced = crate::DisplacedFrameAddresses {
            vertices: vertex_address,
            prev_vertices: vertex_address,
            indices: device.buffer_device_address(indices.handle()),
            draws: device.buffer_device_address(seeds.handle()),
            rows: device.buffer_device_address(rows.handle()),
            row_count: 1,
        };

        let view = SceneVisibilityView::new(&device, &descriptors, &visibility, 64, 256, 2)
            .expect("view lists");
        let open = hzb_image(&device, 1.0);
        let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0)
            * Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let wind_buffer = wind_records_buffer(&device, &[]);
        let pages_buffer = gpu_data.pages.buffer();

        // Two walks over the identical scene and the identical frame arena — the address
        // block is per frame, shared by every view, so the push is the only thing that can
        // separate a view that consumes the arena from one that reads the undisplaced surface.
        let mut runs = Vec::new();
        for displaced_records in [1_u32, 0] {
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
            let address = (
                address_ubo.handle(),
                0,
                size_of::<crate::GpuSceneAddressBlock>() as u64,
            );
            view.write_frame_bindings(&device, &visibility, 0, open.view(), open.view(), address);
            let instance_set = production_instance_set(&descriptors, &gpu_data, &view, 0, address);
            // The bucket vocabulary the scatter binary-searches: both the arena's
            // representation and the page-resident ones, over the instance's live class.
            let (buckets, table) =
                crate::build_executor_buckets(&[(0, material_class.bits())], 256, true);
            view.write_bucket_table(0, &table);

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
                    eye: [0.0, 0.0, 5.0],
                    proj_scale: 1.0,
                    // A high threshold keeps the walk on the root, so the undisplaced run emits
                    // exactly one base record and the two runs compare record for record.
                    error_threshold_px: 1.0e9,
                    record_capacity: 256,
                    list_capacity: 64,
                    survivor: 0,
                    displaced_records,
                    transition_frames: 0,
                    frame_stamp: 1,
                    representation_override: SCENE_CUT_AUTO,
                    node_cull: 1,
                    demand_only: 0,
                    view_class: SceneViewClass::Camera.ordinal(),
                },
            );
            view.add_binning_passes(
                &device,
                &mut graph,
                (&bin_count, &bin_seed, &bin_scatter),
                0,
                false,
                (0, 0),
                None,
            );

            // The frame's binned cut through the production depth pre-pass. Each bucket
            // binds the index stream the production selector picks for its representation:
            // the arena for a displaced bucket, the pages arena for the page-resident ones.
            let depth = depth_target(&device);
            let draw_inputs = view.executor_draw_inputs(0, 256, indices.handle());
            add_depth_prepass(
                &device,
                &mut graph,
                "displaced-depth-prepass",
                &prepass,
                &depth,
                (descriptors.bindless_set(), instance_set),
                view_proj,
                draw_inputs,
                pages_buffer,
                &buckets,
            );
            one_shot(&device, |cmd| graph.execute(&device, cmd));

            let counters = read_words(&device, view.counters(0), 16);
            let record_words = read_words(
                &device,
                view.records(0),
                size_of::<crate::GpuDrawRecord>() / 4,
            );
            let record: crate::GpuDrawRecord =
                *bytemuck::from_bytes(bytemuck::cast_slice(&record_words));
            let bucket = buckets
                .iter()
                .position(|bucket| {
                    (bucket.shader_index << 16) | (bucket.pso_bin & 0xFFFF)
                        == (record.reserved << 16) | (record.pso_bin.bits() & 0xFFFF)
                })
                .expect("the record's bucket is in the frame table");
            let slice = buckets[bucket].base as usize * 5;
            let commands = read_words(&device, view.commands(0), slice + 5);
            let command: Vec<u32> = commands[slice..slice + 5].to_vec();
            let covered = count_written_depth(&device, &depth);
            runs.push((counters, record, command, covered));
            device.wait_idle().expect("idle after the walk");
            descriptors.free_sets(&[instance_set]);
            drop(depth);
            drop(address_ubo);
        }

        let (displaced_counters, displaced_record, displaced_command, displaced_covered) = &runs[0];
        let (base_counters, base_record, _, base_covered) = &runs[1];

        assert_eq!(displaced_counters[0], 1, "the instance is visible");
        assert_eq!(
            displaced_counters[3], 1,
            "a displaced instance emits exactly one record — its whole amplification row"
        );
        assert_eq!(
            displaced_record.representation,
            GpuRepresentation::DisplacedMicro as u32,
            "the record names the amplification arena as its geometry source"
        );
        assert_eq!(
            displaced_record.content_index, ROW,
            "the record carries the arena row, not a page handle"
        );
        assert_eq!(
            displaced_record.pso_bin.bits() & 0x3,
            GpuRepresentation::DisplacedMicro as u32,
            "the psoBin selects the displaced draw bucket, so the pass binds the arena's \
             index stream for it"
        );
        assert_eq!(
            displaced_command,
            &vec![SEED[5], 1, SEED[7], SEED[8], 0],
            "the binned command is the row's draw seed with the record index as firstInstance"
        );
        assert_eq!(
            displaced_counters[8],
            SEED[5] / 3,
            "the frame's rasterized-triangle count gains the row's packed triangles"
        );

        // The amplified surface reached the framebuffer: it overfills the target, so anything
        // short of near-full coverage means the draw fetched from somewhere other than the
        // arena's own micro-vertex and index streams at the seed's slice bases.
        assert!(
            *displaced_covered > 4000,
            "the displaced bucket rasterized the arena's amplified surface \
             ({displaced_covered} of 4096 depth texels written)"
        );

        // The same instance and the same frame arena, walked by a view whose push does not
        // consume it (a shadow page, the reach walk): the base cut.
        assert_eq!(base_counters[3], 1, "the base walk emits the root's record");
        assert_ne!(
            base_record.representation,
            GpuRepresentation::DisplacedMicro as u32,
            "a view reading the undisplaced surface never reaches the arena"
        );
        assert_eq!(
            base_record.content_index, root_handle.index,
            "the base record draws the resident root page"
        );
        // And it rasterized the undisplaced unit quad, not the arena's full-frame surface — the
        // pixel-level form of the record assertion above.
        assert!(
            *base_covered > 16 && *base_covered * 4 < *displaced_covered,
            "the base walk rasterized the undisplaced surface ({base_covered} depth texels \
             against the arena's {displaced_covered})"
        );

        device.wait_idle().expect("idle");
        drop(view);
        drop(open);
        drop(wind_buffer);
        drop(rows);
        drop(seeds);
        drop(vertices);
        drop(indices);
        drop(cull);
        drop(traversal);
        drop(bin_count);
        drop(bin_seed);
        drop(bin_scatter);
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

/// The index stream each draw bucket binds. A displaced bucket reads the frame's amplification
/// arena; every page-resident representation reads the pages arena. A view that never allocates
/// the arena (a shadow page, the GI reach walk) passes a null handle, and a displaced bucket
/// there must fall back rather than bind nothing — the arena is per frame and shared by every
/// view, so a leaked displaced bucket would otherwise fetch a stream the view never bound.
#[test]
fn a_draw_bucket_binds_the_index_stream_its_representation_reads() {
    use ash::vk::Handle;

    let pages = vk::Buffer::from_raw(0x1111);
    let arena = vk::Buffer::from_raw(0x2222);
    let bucket = |representation: crate::GpuRepresentation| crate::ExecutorBucket {
        shader_index: 0,
        pso_bin: (representation as u32) << crate::GPU_PSO_REPRESENTATION_SHIFT,
        base: 0,
        capacity: 64,
    };

    assert_eq!(
        crate::bucket_index_buffer(
            bucket(crate::GpuRepresentation::DisplacedMicro),
            pages,
            arena
        ),
        arena,
        "a displaced bucket fetches through the amplification arena"
    );
    for representation in [
        crate::GpuRepresentation::TriangleCluster,
        crate::GpuRepresentation::AggregateVoxel,
        crate::GpuRepresentation::MicroBlade,
    ] {
        assert_eq!(
            crate::bucket_index_buffer(bucket(representation), pages, arena),
            pages,
            "{representation:?} fetches through the pages arena even with an arena bound"
        );
    }
    assert_eq!(
        crate::bucket_index_buffer(
            bucket(crate::GpuRepresentation::DisplacedMicro),
            pages,
            vk::Buffer::null()
        ),
        pages,
        "a view with no arena this frame falls back to the pages arena"
    );
}
