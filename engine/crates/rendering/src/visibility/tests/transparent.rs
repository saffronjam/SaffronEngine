use super::*;

use crate::gpu_scene_upload::{GpuScenePendingUploads, record_pending_global_uploads};
use crate::page_residency::{PageResidency, PageResidencyBudgets};
use crate::{GlobalGpuTableKind, GpuMaterialTableRecord, GpuPageRecord};

/// The device-side scene the recorded frame uploads from.
struct SceneState {
    gpu_data: GlobalGpuData,
    uploader: GpuSceneUploader,
    gpu_scene: PersistentGpuScene,
    pending: GpuScenePendingUploads,
}

/// The compute stages one recorded frame walks, from the instance cull to the sorted
/// transparent command stream.
struct TransparentChain<'a> {
    cull: &'a Arc<crate::Pipeline>,
    traversal: &'a Arc<crate::Pipeline>,
    sort: TransparentSortPipelines<'a>,
}

/// The camera every recorded frame sorts against: on +Z looking down -Z, so view-space
/// depth is a function of an instance's z alone and two instances sharing a z share an
/// exactly equal sort key.
fn camera_view() -> Mat4 {
    Mat4::look_at_rh(Vec3::new(0.5, 0.5, 5.0), Vec3::new(0.5, 0.5, 0.0), Vec3::Y)
}

/// One sorted transparent draw, in the order the transparent pass replays them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SortedDraw {
    /// The persistent scene slot the draw's instance occupies.
    instance: u32,
    /// The page the traversal's cut drew from, which moves when the cut moves.
    page: u32,
}

/// Records one frame — pending uploads, cull, traversal, the sort levels, the reorder —
/// and returns its sorted transparent draws.
///
/// `error_threshold_px` is the traversal's refinement criterion: zero refines to the
/// finest cut, so a prototype draws from its leaf pages; a threshold no node's projected
/// error can exceed stops it at its guaranteed root.
#[allow(clippy::too_many_arguments)]
fn sorted_draws(
    device: &Device,
    chain: &TransparentChain<'_>,
    view: &SceneVisibilityView,
    state: &mut SceneState,
    open: &Image,
    address_ubo: &Buffer,
    visibility: &SceneVisibility,
    error_threshold_px: f32,
) -> Vec<SortedDraw> {
    state.gpu_data.begin_frame(0).expect("gpu data");
    state.uploader.begin_frame(0).expect("uploader");
    state.gpu_scene.begin_frame(0).expect("scene");
    let mut graph = RenderGraph::new();
    record_pending_global_uploads(
        &mut state.pending,
        device,
        &mut graph,
        &mut state.gpu_data,
        0,
    )
    .expect("drain pending");
    state
        .uploader
        .record_frame(
            device,
            &mut graph,
            &mut state.gpu_data,
            &mut state.gpu_scene,
            0,
        )
        .expect("record");
    one_shot(device, |cmd| graph.execute(device, cmd));

    let block = state.uploader.build_address_block(
        device,
        &state.gpu_data,
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
    view.write_frame_bindings(device, visibility, 0, open.view(), open.view(), address);

    let camera = camera_view();
    let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0) * camera;
    let mut graph = RenderGraph::new();
    let hzb_res = graph.import_image(
        open.handle(),
        open.view(),
        vk::ImageAspectFlags::COLOR,
        vk::ImageLayout::GENERAL,
        None,
    );
    let wind_buffer = wind_records_buffer(device, &[]);
    let wind_records_res = graph.import_buffer(wind_buffer.handle(), None);
    view.add_cull_pass(
        device,
        &mut graph,
        chain.cull,
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
        device,
        &mut graph,
        chain.traversal,
        0,
        SceneTraversalPush {
            view_proj: view_proj.to_cols_array(),
            eye: [0.5, 0.5, 5.0],
            proj_scale: 100_000.0,
            error_threshold_px,
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
    let row2 = camera.row(2);
    // The blend material's two representation buckets, in bucket-key order — the reorder
    // writes one masked full-length slice per bucket.
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
        device,
        &mut graph,
        TransparentSortPipelines {
            keys: chain.sort.keys,
            histogram: chain.sort.histogram,
            scan: chain.sort.scan,
            scatter: chain.sort.scatter,
            reorder: chain.sort.reorder,
        },
        0,
        [row2.x, row2.y, row2.z, row2.w],
        &blend_keys,
    );
    one_shot(device, |cmd| graph.execute(device, cmd));

    let counters = read_words(device, view.counters(0), 8);
    let record_count = counters[3] as usize;
    let pair_count = counters[5] as usize;
    assert_eq!(
        pair_count, record_count,
        "every alpha-blended record collects a sort pair"
    );
    assert_eq!(counters[4], 0, "no overflow");

    let record_words = read_words(
        device,
        view.records(0),
        record_count * size_of::<crate::GpuDrawRecord>() / 4,
    );
    let records: &[crate::GpuDrawRecord] = bytemuck::cast_slice(&record_words);
    // The whole command arena: the binner's slices first, then both bucket slices at full
    // length. Exactly one bucket owns each sorted slot with a live draw; the other masks it
    // to a zero draw. The mesh executor's arguments ride the same slots, so they are read
    // alongside and must agree with the indexed command they cover.
    let command_words = read_words(device, view.commands(0), 3 * 256 * 5);
    let mesh_arg_words = read_words(device, view.mesh_args(0), 3 * 256 * 3);
    (0..pair_count)
        .map(|slot| {
            let live: Vec<&[u32]> = (0..2)
                .filter_map(|group| {
                    let arena = view.transparent_slice_base(group) as usize + slot;
                    let command = &command_words[arena * 5..arena * 5 + 5];
                    assert_eq!(
                        mesh_arg_words[arena * 3],
                        (command[0] / 3).div_ceil(crate::MESH_TRIANGLES_PER_GROUP),
                        "slot {slot}'s mesh dispatch covers its own triangle count"
                    );
                    (command[0] != 0).then_some(command)
                })
                .collect();
            assert_eq!(live.len(), 1, "exactly one bucket owns slot {slot}");
            let command = live[0];
            let record = &records[command[4] as usize];
            SortedDraw {
                instance: record.instance.index,
                page: record.content_index,
            }
        })
        .collect()
}

/// The instances the draws visit, in order. An instance's draws share every key level
/// above the page, so they are contiguous and collapsing runs recovers that order.
fn instance_order(draws: &[SortedDraw]) -> Vec<u32> {
    let mut runs: Vec<u32> = Vec::new();
    for draw in draws {
        if runs.last() != Some(&draw.instance) {
            runs.push(draw.instance);
        }
    }
    runs
}

#[test]
fn transparent_records_sort_back_to_front_and_hold_order_across_changes() {
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

        let mut residency = PageResidency::new(PageResidencyBudgets::default());
        let mut state = SceneState {
            gpu_data: GlobalGpuData::new(&device).expect("GlobalGpuData"),
            uploader: GpuSceneUploader::new(&device).expect("uploader"),
            gpu_scene: PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene"),
            pending: GpuScenePendingUploads::default(),
        };
        state.gpu_scene.create_world(WORLD).expect("world");

        // An alpha-blended resident material so leaf clusters bin transparent.
        let resident_material = state
            .gpu_data
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
        state
            .pending
            .stage_record(GlobalGpuTableKind::Material, resident_material);

        let hierarchy = cooked_quad();
        let mut device_pages = Vec::new();
        for page in &hierarchy.pages {
            let parent = page
                .dependency
                .map(|dependency| device_pages[dependency as usize]);
            let handle = state
                .gpu_data
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
            state.pending.stage_record(GlobalGpuTableKind::Page, handle);
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
            .publish_ready(&mut state.gpu_data, &mut state.pending)
            .expect("publish all");

        let root_cook = hierarchy
            .pages
            .iter()
            .find(|page| page.guaranteed_root)
            .expect("root page")
            .id;
        let scene_material = match state
            .gpu_scene
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
        let scene_root = match state
            .gpu_scene
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
        let prototype = match state
            .gpu_scene
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
        // Three distinct depths from the camera at z = 5, and a fourth instance sharing
        // the middle depth exactly: the view row that measures depth has no x term, so
        // two instances differing only in x tie on the sort's most significant level and
        // are separated only by the levels below it. Their x offsets do move them apart
        // in eye distance, which is what lets a cut land between them below.
        let middle_origin = Vec3::new(1.5, 0.0, -3.0);
        let twin_origin = Vec3::new(-1.5, 0.0, -3.0);
        let near = create_instance(&mut state.gpu_scene, prototype, Vec3::ZERO, 0);
        let middle = create_instance(&mut state.gpu_scene, prototype, middle_origin, 0);
        let twin = create_instance(&mut state.gpu_scene, prototype, twin_origin, 0);
        let far = create_instance(
            &mut state.gpu_scene,
            prototype,
            Vec3::new(0.0, 0.0, -6.0),
            0,
        );
        let slot = |handle: crate::GpuSceneInstanceHandle| handle.raw().index;

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

        let view = SceneVisibilityView::new(&device, &descriptors, &visibility, 64, 256, 2)
            .expect("view lists");
        let open = hzb_image(&device, 1.0);
        let chain = TransparentChain {
            cull: &cull,
            traversal: &traversal,
            sort: TransparentSortPipelines {
                keys: &keys,
                histogram: &histogram,
                scan: &scan,
                scatter: &scatter,
                reorder: &reorder,
            },
        };

        let fine = sorted_draws(
            &device,
            &chain,
            &view,
            &mut state,
            &open,
            &address_ubo,
            &visibility,
            0.0,
        );
        let order = instance_order(&fine);
        assert_eq!(order.len(), 4, "every instance draws: {fine:?}");

        // Back-to-front: every far draw precedes every middle-depth draw, which precede
        // every near draw.
        let position = |order: &[u32], slot: u32| order.iter().position(|entry| *entry == slot);
        let last = |order: &[u32], slot: u32| order.iter().rposition(|entry| *entry == slot);
        let middle_first = position(&order, slot(middle))
            .expect("middle drawn")
            .min(position(&order, slot(twin)).expect("twin drawn"));
        let middle_last = last(&order, slot(middle))
            .expect("middle drawn")
            .max(last(&order, slot(twin)).expect("twin drawn"));
        assert!(
            last(&order, slot(far)).expect("far drawn") < middle_first,
            "far draws before the middle depth: {order:?}"
        );
        assert!(
            middle_last < position(&order, slot(near)).expect("near drawn"),
            "the middle depth draws before near: {order:?}"
        );

        // The tie-break half. Two instances at one depth are ordered by their persistent
        // scene slot, descending — the sort is ascending by (depth, slot) and the reorder
        // walks it in reverse. Nothing about the traversal's atomic emission order enters
        // it, which is what makes the order reproducible frame to frame.
        let middle_at = position(&order, slot(middle)).expect("middle drawn");
        let twin_at = position(&order, slot(twin)).expect("twin drawn");
        assert_eq!(
            middle_at > twin_at,
            slot(middle) < slot(twin),
            "a depth tie draws in descending scene-slot order: {order:?}"
        );

        // Perturb the record stream: a fifth instance at a fourth depth, which lands
        // between the far and middle groups and shifts every later record index.
        let inserted = create_instance(
            &mut state.gpu_scene,
            prototype,
            Vec3::new(0.0, 0.0, -4.5),
            0,
        );
        let churned = sorted_draws(
            &device,
            &chain,
            &view,
            &mut state,
            &open,
            &address_ubo,
            &visibility,
            0.0,
        );
        assert!(
            churned.len() > fine.len(),
            "the inserted instance joins the stream: {churned:?}"
        );
        // Interleaved, not appended: an insertion that landed at one end would satisfy the
        // stability check below without ever exercising it.
        let churned_order = instance_order(&churned);
        let churned_middle_first = position(&churned_order, slot(middle))
            .expect("middle drawn")
            .min(position(&churned_order, slot(twin)).expect("twin drawn"));
        assert!(
            last(&churned_order, slot(inserted)).expect("inserted drawn") < churned_middle_first,
            "the inserted depth draws before the middle depth: {churned_order:?}"
        );
        // Stability under the change: drop the newcomer's draws and the sequence that
        // remains is the one from before it existed, position for position.
        let without_inserted: Vec<SortedDraw> = churned
            .iter()
            .copied()
            .filter(|draw| draw.instance != slot(inserted))
            .collect();
        assert_eq!(
            without_inserted, fine,
            "a stream change moves no other draw: {fine:?} then {churned:?}"
        );

        // Perturb the hierarchy: the same instances, cut differently. A node refines while
        // its projected error exceeds the threshold, and that error falls with the eye
        // distance — so a threshold between the two tied instances' errors refines one of
        // them and leaves the other at its guaranteed root. The pair then draws different
        // pages at one depth, which is the only arrangement where the page key level could
        // reorder a depth tie at all. It sits below the instance level, so it must not.
        let root_node = &hierarchy.nodes[hierarchy
            .pages
            .iter()
            .find(|page| page.guaranteed_root)
            .expect("root page")
            .node as usize];
        let projected = |origin: Vec3| {
            (root_node.appearance_error.total as f32 / 65_536.0) * 100_000.0
                / (origin - Vec3::new(0.5, 0.5, 5.0)).length().max(0.05)
        };
        let split = (projected(middle_origin) * projected(twin_origin)).sqrt();
        let split_cut = sorted_draws(
            &device,
            &chain,
            &view,
            &mut state,
            &open,
            &address_ubo,
            &visibility,
            split,
        );
        let page_of = |draws: &[SortedDraw], instance: u32| {
            draws
                .iter()
                .find(|draw| draw.instance == instance)
                .expect("drawn")
                .page
        };
        assert_ne!(
            page_of(&split_cut, slot(middle)),
            page_of(&split_cut, slot(twin)),
            "the threshold refines one tied instance and not the other: {split_cut:?}"
        );
        assert_eq!(
            instance_order(&split_cut),
            churned_order,
            "a cut change moves no instance: {churned:?} then {split_cut:?}"
        );

        // The other half of a stream change: a record LEAVING it. The departing instance
        // holds the lowest scene slot, so every survivor moves down a place in the levels
        // below depth — the shift an insertion at the highest slot cannot produce, and the
        // one that catches a key level reading its own position instead of its record.
        state
            .gpu_scene
            .apply_world_delta(WORLD, GpuSceneWorldDelta::RemoveInstance(near))
            .expect("remove");
        let thinned = sorted_draws(
            &device,
            &chain,
            &view,
            &mut state,
            &open,
            &address_ubo,
            &visibility,
            0.0,
        );
        let without_near: Vec<SortedDraw> = churned
            .iter()
            .copied()
            .filter(|draw| draw.instance != slot(near))
            .collect();
        assert!(
            without_near.len() < churned.len(),
            "the departing instance was drawing: {churned:?}"
        );
        assert_eq!(
            thinned, without_near,
            "a record leaving the stream moves no other draw: {churned:?} then {thinned:?}"
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
        drop(state);
        drop(visibility);
        drop(descriptors);
    }
    device.wait_idle().expect("idle before teardown");
    drop(device);
    assert_eq!(validation_issue_count(), before);
}
