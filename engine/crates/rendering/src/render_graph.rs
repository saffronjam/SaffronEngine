//! The render graph: `RgUsage`-declared resource usage, derived barriers, and
//! recorded pass order — the silent-failure heart of the renderer.
//!
//! A pass *declares* what it touches ([`RgUsage`] reads/writes plus color/depth
//! attachments) and the graph derives every `vkCmdPipelineBarrier2`, every layout
//! transition, and the cross-frame layout write-back. No pass ever writes a
//! barrier by hand.
//!
//! The barrier derivation is pure logic on plain data, split out from the GPU
//! recording so it is unit-testable with no device — a missing or wrong barrier
//! is a data race, not a compile error, so the derivation is the part that must be
//! exhaustively tested in isolation.

use ash::vk;

use crate::Device;
use crate::nested_scopes::NestedScopeRecorder;
use crate::profiler::{CpuMarkerRegistry, CpuSpanBuffer, RgTimestamps, cpu_now_ns};

/// The per-frame profiler recorders the graph drives while executing: the GPU
/// timestamp recorder and the CPU span recorder, both armed only when a profiler mode
/// is active. A `None` recorder makes every scope a cheap branch (the unarmed `Off`
/// case). The render graph opens a GPU + CPU scope around each pass and reserves a
/// pipeline-stats slot for top-level graphics passes.
#[derive(Default)]
pub struct ProfileRecorders<'a> {
    /// The GPU timestamp recorder, or `None` when unarmed.
    pub gpu: Option<&'a mut RgTimestamps>,
    /// The CPU span recorder's registry + this frame's buffer, or `None` when unarmed.
    pub cpu: Option<(&'a mut CpuMarkerRegistry, &'a mut CpuSpanBuffer)>,
}

/// What a pass does with a resource. The single source of truth for barrier and
/// layout-transition derivation — a pass declares usage, never writes a barrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RgUsage {
    /// Color attachment write.
    ColorWrite,
    /// Depth attachment write.
    DepthWrite,
    /// Sampled in a fragment shader.
    SampledRead,
    /// Storage buffer written by a compute shader.
    StorageWriteCompute,
    /// Storage buffer read by a compute shader.
    StorageReadCompute,
    /// Storage buffer read by a fragment shader.
    StorageReadFragment,
    /// Image read+written in place by a compute shader (GENERAL layout).
    StorageImageRwCompute,
    /// Image sampled in a compute shader (SHADER_READ_ONLY layout).
    SampledReadCompute,
    /// Buffer read as a vertex stream (the compute-skinned deformed buffer).
    VertexInputRead,
    /// Buffer read as acceleration-structure-build input (the deformed buffer, by a BLAS refit).
    AccelStructBuildRead,
    /// Buffer read as the index stream by an (indirect) indexed draw.
    IndexInputRead,
    /// Buffer read as indirect dispatch/draw arguments (or a draw-count).
    IndirectCommandRead,
}

/// Whether a pass records graphics commands (opens a rendering scope) or compute
/// commands (no rendering scope).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RgPassKind {
    /// A graphics pass: the graph opens `cmd_begin_rendering` around the body.
    Graphics,
    /// A compute pass: the body records directly, with no rendering scope.
    Compute,
}

/// A handle to a graph resource: an index into the graph's resource table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgResource {
    /// The resource's index in [`RenderGraph::resources`].
    pub index: u32,
}

/// A declared `(resource, usage)` pair — a pass's non-attachment reads/writes.
#[derive(Clone, Copy, Debug)]
pub struct RgAccess {
    /// The resource being read or written.
    pub resource: RgResource,
    /// How the pass touches it.
    pub usage: RgUsage,
}

/// A color or depth attachment binding for a graphics pass. The write usage and
/// the layout transition are derived; only the load/store/clear are declared here.
///
/// `resolve` is an MSAA resolve target: the multisampled attachment is resolved
/// into it at end-of-pass (color averaged, depth via sample 0); the graph treats
/// it as a second write of the matching kind.
#[derive(Clone, Copy)]
pub struct RgAttachment {
    /// The attachment image resource.
    pub resource: RgResource,
    /// How the attachment's prior contents are loaded.
    pub load_op: vk::AttachmentLoadOp,
    /// Whether the attachment's contents are stored after the pass.
    pub store_op: vk::AttachmentStoreOp,
    /// The clear value used when `load_op` is `CLEAR`.
    pub clear_value: vk::ClearValue,
    /// An optional MSAA resolve target written at end-of-pass.
    pub resolve: Option<RgResource>,
}

impl RgAttachment {
    /// A `CLEAR`-then-`STORE` attachment with a zero clear value and no resolve —
    /// the common case for a freshly written target.
    pub fn clear_store(resource: RgResource) -> Self {
        Self {
            resource,
            load_op: vk::AttachmentLoadOp::CLEAR,
            store_op: vk::AttachmentStoreOp::STORE,
            clear_value: vk::ClearValue::default(),
            resolve: None,
        }
    }
}

/// A pass's recording closure: receives the command buffer and a recorder for opening nested
/// profiler sub-scopes over its own phases.
type PassBody = Box<dyn FnOnce(vk::CommandBuffer, &mut NestedScopeRecorder<'_>)>;

/// A unit of GPU work: its declared resource usage plus the closure that records
/// it.
///
/// The graph derives the barriers/layout transitions the body needs, opens the
/// rendering scope (graphics passes), runs the body, then closes the scope. The
/// body runs exactly once on the render thread while the command buffer records,
/// so it is `FnOnce`; it captures already-resolved handles (not the renderer
/// aggregate), and recording is single-threaded, so it need not be `Send`.
pub struct RgPass {
    /// A human-readable name (used for capture-tool labels and profiler scopes).
    pub name: String,
    /// Whether this is a graphics or compute pass.
    pub kind: RgPassKind,
    /// Non-attachment declared reads/writes.
    pub accesses: Vec<RgAccess>,
    /// Color attachments — MRT: index 0 is location 0, etc.
    pub colors: Vec<RgAttachment>,
    /// An optional depth attachment.
    pub depth: Option<RgAttachment>,
    /// The render area for a graphics pass (viewport/scissor/clear extent).
    pub render_area: vk::Extent2D,
    /// The body that records the pass's commands. Consumed on execute. The recorder lets
    /// the body open nested profiler sub-scopes for its own phases.
    pub execute: Option<PassBody>,
}

impl RgPass {
    /// A graphics pass with the given name and render area, no accesses or
    /// attachments yet. Chain [`RgPass::access`] / [`RgPass::color`] /
    /// [`RgPass::depth_attachment`] / [`RgPass::body`] to fill it in.
    pub fn graphics(name: impl Into<String>, render_area: vk::Extent2D) -> Self {
        Self {
            name: name.into(),
            kind: RgPassKind::Graphics,
            accesses: Vec::new(),
            colors: Vec::new(),
            depth: None,
            render_area,
            execute: None,
        }
    }

    /// A compute pass with the given name, no render area (compute passes open no
    /// rendering scope).
    pub fn compute(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: RgPassKind::Compute,
            accesses: Vec::new(),
            colors: Vec::new(),
            depth: None,
            render_area: vk::Extent2D::default(),
            execute: None,
        }
    }

    /// Declares a non-attachment `(resource, usage)` access.
    #[must_use]
    pub fn access(mut self, resource: RgResource, usage: RgUsage) -> Self {
        self.accesses.push(RgAccess { resource, usage });
        self
    }

    /// Adds a color attachment.
    #[must_use]
    pub fn color(mut self, attachment: RgAttachment) -> Self {
        self.colors.push(attachment);
        self
    }

    /// Sets the depth attachment.
    #[must_use]
    pub fn depth_attachment(mut self, attachment: RgAttachment) -> Self {
        self.depth = Some(attachment);
        self
    }

    /// Sets the recording body. The body receives the command buffer and a recorder for
    /// opening nested profiler sub-scopes.
    #[must_use]
    pub fn body(
        mut self,
        body: impl FnOnce(vk::CommandBuffer, &mut NestedScopeRecorder<'_>) + 'static,
    ) -> Self {
        self.execute = Some(Box::new(body));
        self
    }
}

/// Per-resource tracked state, advanced as passes are recorded in order.
///
/// The cross-frame layout write-back is expressed safely: an imported image may
/// carry `external_layout`, an index into [`RenderGraph::external_layouts`]. The slot
/// seeds the entry layout on
/// import and receives the resolved exit layout after execute, so an image's
/// layout carries across frames without a raw pointer.
#[derive(Clone)]
struct RgResourceState {
    is_image: bool,
    image: vk::Image,
    view: vk::ImageView,
    buffer: vk::Buffer,
    aspect: vk::ImageAspectFlags,
    layout: vk::ImageLayout,
    last_stage: vk::PipelineStageFlags2,
    last_access: vk::AccessFlags2,
    last_was_write: bool,
    touched: bool,
    external_layout: Option<usize>,
}

impl Default for RgResourceState {
    fn default() -> Self {
        Self {
            is_image: false,
            image: vk::Image::null(),
            view: vk::ImageView::null(),
            buffer: vk::Buffer::null(),
            aspect: vk::ImageAspectFlags::COLOR,
            layout: vk::ImageLayout::UNDEFINED,
            last_stage: vk::PipelineStageFlags2::TOP_OF_PIPE,
            last_access: vk::AccessFlags2::empty(),
            last_was_write: false,
            touched: false,
            external_layout: None,
        }
    }
}

/// The stage/access/layout/is-write tuple a usage maps to — the golden table that
/// drives barrier derivation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RgUsageInfo {
    stage: vk::PipelineStageFlags2,
    access: vk::AccessFlags2,
    /// `UNDEFINED` for buffer usages (no layout).
    layout: vk::ImageLayout,
    is_write: bool,
}

/// The stage/access/layout/is-write contract for each usage — the load-bearing source
/// of truth.
fn usage_info(usage: RgUsage) -> RgUsageInfo {
    match usage {
        RgUsage::ColorWrite => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            access: vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            is_write: true,
        },
        RgUsage::DepthWrite => RgUsageInfo {
            stage: vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS,
            access: vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
            layout: vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
            is_write: true,
        },
        RgUsage::SampledRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::FRAGMENT_SHADER,
            access: vk::AccessFlags2::SHADER_SAMPLED_READ,
            layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            is_write: false,
        },
        RgUsage::StorageWriteCompute => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_WRITE,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: true,
        },
        RgUsage::StorageReadCompute => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::StorageReadFragment => RgUsageInfo {
            stage: vk::PipelineStageFlags2::FRAGMENT_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::StorageImageRwCompute => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            layout: vk::ImageLayout::GENERAL,
            is_write: true,
        },
        RgUsage::SampledReadCompute => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_SAMPLED_READ,
            layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            is_write: false,
        },
        RgUsage::VertexInputRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
            access: vk::AccessFlags2::VERTEX_ATTRIBUTE_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::AccelStructBuildRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR,
            access: vk::AccessFlags2::SHADER_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::IndexInputRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::INDEX_INPUT,
            access: vk::AccessFlags2::INDEX_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        // `DRAW_INDIRECT` is the stage where both indirect draw *and* indirect dispatch parameters
        // are consumed, per `VK_PIPELINE_STAGE_2_DRAW_INDIRECT_BIT`.
        RgUsage::IndirectCommandRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::DRAW_INDIRECT,
            access: vk::AccessFlags2::INDIRECT_COMMAND_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
    }
}

/// Seeds a freshly-imported image's source scope from its entry layout: a
/// `SHADER_READ_ONLY` image was last sampled by a fragment shader (the
/// write-after-read source), so the first write waits on that read. Any other
/// entry layout has no prior in-frame work to wait on.
fn seed_image_state(r: &mut RgResourceState) {
    if r.layout == vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL {
        r.last_stage = vk::PipelineStageFlags2::FRAGMENT_SHADER;
        r.last_access = vk::AccessFlags2::SHADER_SAMPLED_READ;
    } else {
        r.last_stage = vk::PipelineStageFlags2::TOP_OF_PIPE;
        r.last_access = vk::AccessFlags2::empty();
    }
}

/// The barriers a single pass needs, derived from its declared usage.
#[derive(Default)]
struct DerivedBarriers {
    image: Vec<vk::ImageMemoryBarrier2<'static>>,
    memory: Vec<vk::MemoryBarrier2<'static>>,
}

impl DerivedBarriers {
    fn is_empty(&self) -> bool {
        self.image.is_empty() && self.memory.is_empty()
    }
}

/// Derives a barrier for one `(resource, usage)`, appends it to `barriers`, and
/// advances the resource state.
///
/// The hazard rule: a hazard exists when a write touches
/// an already-touched resource (write-after-anything) or a read follows a write
/// (read-after-write). Images barrier on a layout change *or* a hazard; buffers on
/// a hazard only. A read after a read with no layout change emits nothing.
fn apply_access(r: &mut RgResourceState, target: RgUsageInfo, barriers: &mut DerivedBarriers) {
    let hazard = (target.is_write && r.touched) || (!target.is_write && r.last_was_write);
    if r.is_image {
        let layout_change =
            target.layout != vk::ImageLayout::UNDEFINED && r.layout != target.layout;
        if layout_change || hazard {
            let new_layout = if layout_change {
                target.layout
            } else {
                r.layout
            };
            let barrier = vk::ImageMemoryBarrier2::default()
                .src_stage_mask(r.last_stage)
                .src_access_mask(r.last_access)
                .dst_stage_mask(target.stage)
                .dst_access_mask(target.access)
                .old_layout(r.layout)
                .new_layout(new_layout)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(r.image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: r.aspect,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            barriers.image.push(barrier);
        }
        if layout_change {
            r.layout = target.layout;
        }
    } else if hazard {
        let barrier = vk::MemoryBarrier2::default()
            .src_stage_mask(r.last_stage)
            .src_access_mask(r.last_access)
            .dst_stage_mask(target.stage)
            .dst_access_mask(target.access);
        barriers.memory.push(barrier);
    }

    r.last_stage = target.stage;
    r.last_access = target.access;
    r.last_was_write = target.is_write;
    r.touched = true;
}

/// A frame's render graph: imported resources plus the passes over them. Rebuilt
/// every frame (cheap) and recorded by [`RenderGraph::execute`].
#[derive(Default)]
pub struct RenderGraph {
    resources: Vec<RgResourceState>,
    passes: Vec<RgPass>,
    external_layouts: Vec<vk::ImageLayout>,
}

impl RenderGraph {
    /// A fresh, empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a cross-frame layout slot seeded with `initial`, returning its key
    /// for [`RenderGraph::import_image`]. After [`RenderGraph::execute`] the slot
    /// holds the image's resolved exit layout — read it back to seed the next
    /// frame's import.
    pub fn alloc_external_layout(&mut self, initial: vk::ImageLayout) -> usize {
        self.external_layouts.push(initial);
        self.external_layouts.len() - 1
    }

    /// The resolved layout currently in a slot (the entry layout before execute,
    /// the exit layout after).
    pub fn external_layout(&self, slot: usize) -> vk::ImageLayout {
        self.external_layouts[slot]
    }

    /// Imports an external image (offscreen/swapchain target). When `external` is
    /// set, the slot's layout seeds the entry layout and receives the resolved
    /// layout after execute, so the image's layout carries across frames.
    pub fn import_image(
        &mut self,
        image: vk::Image,
        view: vk::ImageView,
        aspect: vk::ImageAspectFlags,
        initial_layout: vk::ImageLayout,
        external: Option<usize>,
    ) -> RgResource {
        let mut r = RgResourceState {
            is_image: true,
            image,
            view,
            aspect,
            layout: initial_layout,
            external_layout: external,
            ..RgResourceState::default()
        };
        if let Some(slot) = external {
            r.layout = self.external_layouts[slot];
        }
        seed_image_state(&mut r);
        self.resources.push(r);
        RgResource {
            index: (self.resources.len() - 1) as u32,
        }
    }

    /// Imports an external 3D image (e.g. a GDF cascade volume). Tracked identically
    /// to a 2D image for barrier purposes — the barrier transitions the whole image
    /// and dimensionality is irrelevant.
    pub fn import_image_3d(
        &mut self,
        image: vk::Image,
        view: vk::ImageView,
        initial_layout: vk::ImageLayout,
        external: Option<usize>,
    ) -> RgResource {
        self.import_image(
            image,
            view,
            vk::ImageAspectFlags::COLOR,
            initial_layout,
            external,
        )
    }

    /// Imports an external buffer produced and/or consumed within the frame.
    pub fn import_buffer(&mut self, buffer: vk::Buffer) -> RgResource {
        let r = RgResourceState {
            is_image: false,
            buffer,
            ..RgResourceState::default()
        };
        self.resources.push(r);
        RgResource {
            index: (self.resources.len() - 1) as u32,
        }
    }

    /// Appends a pass to the graph.
    pub fn add_pass(&mut self, pass: RgPass) {
        self.passes.push(pass);
    }

    /// The underlying image handle of an imaged resource (null for a buffer
    /// resource). Pass bodies resolve handles through the graph rather than
    /// recapturing the renderer aggregate.
    pub fn image(&self, resource: RgResource) -> vk::Image {
        self.resources[resource.index as usize].image
    }

    /// The underlying image-view handle of an imaged resource (null for a buffer).
    pub fn view(&self, resource: RgResource) -> vk::ImageView {
        self.resources[resource.index as usize].view
    }

    /// The underlying buffer handle of a buffer resource (null for an image).
    pub fn buffer(&self, resource: RgResource) -> vk::Buffer {
        self.resources[resource.index as usize].buffer
    }

    /// Derives the barriers a pass needs from its declared accesses and attachments,
    /// advancing the resource table. Color/depth attachments are treated as the
    /// matching write usage; an MSAA resolve target is a second write of that kind.
    /// Pure logic — no GPU — so it is the unit-tested core.
    fn derive_pass_barriers(&mut self, pass: &RgPass) -> DerivedBarriers {
        let mut barriers = DerivedBarriers::default();
        for access in &pass.accesses {
            apply_access(
                &mut self.resources[access.resource.index as usize],
                usage_info(access.usage),
                &mut barriers,
            );
        }
        for att in &pass.colors {
            apply_access(
                &mut self.resources[att.resource.index as usize],
                usage_info(RgUsage::ColorWrite),
                &mut barriers,
            );
            if let Some(resolve) = att.resolve {
                apply_access(
                    &mut self.resources[resolve.index as usize],
                    usage_info(RgUsage::ColorWrite),
                    &mut barriers,
                );
            }
        }
        if let Some(depth) = &pass.depth {
            apply_access(
                &mut self.resources[depth.resource.index as usize],
                usage_info(RgUsage::DepthWrite),
                &mut barriers,
            );
            if let Some(resolve) = depth.resolve {
                apply_access(
                    &mut self.resources[resolve.index as usize],
                    usage_info(RgUsage::DepthWrite),
                    &mut barriers,
                );
            }
        }
        barriers
    }

    /// Derives and emits each pass's barriers from its declared usage, then records
    /// the pass body inside its rendering scope (graphics) or directly (compute).
    /// After every pass, resolves cross-frame layouts into their external slots.
    ///
    /// Recording is single-threaded: the body closures run here on the render
    /// thread, exactly once each, while `cmd` records.
    pub fn execute(&mut self, device: &Device, cmd: vk::CommandBuffer) {
        self.execute_profiled(device, cmd, &mut ProfileRecorders::default());
    }

    /// [`RenderGraph::execute`] with the profiler recorders armed: each pass body is
    /// bracketed by a GPU timestamp scope (when `recorders.gpu` is armed) and a CPU
    /// span (when `recorders.cpu` is armed), and a top-level graphics pass reserves a
    /// pipeline-statistics slot. Unarmed recorders make every scope a cheap branch.
    pub fn execute_profiled(
        &mut self,
        device: &Device,
        cmd: vk::CommandBuffer,
        recorders: &mut ProfileRecorders<'_>,
    ) {
        let raw = device.raw();
        let passes = std::mem::take(&mut self.passes);
        for pass in passes {
            // CPU span over the cost of recording this pass on the render thread.
            let cpu_index = recorders
                .cpu
                .as_mut()
                .map(|(registry, buffer)| buffer.begin_span(registry, &pass.name, cpu_now_ns()));

            // Top-level GPU scope for the pass (its barriers + body). The body may open
            // child scopes on the same recorder, which nest under this one.
            let gpu_index = recorders
                .gpu
                .as_mut()
                .and_then(|ts| ts.begin_scope(raw, cmd, &pass.name));

            let barriers = self.derive_pass_barriers(&pass);
            if !barriers.is_empty() {
                let dependency = vk::DependencyInfo::default()
                    .image_memory_barriers(&barriers.image)
                    .memory_barriers(&barriers.memory);
                // SAFETY: the ash seam. The barriers were derived from the passes'
                // declared usage against the imported handles; `cmd` is recording.
                unsafe { raw.cmd_pipeline_barrier2(cmd, &dependency) };
            }

            // One pipeline-statistics query per top-level graphics pass (the slot is
            // reserved here; the body issues begin/end inside the rendering scope).
            if let (Some(index), Some(ts)) = (gpu_index, recorders.gpu.as_mut()) {
                let pixels = u64::from(pass.render_area.width) * u64::from(pass.render_area.height);
                let _ = ts.reserve_stats_slot(index, pixels);
            }

            {
                // The pass body may open child scopes; reborrow the recorders into a
                // handle scoped to the body call so the top-level end_scope below still
                // sees the recorders free.
                let mut nested = NestedScopeRecorder::new(
                    raw,
                    cmd,
                    recorders.gpu.as_deref_mut(),
                    recorders.cpu.as_mut().map(|(r, b)| (&mut **r, &mut **b)),
                );
                match pass.kind {
                    RgPassKind::Graphics => self.record_graphics(device, cmd, pass, &mut nested),
                    RgPassKind::Compute => {
                        if let Some(body) = pass.execute {
                            body(cmd, &mut nested);
                        }
                    }
                }
            }

            if let Some(ts) = recorders.gpu.as_mut() {
                ts.end_scope(raw, cmd, gpu_index);
            }
            if let (Some(index), Some((_, buffer))) = (cpu_index, recorders.cpu.as_mut()) {
                buffer.end_span(index, cpu_now_ns());
            }
        }

        for r in &self.resources {
            if let Some(slot) = r.external_layout {
                self.external_layouts[slot] = r.layout;
            }
        }
    }

    /// Opens a `cmd_begin_rendering` scope for a graphics pass — color/depth
    /// attachment infos (incl. MSAA color `AVERAGE` / depth `SAMPLE_ZERO` resolve),
    /// the full-area viewport/scissor — runs the body, then closes the scope.
    fn record_graphics(
        &self,
        device: &Device,
        cmd: vk::CommandBuffer,
        pass: RgPass,
        scopes: &mut NestedScopeRecorder<'_>,
    ) {
        let raw = device.raw();
        let mut color_infos = Vec::with_capacity(pass.colors.len());
        for att in &pass.colors {
            let r = &self.resources[att.resource.index as usize];
            let mut info = vk::RenderingAttachmentInfo::default()
                .image_view(r.view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(att.load_op)
                .store_op(att.store_op)
                .clear_value(att.clear_value);
            if let Some(resolve) = att.resolve {
                info = info
                    .resolve_mode(vk::ResolveModeFlags::AVERAGE)
                    .resolve_image_view(self.resources[resolve.index as usize].view)
                    .resolve_image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            }
            color_infos.push(info);
        }

        let depth_info = pass.depth.as_ref().map(|depth| {
            let r = &self.resources[depth.resource.index as usize];
            let mut info = vk::RenderingAttachmentInfo::default()
                .image_view(r.view)
                .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                .load_op(depth.load_op)
                .store_op(depth.store_op)
                .clear_value(depth.clear_value);
            if let Some(resolve) = depth.resolve {
                info = info
                    .resolve_mode(vk::ResolveModeFlags::SAMPLE_ZERO)
                    .resolve_image_view(self.resources[resolve.index as usize].view)
                    .resolve_image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL);
            }
            info
        });

        let mut rendering = vk::RenderingInfo::default()
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: pass.render_area,
            })
            .layer_count(1)
            .color_attachments(&color_infos);
        if let Some(ref depth) = depth_info {
            rendering = rendering.depth_attachment(depth);
        }

        let viewport = vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: pass.render_area.width as f32,
            height: pass.render_area.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let scissor = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: pass.render_area,
        };

        // SAFETY: the ash seam. The attachment infos reference imported views; the
        // rendering scope is opened and closed in this method and the body records
        // between them.
        unsafe {
            raw.cmd_begin_rendering(cmd, &rendering);
            raw.cmd_set_viewport(cmd, 0, &[viewport]);
            raw.cmd_set_scissor(cmd, 0, &[scissor]);
        }
        if let Some(body) = pass.execute {
            body(cmd, scopes);
        }
        // SAFETY: the ash seam. Closes the rendering scope opened above.
        unsafe { raw.cmd_end_rendering(cmd) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                RgUsage::IndirectCommandRead,
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
        assert!(barriers.memory.is_empty());
        let b = barriers.image[0];
        assert_eq!(b.old_layout, vk::ImageLayout::UNDEFINED);
        assert_eq!(b.new_layout, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        assert_eq!(b.src_stage_mask, vk::PipelineStageFlags2::TOP_OF_PIPE);
        assert_eq!(b.dst_stage_mask, vk::PipelineStageFlags2::FRAGMENT_SHADER);
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
            barriers.memory.len(),
            1,
            "read after write is a buffer hazard"
        );
        assert!(
            barriers.image.is_empty(),
            "buffers never emit image barriers"
        );
        let b = barriers.memory[0];
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
            barriers.memory.len(),
            1,
            "index read after write is a hazard"
        );
        assert!(
            barriers.image.is_empty(),
            "buffers never emit image barriers"
        );
        let b = barriers.memory[0];
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
            barriers.memory.len(),
            1,
            "indirect-args read after write is a hazard"
        );
        assert!(
            barriers.image.is_empty(),
            "buffers never emit image barriers"
        );
        let b = barriers.memory[0];
        assert_eq!(b.src_stage_mask, vk::PipelineStageFlags2::COMPUTE_SHADER);
        assert_eq!(b.src_access_mask, vk::AccessFlags2::SHADER_STORAGE_WRITE);
        assert_eq!(b.dst_stage_mask, vk::PipelineStageFlags2::DRAW_INDIRECT);
        assert_eq!(b.dst_access_mask, vk::AccessFlags2::INDIRECT_COMMAND_READ);
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
        assert_eq!(barriers.memory.len(), 1, "write after read is a hazard");
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
        assert_eq!(p1.memory.len(), 1);
        assert!(p1.image.is_empty());
        assert_eq!(
            p1.memory[0].src_stage_mask,
            vk::PipelineStageFlags2::COMPUTE_SHADER
        );
        assert_eq!(
            p1.memory[0].dst_stage_mask,
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
    fn cross_frame_layout_write_back() {
        // An imported image's exit layout becomes its next-frame entry layout. The graph
        // owns the external slot; after deriving the frame's passes, the slot holds the
        // resolved layout, which seeds the next frame's import.
        let mut graph = RenderGraph::new();
        let slot = graph.alloc_external_layout(vk::ImageLayout::UNDEFINED);
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
        let _ = graph.derive_pass_barriers(&graph.passes[0].clone_for_test());
        // Mirror execute's write-back step.
        for r in &graph.resources {
            if let Some(s) = r.external_layout {
                graph.external_layouts[s] = r.layout;
            }
        }
        assert_eq!(
            graph.external_layout(slot),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            "the exit layout is written back into the external slot"
        );

        // Next frame: a fresh import from the same slot seeds the entry layout.
        let mut next = RenderGraph::new();
        next.external_layouts.push(graph.external_layout(slot));
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
    }

    impl RgPass {
        /// A shallow clone of a pass for tests (the body is not cloneable, so it is
        /// dropped). Lets a test re-derive barriers without consuming the graph's pass.
        fn clone_for_test(&self) -> RgPass {
            RgPass {
                name: self.name.clone(),
                kind: self.kind,
                accesses: self.accesses.clone(),
                colors: self.colors.clone(),
                depth: self.depth,
                render_area: self.render_area,
                execute: None,
            }
        }
    }
}
