use std::mem::{offset_of, size_of};

use super::*;
use ash::vk::Handle;

fn apply_access(
    resource: &mut RgResourceState,
    target: RgUsageInfo,
    barriers: &mut DerivedBarriers,
) {
    apply_access_queued(
        resource,
        target,
        None,
        0,
        RgQueueAssignment::Graphics,
        0,
        barriers,
    );
}

fn image_state(layout: vk::ImageLayout) -> RgResourceState {
    let mut r = RgResourceState {
        is_image: true,
        image: vk::Image::null(),
        layout,
        ..RgResourceState::default()
    };
    seed_image_state(&mut r);
    r
}

fn buffer_state() -> RgResourceState {
    RgResourceState {
        is_image: false,
        buffer: vk::Buffer::null(),
        ..RgResourceState::default()
    }
}

#[test]
fn pipeline_statistics_are_reserved_only_for_graphics_batches() {
    assert!(records_pipeline_statistics(RgQueueAssignment::Graphics));
    assert!(!records_pipeline_statistics(
        RgQueueAssignment::AsyncCompute
    ));
}

fn async_compute(name: &'static str) -> RgPass {
    RgPass::compute(name).queue(RgQueuePreference::AsyncCompute)
}

fn owned_buffer_state(queue: RgQueueAssignment, family: u32) -> RgExternalBufferState {
    RgExternalBufferState {
        accesses: vec![RgBufferAccessState {
            start: 0,
            end: 256,
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_READ,
            is_write: false,
            queue_family: family,
            queue,
            last_pass: None,
        }],
    }
}

fn owned_image_state(
    layout: vk::ImageLayout,
    queue: RgQueueAssignment,
    family: u32,
) -> RgExternalState {
    RgExternalState {
        layout,
        queue_family: Some(family),
        queue: Some(queue),
        stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
        access: vk::AccessFlags2::SHADER_SAMPLED_READ,
        was_write: false,
        touched: true,
    }
}

#[test]
fn usage_info_matches_the_golden_table() {
    let cases = [
        (
            RgUsage::ColorWrite,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            true,
        ),
        (
            RgUsage::DepthWrite,
            vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS,
            vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
            vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
            true,
        ),
        (
            RgUsage::SampledRead,
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            false,
        ),
        (
            RgUsage::StorageWriteCompute,
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_WRITE,
            vk::ImageLayout::UNDEFINED,
            true,
        ),
        (
            RgUsage::StorageReadCompute,
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ,
            vk::ImageLayout::UNDEFINED,
            false,
        ),
        (
            RgUsage::StorageReadFragment,
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ,
            vk::ImageLayout::UNDEFINED,
            false,
        ),
        (
            RgUsage::StorageReadWriteCompute,
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            vk::ImageLayout::UNDEFINED,
            true,
        ),
        (
            RgUsage::StorageImageRwCompute,
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            vk::ImageLayout::GENERAL,
            true,
        ),
        (
            RgUsage::SampledReadCompute,
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            false,
        ),
        (
            RgUsage::TransferRead,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
            vk::ImageLayout::UNDEFINED,
            false,
        ),
        (
            RgUsage::TransferWrite,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_WRITE,
            vk::ImageLayout::UNDEFINED,
            true,
        ),
        (
            RgUsage::VertexInputRead,
            vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
            vk::AccessFlags2::VERTEX_ATTRIBUTE_READ,
            vk::ImageLayout::UNDEFINED,
            false,
        ),
        (
            RgUsage::AccelStructBuildRead,
            vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR,
            vk::AccessFlags2::SHADER_READ,
            vk::ImageLayout::UNDEFINED,
            false,
        ),
        (
            RgUsage::IndexInputRead,
            vk::PipelineStageFlags2::INDEX_INPUT,
            vk::AccessFlags2::INDEX_READ,
            vk::ImageLayout::UNDEFINED,
            false,
        ),
        (
            RgUsage::ShaderDeviceAddressRead,
            vk::PipelineStageFlags2::ALL_COMMANDS,
            vk::AccessFlags2::SHADER_READ,
            vk::ImageLayout::UNDEFINED,
            false,
        ),
        (
            RgUsage::IndirectCommandRead,
            vk::PipelineStageFlags2::DRAW_INDIRECT,
            vk::AccessFlags2::INDIRECT_COMMAND_READ,
            vk::ImageLayout::UNDEFINED,
            false,
        ),
        (
            RgUsage::IndirectCountRead,
            vk::PipelineStageFlags2::DRAW_INDIRECT,
            vk::AccessFlags2::INDIRECT_COMMAND_READ,
            vk::ImageLayout::UNDEFINED,
            false,
        ),
    ];
    for (usage, stage, access, layout, is_write) in cases {
        let info = usage_info(usage);
        assert_eq!(info.stage, stage, "stage for {usage:?}");
        assert_eq!(info.access, access, "access for {usage:?}");
        assert_eq!(info.layout, layout, "layout for {usage:?}");
        assert_eq!(info.is_write, is_write, "is_write for {usage:?}");
    }
}

#[test]
fn image_barrier_on_layout_change() {
    // UNDEFINED → sampled-read is a layout change with no hazard (fresh image).
    let mut r = image_state(vk::ImageLayout::UNDEFINED);
    let mut barriers = DerivedBarriers::default();
    apply_access(&mut r, usage_info(RgUsage::SampledRead), &mut barriers);

    assert_eq!(barriers.image.len(), 1);
    assert!(barriers.buffer.is_empty());
    let b = barriers.image[0];
    assert_eq!(b.old_layout, vk::ImageLayout::UNDEFINED);
    assert_eq!(b.new_layout, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
    assert_eq!(b.src_stage_mask, vk::PipelineStageFlags2::TOP_OF_PIPE);
    assert_eq!(b.dst_stage_mask, vk::PipelineStageFlags2::FRAGMENT_SHADER);
    assert_eq!(b.subresource_range.base_mip_level, 0);
    assert_eq!(b.subresource_range.level_count, vk::REMAINING_MIP_LEVELS);
    assert_eq!(b.subresource_range.base_array_layer, 0);
    assert_eq!(b.subresource_range.layer_count, vk::REMAINING_ARRAY_LAYERS);
    assert_eq!(r.layout, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
}

#[test]
fn image_barrier_on_write_after_touch_hazard() {
    // Two compute storage-image writes to the same GENERAL image: the second is a
    // write-after-write hazard with no layout change.
    let mut r = image_state(vk::ImageLayout::GENERAL);
    let mut barriers = DerivedBarriers::default();
    apply_access(
        &mut r,
        usage_info(RgUsage::StorageImageRwCompute),
        &mut barriers,
    );
    // First touch into GENERAL is a layout change (UNDEFINED-seeded? no: started at
    // GENERAL, so no layout change — but `touched` is false, so no barrier).
    assert!(
        barriers.is_empty(),
        "first write into matching layout needs no barrier"
    );

    apply_access(
        &mut r,
        usage_info(RgUsage::StorageImageRwCompute),
        &mut barriers,
    );
    assert_eq!(barriers.image.len(), 1, "second write is a WAW hazard");
    let b = barriers.image[0];
    assert_eq!(b.old_layout, vk::ImageLayout::GENERAL);
    assert_eq!(
        b.new_layout,
        vk::ImageLayout::GENERAL,
        "no layout change, layout preserved"
    );
}

#[test]
fn image_barrier_on_read_after_write_hazard() {
    // Compute storage-image write, then a compute sampled read of the same image.
    let mut r = image_state(vk::ImageLayout::GENERAL);
    let mut barriers = DerivedBarriers::default();
    apply_access(
        &mut r,
        usage_info(RgUsage::StorageImageRwCompute),
        &mut barriers,
    );
    barriers = DerivedBarriers::default();

    apply_access(
        &mut r,
        usage_info(RgUsage::SampledReadCompute),
        &mut barriers,
    );
    assert_eq!(
        barriers.image.len(),
        1,
        "read after write is a hazard and a layout change"
    );
    let b = barriers.image[0];
    assert_eq!(b.old_layout, vk::ImageLayout::GENERAL);
    assert_eq!(b.new_layout, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
    assert_eq!(
        b.src_access_mask & vk::AccessFlags2::SHADER_STORAGE_WRITE,
        vk::AccessFlags2::SHADER_STORAGE_WRITE
    );
    assert_eq!(b.dst_access_mask, vk::AccessFlags2::SHADER_SAMPLED_READ);
}

#[test]
fn no_image_barrier_on_read_after_read() {
    // Two fragment sampled-reads of an already-SHADER_READ_ONLY image: no layout
    // change, no hazard, so no barrier at all (the false-barrier guard).
    let mut r = image_state(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
    let mut barriers = DerivedBarriers::default();
    apply_access(&mut r, usage_info(RgUsage::SampledRead), &mut barriers);
    assert!(
        barriers.is_empty(),
        "first read needs no barrier (already in layout)"
    );
    apply_access(&mut r, usage_info(RgUsage::SampledRead), &mut barriers);
    assert!(barriers.is_empty(), "read after read emits no barrier");
}

#[test]
fn buffer_memory_barrier_on_hazard_only() {
    // Compute write, then a vertex-input read: a read-after-write hazard → one
    // memory barrier. No image barrier ever for a buffer.
    let mut r = buffer_state();
    let mut barriers = DerivedBarriers::default();
    apply_access(
        &mut r,
        usage_info(RgUsage::StorageWriteCompute),
        &mut barriers,
    );
    assert!(barriers.is_empty(), "first buffer write is no hazard");

    apply_access(&mut r, usage_info(RgUsage::VertexInputRead), &mut barriers);
    assert_eq!(
        barriers.buffer.len(),
        1,
        "read after write is a buffer hazard"
    );
    assert!(
        barriers.image.is_empty(),
        "buffers never emit image barriers"
    );
    let b = barriers.buffer[0];
    assert_eq!(b.src_stage_mask, vk::PipelineStageFlags2::COMPUTE_SHADER);
    assert_eq!(b.src_access_mask, vk::AccessFlags2::SHADER_STORAGE_WRITE);
    assert_eq!(
        b.dst_stage_mask,
        vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT
    );
    assert_eq!(b.dst_access_mask, vk::AccessFlags2::VERTEX_ATTRIBUTE_READ);
}

#[test]
fn index_input_read_after_compute_write_is_one_memory_barrier() {
    // The tessellator's generated index buffer: a compute write ordered ahead of the
    // indexed draw's index-input read — exactly one memory barrier, no image barrier.
    let mut r = buffer_state();
    let mut barriers = DerivedBarriers::default();
    apply_access(
        &mut r,
        usage_info(RgUsage::StorageWriteCompute),
        &mut barriers,
    );
    assert!(barriers.is_empty(), "first buffer write is no hazard");

    apply_access(&mut r, usage_info(RgUsage::IndexInputRead), &mut barriers);
    assert_eq!(
        barriers.buffer.len(),
        1,
        "index read after write is a hazard"
    );
    assert!(
        barriers.image.is_empty(),
        "buffers never emit image barriers"
    );
    let b = barriers.buffer[0];
    assert_eq!(b.src_stage_mask, vk::PipelineStageFlags2::COMPUTE_SHADER);
    assert_eq!(b.src_access_mask, vk::AccessFlags2::SHADER_STORAGE_WRITE);
    assert_eq!(b.dst_stage_mask, vk::PipelineStageFlags2::INDEX_INPUT);
    assert_eq!(b.dst_access_mask, vk::AccessFlags2::INDEX_READ);
}

#[test]
fn indirect_command_read_after_compute_write_is_one_memory_barrier() {
    // The no-op indirect round-trip: the args/count buffer a compute pass writes, ordered
    // ahead of the indirect dispatch/draw that consumes it — proven at the derivation layer,
    // no device.
    let mut r = buffer_state();
    let mut barriers = DerivedBarriers::default();
    apply_access(
        &mut r,
        usage_info(RgUsage::StorageWriteCompute),
        &mut barriers,
    );
    assert!(barriers.is_empty(), "first buffer write is no hazard");

    apply_access(
        &mut r,
        usage_info(RgUsage::IndirectCommandRead),
        &mut barriers,
    );
    assert_eq!(
        barriers.buffer.len(),
        1,
        "indirect-args read after write is a hazard"
    );
    assert!(
        barriers.image.is_empty(),
        "buffers never emit image barriers"
    );
    let b = barriers.buffer[0];
    assert_eq!(b.src_stage_mask, vk::PipelineStageFlags2::COMPUTE_SHADER);
    assert_eq!(b.src_access_mask, vk::AccessFlags2::SHADER_STORAGE_WRITE);
    assert_eq!(b.dst_stage_mask, vk::PipelineStageFlags2::DRAW_INDIRECT);
    assert_eq!(b.dst_access_mask, vk::AccessFlags2::INDIRECT_COMMAND_READ);
}

#[test]
fn indexed_indirect_abi_and_argument_barrier_ranges_are_byte_locked() {
    assert_eq!(size_of::<vk::DrawIndexedIndirectCommand>(), 20);
    assert_eq!(offset_of!(vk::DrawIndexedIndirectCommand, index_count), 0);
    assert_eq!(
        offset_of!(vk::DrawIndexedIndirectCommand, instance_count),
        4
    );
    assert_eq!(offset_of!(vk::DrawIndexedIndirectCommand, first_index), 8);
    assert_eq!(
        offset_of!(vk::DrawIndexedIndirectCommand, vertex_offset),
        12
    );
    assert_eq!(
        offset_of!(vk::DrawIndexedIndirectCommand, first_instance),
        16
    );

    let mut resource = RgResourceState {
        is_image: false,
        buffer: vk::Buffer::null(),
        buffer_size: 128,
        ..RgResourceState::default()
    };
    let mut write = DerivedBarriers::default();
    apply_access_queued(
        &mut resource,
        usage_info(RgUsage::StorageWriteCompute),
        Some(RgBufferRange::new(64, 24).unwrap()),
        0,
        RgQueueAssignment::Graphics,
        0,
        &mut write,
    );
    assert!(write.is_empty());

    let mut arguments = DerivedBarriers::default();
    apply_access_queued(
        &mut resource,
        usage_info(RgUsage::IndirectCommandRead),
        Some(RgBufferRange::new(64, 20).unwrap()),
        1,
        RgQueueAssignment::Graphics,
        0,
        &mut arguments,
    );
    assert_eq!(arguments.buffer.len(), 1);
    assert_eq!(arguments.buffer[0].offset, 64);
    assert_eq!(arguments.buffer[0].size, 20);
    assert_eq!(
        arguments.buffer[0].dst_stage_mask,
        vk::PipelineStageFlags2::DRAW_INDIRECT
    );
    assert_eq!(
        arguments.buffer[0].dst_access_mask,
        vk::AccessFlags2::INDIRECT_COMMAND_READ
    );

    let mut count = DerivedBarriers::default();
    apply_access_queued(
        &mut resource,
        usage_info(RgUsage::IndirectCountRead),
        Some(RgBufferRange::new(84, 4).unwrap()),
        1,
        RgQueueAssignment::Graphics,
        0,
        &mut count,
    );
    assert_eq!(count.buffer.len(), 1);
    assert_eq!(count.buffer[0].offset, 84);
    assert_eq!(count.buffer[0].size, 4);
    assert_eq!(
        count.buffer[0].dst_stage_mask,
        vk::PipelineStageFlags2::DRAW_INDIRECT
    );
    assert_eq!(
        count.buffer[0].dst_access_mask,
        vk::AccessFlags2::INDIRECT_COMMAND_READ
    );
}

#[test]
fn compute_write_transitions_cover_every_buffer_consumer() {
    let consumers = [
        (
            RgUsage::StorageReadCompute,
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ,
        ),
        (
            RgUsage::TransferRead,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
        ),
        (
            RgUsage::VertexInputRead,
            vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
            vk::AccessFlags2::VERTEX_ATTRIBUTE_READ,
        ),
        (
            RgUsage::IndexInputRead,
            vk::PipelineStageFlags2::INDEX_INPUT,
            vk::AccessFlags2::INDEX_READ,
        ),
        (
            RgUsage::ShaderDeviceAddressRead,
            vk::PipelineStageFlags2::ALL_COMMANDS,
            vk::AccessFlags2::SHADER_READ,
        ),
        (
            RgUsage::IndirectCommandRead,
            vk::PipelineStageFlags2::DRAW_INDIRECT,
            vk::AccessFlags2::INDIRECT_COMMAND_READ,
        ),
        (
            RgUsage::IndirectCountRead,
            vk::PipelineStageFlags2::DRAW_INDIRECT,
            vk::AccessFlags2::INDIRECT_COMMAND_READ,
        ),
        (
            RgUsage::AccelStructBuildRead,
            vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR,
            vk::AccessFlags2::SHADER_READ,
        ),
    ];
    for (usage, stage, access) in consumers {
        let mut resource = buffer_state();
        let mut write = DerivedBarriers::default();
        apply_access(
            &mut resource,
            usage_info(RgUsage::StorageWriteCompute),
            &mut write,
        );
        assert!(write.is_empty());

        let mut read = DerivedBarriers::default();
        apply_access(&mut resource, usage_info(usage), &mut read);
        assert_eq!(read.buffer.len(), 1, "transition for {usage:?}");
        let barrier = read.buffer[0];
        assert_eq!(
            barrier.src_stage_mask,
            vk::PipelineStageFlags2::COMPUTE_SHADER
        );
        assert_eq!(
            barrier.src_access_mask,
            vk::AccessFlags2::SHADER_STORAGE_WRITE
        );
        assert_eq!(barrier.dst_stage_mask, stage);
        assert_eq!(barrier.dst_access_mask, access);
    }
}

#[test]
fn transfer_write_transitions_to_compute_read() {
    let mut resource = buffer_state();
    let mut barriers = DerivedBarriers::default();
    apply_access(
        &mut resource,
        usage_info(RgUsage::TransferWrite),
        &mut barriers,
    );
    assert!(barriers.is_empty());

    let mut barriers = DerivedBarriers::default();
    apply_access(
        &mut resource,
        usage_info(RgUsage::StorageReadCompute),
        &mut barriers,
    );
    let barrier = barriers.buffer[0];
    assert_eq!(barrier.src_stage_mask, vk::PipelineStageFlags2::COPY);
    assert_eq!(barrier.src_access_mask, vk::AccessFlags2::TRANSFER_WRITE);
    assert_eq!(
        barrier.dst_stage_mask,
        vk::PipelineStageFlags2::COMPUTE_SHADER
    );
    assert_eq!(
        barrier.dst_access_mask,
        vk::AccessFlags2::SHADER_STORAGE_READ
    );
}

#[test]
fn byte_ranges_barrier_only_the_overlapping_hazard() {
    let mut resource = RgResourceState {
        is_image: false,
        buffer: vk::Buffer::null(),
        buffer_size: 256,
        ..RgResourceState::default()
    };
    let mut first = DerivedBarriers::default();
    apply_access_queued(
        &mut resource,
        usage_info(RgUsage::StorageWriteCompute),
        Some(RgBufferRange::new(0, 64).unwrap()),
        0,
        RgQueueAssignment::Graphics,
        0,
        &mut first,
    );
    assert!(first.is_empty());

    let mut disjoint = DerivedBarriers::default();
    apply_access_queued(
        &mut resource,
        usage_info(RgUsage::VertexInputRead),
        Some(RgBufferRange::new(64, 64).unwrap()),
        1,
        RgQueueAssignment::Graphics,
        0,
        &mut disjoint,
    );
    assert!(disjoint.is_empty());

    let mut overlap = DerivedBarriers::default();
    apply_access_queued(
        &mut resource,
        usage_info(RgUsage::IndirectCommandRead),
        Some(RgBufferRange::new(32, 64).unwrap()),
        2,
        RgQueueAssignment::Graphics,
        0,
        &mut overlap,
    );
    assert_eq!(overlap.buffer.len(), 1);
    assert_eq!(overlap.buffer[0].offset, 32);
    assert_eq!(overlap.buffer[0].size, 32);
}

#[test]
fn buffer_ranges_reject_empty_and_wrapping_inputs() {
    assert_eq!(RgBufferRange::new(4, 0), Err(RgBufferRangeError::Empty));
    assert_eq!(
        RgBufferRange::new(u64::MAX - 1, 4),
        Err(RgBufferRangeError::EndOverflow)
    );
}

#[test]
fn async_compute_schedule_pairs_release_and_acquire() {
    let mut graph = RenderGraph::new();
    let state =
        graph.alloc_external_buffer_state(owned_buffer_state(RgQueueAssignment::AsyncCompute, 5));
    let buffer = graph.import_buffer(vk::Buffer::null(), Some(state));
    let range = RgBufferRange::new(32, 64).unwrap();
    graph.add_pass(async_compute("count").access_buffer(
        buffer,
        range,
        RgUsage::StorageWriteCompute,
    ));
    graph.add_pass(
        RgPass::graphics("draw", vk::Extent2D::default()).access_buffer(
            buffer,
            range,
            RgUsage::IndirectCountRead,
        ),
    );

    let schedule = graph.barrier_schedule(RgQueueFamilies {
        graphics: 2,
        async_compute: Some(5),
    });
    assert_eq!(schedule[0].queue, RgQueueAssignment::AsyncCompute);
    assert_eq!(schedule[1].queue, RgQueueAssignment::Graphics);
    assert_eq!(schedule[1].wait_for_passes, [0]);
    assert_eq!(schedule[0].after_buffers.len(), 1);
    assert_eq!(schedule[1].before_buffers.len(), 1);
    let release = schedule[0].after_buffers[0];
    assert_eq!(release.src_queue_family_index, 5);
    assert_eq!(release.dst_queue_family_index, 2);
    assert_eq!(
        release.src_stage_mask,
        vk::PipelineStageFlags2::COMPUTE_SHADER
    );
    assert_eq!(release.dst_stage_mask, vk::PipelineStageFlags2::NONE);
    assert_eq!(release.offset, 32);
    assert_eq!(release.size, 64);
    let acquire = schedule[1].before_buffers[0];
    assert_eq!(acquire.src_queue_family_index, 5);
    assert_eq!(acquire.dst_queue_family_index, 2);
    assert_eq!(acquire.src_stage_mask, vk::PipelineStageFlags2::NONE);
    assert_eq!(
        acquire.dst_stage_mask,
        vk::PipelineStageFlags2::DRAW_INDIRECT
    );
    assert_eq!(acquire.offset, 32);
    assert_eq!(acquire.size, 64);
}

#[test]
fn async_compute_preference_falls_back_to_the_same_graphics_pass() {
    let mut graph = RenderGraph::new();
    let buffer = graph.import_buffer(vk::Buffer::null(), None);
    graph.add_pass(async_compute("count").access(buffer, RgUsage::StorageWriteCompute));
    graph.add_pass(async_compute("consume").access(buffer, RgUsage::StorageReadCompute));

    let schedule = graph.barrier_schedule(RgQueueFamilies::graphics_only(3));
    assert_eq!(schedule[0].queue, RgQueueAssignment::Graphics);
    assert_eq!(schedule[1].queue, RgQueueAssignment::Graphics);
    assert!(schedule[1].wait_for_passes.is_empty());
    assert!(schedule[0].after_buffers.is_empty());
    assert_eq!(schedule[1].before_buffers.len(), 1);
    assert_eq!(
        schedule[1].before_buffers[0].src_queue_family_index,
        vk::QUEUE_FAMILY_IGNORED
    );
}

#[test]
fn graph_owned_compute_work_resolves_to_the_independent_queue() {
    let mut graph = RenderGraph::new();
    let scratch = graph.register_buffer(RgBufferResource {
        buffer: vk::Buffer::from_raw(42),
        size: 4096,
        usage: vk::BufferUsageFlags::STORAGE_BUFFER,
        lifetime: RgBufferLifetime::Transient,
    });
    graph.add_pass(
        async_compute("tess-factor")
            .access(scratch, RgUsage::StorageWriteCompute)
            .body(|_, _| {}),
    );

    assert_eq!(
        graph.queue_assignments(RgQueueFamilies {
            graphics: 0,
            async_compute: Some(1),
        }),
        [RgQueueAssignment::AsyncCompute]
    );
    assert_eq!(
        graph.queue_assignments(RgQueueFamilies::graphics_only(0)),
        [RgQueueAssignment::Graphics]
    );
}

#[test]
fn graphics_output_feeds_async_post_compute_in_a_production_graph_shape() {
    let mut graph = RenderGraph::new();
    let color = graph.import_image(
        vk::Image::from_raw(7),
        vk::ImageView::from_raw(8),
        vk::ImageAspectFlags::COLOR,
        vk::ImageLayout::UNDEFINED,
        None,
    );
    graph.add_pass(
        RgPass::graphics("scene", vk::Extent2D::default()).color(RgAttachment::clear_store(color)),
    );
    graph.add_pass(async_compute("tonemap").access(color, RgUsage::StorageImageRwCompute));

    assert_eq!(
        graph.queue_assignments(RgQueueFamilies {
            graphics: 0,
            async_compute: Some(1),
        }),
        [RgQueueAssignment::Graphics, RgQueueAssignment::AsyncCompute]
    );
    assert_eq!(
        graph.queue_assignments(RgQueueFamilies::graphics_only(0)),
        [RgQueueAssignment::Graphics, RgQueueAssignment::Graphics]
    );
}

#[test]
fn submission_plan_batches_contiguous_queues_and_links_dependencies() {
    let mut graph = RenderGraph::new();
    let first_state =
        graph.alloc_external_buffer_state(owned_buffer_state(RgQueueAssignment::AsyncCompute, 5));
    let first = graph.import_buffer(vk::Buffer::from_raw(1), Some(first_state));
    let second_state =
        graph.alloc_external_buffer_state(owned_buffer_state(RgQueueAssignment::Graphics, 2));
    let second = graph.import_buffer(vk::Buffer::from_raw(2), Some(second_state));
    let whole = RgBufferRange::new(0, 256).unwrap();
    graph.add_pass(async_compute("async-produce").access_buffer(
        first,
        whole,
        RgUsage::StorageWriteCompute,
    ));
    graph.add_pass(
        RgPass::compute("graphics-transfer")
            .access_buffer(first, whole, RgUsage::StorageReadCompute)
            .access_buffer(second, whole, RgUsage::StorageWriteCompute),
    );
    graph.add_pass(async_compute("async-consume").access_buffer(
        second,
        whole,
        RgUsage::StorageReadCompute,
    ));

    let plan = graph.submission_plan(RgQueueFamilies {
        graphics: 2,
        async_compute: Some(5),
    });
    assert_eq!(plan.graphics_batch_count(), 1);
    assert_eq!(plan.compute_batch_count(), 2);
    assert_eq!(plan.batches.len(), 3);
    assert_eq!(plan.batches[0].queue, RgQueueAssignment::AsyncCompute);
    assert_eq!(plan.batches[0].passes, 0..1);
    assert!(plan.batches[0].wait_for_batches.is_empty());
    assert_eq!(plan.batches[1].queue, RgQueueAssignment::Graphics);
    assert_eq!(plan.batches[1].passes, 1..2);
    assert_eq!(plan.batches[1].wait_for_batches, [0]);
    assert_eq!(plan.batches[2].queue, RgQueueAssignment::AsyncCompute);
    assert_eq!(plan.batches[2].passes, 2..3);
    assert_eq!(plan.batches[2].wait_for_batches, [1]);
}

#[test]
fn submission_plan_keeps_adjacent_passes_in_one_queue_batch() {
    let mut graph = RenderGraph::new();
    let slot = graph.alloc_external_state(owned_image_state(
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        RgQueueAssignment::AsyncCompute,
        3,
    ));
    let image = graph.import_image(
        vk::Image::null(),
        vk::ImageView::null(),
        vk::ImageAspectFlags::COLOR,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        Some(slot),
    );
    graph.add_pass(async_compute("compute-a").access(image, RgUsage::SampledReadCompute));
    graph.add_pass(async_compute("compute-b").access(image, RgUsage::SampledReadCompute));
    graph.add_pass(RgPass::compute("graphics-a"));
    graph.add_pass(RgPass::compute("graphics-b"));

    let plan = graph.submission_plan(RgQueueFamilies {
        graphics: 1,
        async_compute: Some(3),
    });
    assert_eq!(plan.batches.len(), 2);
    assert_eq!(plan.batches[0].passes, 0..2);
    assert_eq!(plan.batches[1].passes, 2..4);
    assert!(plan.batches[0].wait_for_batches.is_empty());
    assert!(plan.batches[1].wait_for_batches.is_empty());
}

#[test]
fn same_family_cross_frame_graphics_to_async_uses_graphics_prologue() {
    let mut graph = RenderGraph::new();
    let slot = graph.alloc_external_state(owned_image_state(
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        RgQueueAssignment::Graphics,
        7,
    ));
    let image = graph.import_image(
        vk::Image::null(),
        vk::ImageView::null(),
        vk::ImageAspectFlags::COLOR,
        vk::ImageLayout::UNDEFINED,
        Some(slot),
    );
    graph.add_pass(async_compute("async-write").access(image, RgUsage::StorageImageRwCompute));

    let plan = graph.submission_plan(RgQueueFamilies {
        graphics: 7,
        async_compute: Some(7),
    });
    assert_eq!(plan.batches.len(), 2);
    assert_eq!(plan.batches[0].queue, RgQueueAssignment::Graphics);
    assert!(plan.batches[0].passes.is_empty());
    assert_eq!(plan.batches[1].queue, RgQueueAssignment::AsyncCompute);
    assert_eq!(plan.batches[1].wait_for_batches, [0]);
    let acquire = plan.passes[0].before_images[0];
    assert_eq!(acquire.src_queue_family_index, vk::QUEUE_FAMILY_IGNORED);
    assert_eq!(acquire.dst_queue_family_index, vk::QUEUE_FAMILY_IGNORED);
}

#[test]
fn same_family_cross_frame_async_to_graphics_uses_compute_prologue() {
    let mut graph = RenderGraph::new();
    let slot = graph.alloc_external_state(owned_image_state(
        vk::ImageLayout::GENERAL,
        RgQueueAssignment::AsyncCompute,
        7,
    ));
    let image = graph.import_image(
        vk::Image::null(),
        vk::ImageView::null(),
        vk::ImageAspectFlags::COLOR,
        vk::ImageLayout::UNDEFINED,
        Some(slot),
    );
    graph.add_pass(RgPass::compute("graphics-read").access(image, RgUsage::SampledReadCompute));

    let plan = graph.submission_plan(RgQueueFamilies {
        graphics: 7,
        async_compute: Some(7),
    });
    assert_eq!(plan.batches.len(), 2);
    assert_eq!(plan.batches[0].queue, RgQueueAssignment::AsyncCompute);
    assert!(plan.batches[0].passes.is_empty());
    assert_eq!(plan.batches[1].queue, RgQueueAssignment::Graphics);
    assert_eq!(plan.batches[1].wait_for_batches, [0]);
    let acquire = plan.passes[0].before_images[0];
    assert_eq!(acquire.src_queue_family_index, vk::QUEUE_FAMILY_IGNORED);
    assert_eq!(acquire.dst_queue_family_index, vk::QUEUE_FAMILY_IGNORED);
}

#[test]
fn buffer_no_barrier_on_read_after_read() {
    // Two compute reads of a buffer: no write was seen, so no hazard, no barrier.
    let mut r = buffer_state();
    let mut barriers = DerivedBarriers::default();
    apply_access(
        &mut r,
        usage_info(RgUsage::StorageReadCompute),
        &mut barriers,
    );
    apply_access(
        &mut r,
        usage_info(RgUsage::StorageReadFragment),
        &mut barriers,
    );
    assert!(
        barriers.is_empty(),
        "read after read on a buffer emits no barrier"
    );
}

#[test]
fn buffer_write_after_read_is_a_hazard() {
    // A read then a write: write-after-anything-touched is a hazard.
    let mut r = buffer_state();
    let mut barriers = DerivedBarriers::default();
    apply_access(
        &mut r,
        usage_info(RgUsage::StorageReadCompute),
        &mut barriers,
    );
    assert!(barriers.is_empty());
    apply_access(
        &mut r,
        usage_info(RgUsage::StorageWriteCompute),
        &mut barriers,
    );
    assert_eq!(barriers.buffer.len(), 1, "write after read is a hazard");
}

#[test]
fn seeded_shader_read_image_war_source() {
    // A freshly-imported SHADER_READ_ONLY image seeds its source as a fragment
    // sampled read, so the first write waits on that read (write-after-read).
    let r = image_state(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
    assert_eq!(r.last_stage, vk::PipelineStageFlags2::FRAGMENT_SHADER);
    assert_eq!(r.last_access, vk::AccessFlags2::SHADER_SAMPLED_READ);

    // Now a color write into it: layout change + the WAR source carries through.
    let mut r = r;
    let mut barriers = DerivedBarriers::default();
    apply_access(&mut r, usage_info(RgUsage::ColorWrite), &mut barriers);
    assert_eq!(barriers.image.len(), 1);
    let b = barriers.image[0];
    assert_eq!(b.src_stage_mask, vk::PipelineStageFlags2::FRAGMENT_SHADER);
    assert_eq!(b.src_access_mask, vk::AccessFlags2::SHADER_SAMPLED_READ);
    assert_eq!(b.new_layout, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
}

#[test]
fn seeded_other_layout_image_has_no_war_source() {
    // A non-SHADER_READ_ONLY entry layout has no prior in-frame work to wait on.
    let r = image_state(vk::ImageLayout::GENERAL);
    assert_eq!(r.last_stage, vk::PipelineStageFlags2::TOP_OF_PIPE);
    assert_eq!(r.last_access, vk::AccessFlags2::empty());
}

#[test]
fn multi_pass_skin_to_vertex_to_color_sequence() {
    // The canonical chain: a compute skin write to a buffer, a vertex-input read of
    // that buffer, then a color write to an image. Drive the per-resource state the
    // way the graph does and assert the exact barrier list, in order.
    let mut deformed = buffer_state();
    let mut target = image_state(vk::ImageLayout::UNDEFINED);

    // Pass 0: skin compute write to the deformed buffer — first touch, no barrier.
    let mut p0 = DerivedBarriers::default();
    apply_access(
        &mut deformed,
        usage_info(RgUsage::StorageWriteCompute),
        &mut p0,
    );
    assert!(p0.is_empty());

    // Pass 1: vertex-input read of the deformed buffer — read-after-write hazard →
    // one memory barrier, COMPUTE_SHADER/STORAGE_WRITE → VERTEX_ATTRIBUTE_*.
    let mut p1 = DerivedBarriers::default();
    apply_access(&mut deformed, usage_info(RgUsage::VertexInputRead), &mut p1);
    assert_eq!(p1.buffer.len(), 1);
    assert!(p1.image.is_empty());
    assert_eq!(
        p1.buffer[0].src_stage_mask,
        vk::PipelineStageFlags2::COMPUTE_SHADER
    );
    assert_eq!(
        p1.buffer[0].dst_stage_mask,
        vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT
    );

    // Pass 1 also writes color into the target — UNDEFINED → COLOR_ATTACHMENT, a
    // layout change with no hazard (fresh image), so one image barrier in the same
    // pass alongside the buffer memory barrier.
    apply_access(&mut target, usage_info(RgUsage::ColorWrite), &mut p1);
    assert_eq!(p1.image.len(), 1);
    assert_eq!(p1.image[0].old_layout, vk::ImageLayout::UNDEFINED);
    assert_eq!(
        p1.image[0].new_layout,
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
    );
}

#[test]
fn compute_to_graphics_layout_transition() {
    // Compute writes a storage image (GENERAL), then a graphics pass samples it in
    // a fragment shader (SHADER_READ_ONLY): a compute→graphics hazard + transition.
    let mut r = image_state(vk::ImageLayout::UNDEFINED);

    let mut p0 = DerivedBarriers::default();
    apply_access(&mut r, usage_info(RgUsage::StorageImageRwCompute), &mut p0);
    // UNDEFINED → GENERAL is a layout change → one barrier even though no hazard.
    assert_eq!(p0.image.len(), 1);
    assert_eq!(p0.image[0].new_layout, vk::ImageLayout::GENERAL);

    let mut p1 = DerivedBarriers::default();
    apply_access(&mut r, usage_info(RgUsage::SampledRead), &mut p1);
    assert_eq!(
        p1.image.len(),
        1,
        "compute write → graphics sample needs a barrier"
    );
    let b = p1.image[0];
    assert_eq!(b.src_stage_mask, vk::PipelineStageFlags2::COMPUTE_SHADER);
    assert_eq!(b.dst_stage_mask, vk::PipelineStageFlags2::FRAGMENT_SHADER);
    assert_eq!(b.old_layout, vk::ImageLayout::GENERAL);
    assert_eq!(b.new_layout, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
}

#[test]
fn graphics_to_compute_layout_transition() {
    // A color attachment (COLOR_ATTACHMENT_OPTIMAL) then sampled in a compute shader
    // (SHADER_READ_ONLY): graphics→compute transition + read-after-write hazard.
    let mut r = image_state(vk::ImageLayout::UNDEFINED);
    let mut p0 = DerivedBarriers::default();
    apply_access(&mut r, usage_info(RgUsage::ColorWrite), &mut p0);

    let mut p1 = DerivedBarriers::default();
    apply_access(&mut r, usage_info(RgUsage::SampledReadCompute), &mut p1);
    assert_eq!(p1.image.len(), 1);
    let b = p1.image[0];
    assert_eq!(
        b.src_stage_mask,
        vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT
    );
    assert_eq!(b.src_access_mask, vk::AccessFlags2::COLOR_ATTACHMENT_WRITE);
    assert_eq!(b.dst_stage_mask, vk::PipelineStageFlags2::COMPUTE_SHADER);
    assert_eq!(b.old_layout, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
    assert_eq!(b.new_layout, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
}

#[test]
fn cross_frame_external_state_write_back() {
    // An imported image's exit layout becomes its next-frame entry layout. The graph
    // owns the external slot; after deriving the frame's passes, the slot holds the
    // resolved layout, which seeds the next frame's import.
    let mut graph = RenderGraph::new();
    let slot = graph.alloc_external_state(RgExternalState::new(vk::ImageLayout::UNDEFINED));
    let res = graph.import_image(
        vk::Image::null(),
        vk::ImageView::null(),
        vk::ImageAspectFlags::COLOR,
        vk::ImageLayout::UNDEFINED,
        Some(slot),
    );

    // A graphics pass writes color into it (layout → COLOR_ATTACHMENT_OPTIMAL).
    let pass = RgPass::graphics(
        "scene",
        vk::Extent2D {
            width: 4,
            height: 4,
        },
    )
    .color(RgAttachment::clear_store(res));
    graph.add_pass(pass);

    // Derive (no GPU recording needed for the write-back contract).
    let _ = graph.derive_pass_barriers(
        &graph.passes[0].clone_for_test(),
        0,
        RgQueueAssignment::Graphics,
        0,
    );
    graph.write_external_states();
    assert_eq!(
        graph.external_state(slot).layout,
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        "the exit layout is written back into the external slot"
    );
    assert_eq!(graph.external_state(slot).queue_family, Some(0));

    // Next frame: a fresh import from the same slot seeds the entry layout.
    let mut next = RenderGraph::new();
    next.external_states.push(graph.external_state(slot));
    let res2 = next.import_image(
        vk::Image::null(),
        vk::ImageView::null(),
        vk::ImageAspectFlags::COLOR,
        vk::ImageLayout::UNDEFINED,
        Some(0),
    );
    assert_eq!(
        next.resources[res2.index as usize].layout,
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        "the next frame's entry layout is last frame's exit layout"
    );
    assert_eq!(next.resources[res2.index as usize].queue_family, Some(0));
}

impl RgPass {
    /// A shallow clone of a pass for tests (the body is not cloneable, so it is
    /// dropped). Lets a test re-derive barriers without consuming the graph's pass.
    fn clone_for_test(&self) -> RgPass {
        RgPass {
            name: self.name,
            kind: self.kind,
            queue: self.queue,
            accesses: self.accesses.clone(),
            colors: self.colors.clone(),
            depth: self.depth,
            render_area: self.render_area,
            execute: None,
        }
    }
}

/// A pass index names a position in the graph that produced it. Carried into the next frame's
/// graph it points at whatever pass happens to sit at that index, which can be a *later* one —
/// and a batch that waits on a later batch is a cycle the submit cannot satisfy. The write-back
/// therefore drops it, leaving the queue/family to carry ownership across the frame exactly as it
/// does for an image.
#[test]
fn external_buffer_state_carries_ownership_without_a_pass_index() {
    let mut graph = RenderGraph::new();
    let slot =
        graph.alloc_external_buffer_state(owned_buffer_state(RgQueueAssignment::AsyncCompute, 5));
    let buffer = graph.import_buffer(vk::Buffer::from_raw(7), Some(slot));
    let whole = RgBufferRange::new(0, 256).unwrap();
    graph.add_pass(RgPass::compute("warm-up"));
    graph.add_pass(async_compute("build").access_buffer(
        buffer,
        whole,
        RgUsage::StorageWriteCompute,
    ));
    let families = RgQueueFamilies {
        graphics: 2,
        async_compute: Some(5),
    };
    let (_, exit, _) = graph.compile_barriers(families);
    graph.resources = exit;
    graph.write_external_states();
    let carried = graph.external_buffer_state(slot);
    assert_eq!(carried.accesses.len(), 1);
    assert_eq!(carried.accesses[0].queue, RgQueueAssignment::AsyncCompute);
    assert_eq!(carried.accesses[0].queue_family, 5);
    assert_eq!(carried.accesses[0].last_pass, None);

    // The next frame declares the same buffer's build first and a graphics reader after it. The
    // build must wait on nothing, and the reader on the build — never the other way round.
    let mut next = RenderGraph::new();
    let next_slot = next.alloc_external_buffer_state(carried);
    let next_buffer = next.import_buffer(vk::Buffer::from_raw(7), Some(next_slot));
    next.add_pass(async_compute("build").access_buffer(
        next_buffer,
        whole,
        RgUsage::StorageWriteCompute,
    ));
    next.add_pass(RgPass::compute("consume").access_buffer(
        next_buffer,
        whole,
        RgUsage::StorageReadCompute,
    ));
    let plan = next.submission_plan(families);
    assert_eq!(plan.compute_batch_count(), 1);
    for (index, batch) in plan.batches.iter().enumerate() {
        for &source in &batch.wait_for_batches {
            assert!(
                source < index,
                "batch {index} waits on batch {source}, which the submit has not reached"
            );
        }
    }
}

/// A batch retires as a unit, so a hang report can attribute the wedge no finer than the run of
/// passes it covers. That run is what names it.
#[test]
fn a_batch_is_named_by_the_run_of_passes_it_covers() {
    let passes = [
        Some(RgPass::compute("gbuffer")),
        Some(RgPass::compute("gtao")),
        Some(RgPass::compute("ssgi")),
    ];

    assert_eq!(super::graph::batch_label(&passes, 0..3), "gbuffer…ssgi");
    assert_eq!(super::graph::batch_label(&passes, 1..2), "gtao");
    assert_eq!(
        super::graph::batch_label(&passes, 1..1),
        "queue-ownership-release",
        "a synthetic prologue batch covers no passes at all"
    );
}
