//! The render graph: `RgUsage`-declared resource usage, derived barriers, and recorded pass order.
//!
//! A pass declares what it touches ([`RgUsage`] reads/writes plus color/depth attachments) and the
//! graph derives every `vkCmdPipelineBarrier2`, every layout transition, and the cross-frame layout
//! write-back. No pass writes a barrier by hand. The derivation is pure logic on plain data, kept
//! separate from the GPU recording so it is testable with no device.

mod barriers;
mod graph;

use ash::vk;

use crate::Device;
use crate::nested_scopes::NestedScopeRecorder;
use crate::profiler::{CpuMarkerRegistry, CpuSpanBuffer, RgTimestamps, cpu_now_ns};

use barriers::*;
pub use graph::*;

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

/// What a pass does with a resource, from which the graph derives every barrier and layout
/// transition.
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
    /// Storage buffer read and written by a compute shader.
    StorageReadWriteCompute,
    /// Image read+written in place by a compute shader (GENERAL layout).
    StorageImageRwCompute,
    /// Image sampled in a compute shader (SHADER_READ_ONLY layout).
    SampledReadCompute,
    /// Buffer read by a transfer operation.
    TransferRead,
    /// Buffer written by a transfer operation.
    TransferWrite,
    /// Buffer read as a vertex stream (the compute-skinned deformed buffer).
    VertexInputRead,
    /// Buffer read as acceleration-structure-build input (the deformed buffer, by a BLAS refit).
    AccelStructBuildRead,
    /// Buffer read as the index stream by an (indirect) indexed draw.
    IndexInputRead,
    /// Buffer read through a shader device address.
    ShaderDeviceAddressRead,
    /// Buffer read as indirect dispatch/draw arguments (or a draw-count).
    IndirectCommandRead,
    /// Buffer read both as indirect draw arguments and as mesh-shader storage data.
    MeshExecutorCommandRead,
    /// Buffer read as the count consumed by an indirect-count draw.
    IndirectCountRead,
}

/// A half-open byte range within a graph buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgBufferRange {
    /// First byte in the range.
    pub offset: vk::DeviceSize,
    /// Number of bytes in the range.
    pub size: vk::DeviceSize,
}

impl RgBufferRange {
    /// Creates a non-empty byte range whose end is representable.
    pub fn new(offset: vk::DeviceSize, size: vk::DeviceSize) -> Result<Self, RgBufferRangeError> {
        if size == 0 {
            return Err(RgBufferRangeError::Empty);
        }
        offset
            .checked_add(size)
            .ok_or(RgBufferRangeError::EndOverflow)?;
        Ok(Self { offset, size })
    }
}

/// Invalid graph-buffer range construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RgBufferRangeError {
    /// A zero-byte range cannot describe an access.
    #[error("render-graph buffer range is empty")]
    Empty,
    /// `offset + size` is not representable.
    #[error("render-graph buffer range end overflows")]
    EndOverflow,
}

/// Whether a graph buffer is external or allocated for a graph-defined lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RgBufferLifetime {
    /// The allocation is owned outside the graph.
    Imported,
    /// The allocation is recyclable after the owning frame fence signals.
    Transient,
    /// The allocation persists between graph instances under a stable key.
    Persistent,
}

/// Size, Vulkan usages, and allocation lifetime for a graph-owned buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgBufferDesc {
    /// Requested size in bytes.
    pub size: vk::DeviceSize,
    /// Vulkan buffer usage flags.
    pub usage: vk::BufferUsageFlags,
    /// Transient or persistent allocation lifetime.
    pub lifetime: RgBufferLifetime,
}

/// Complete declaration of a buffer registered in the graph resource table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgBufferResource {
    /// Vulkan buffer handle supplied by the graph resource allocator or importer.
    pub buffer: vk::Buffer,
    /// Addressable size in bytes.
    pub size: vk::DeviceSize,
    /// Vulkan usages the allocation was created with.
    pub usage: vk::BufferUsageFlags,
    /// Allocation lifetime represented by this registration.
    pub lifetime: RgBufferLifetime,
}

/// A pass's preferred execution queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RgQueuePreference {
    /// Execute on the graphics queue.
    #[default]
    Graphics,
    /// Execute on an independent compute queue when useful, otherwise graphics.
    AsyncCompute,
}

/// Queue selected for a declared pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RgQueueAssignment {
    /// Graphics queue and family.
    Graphics,
    /// Independent compute queue and family.
    AsyncCompute,
}

/// Queue-family topology used to resolve pass assignments and ownership transfers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgQueueFamilies {
    /// Graphics queue-family index.
    pub graphics: u32,
    /// Independent compute family, when the device and submission path support overlap.
    pub async_compute: Option<u32>,
}

impl RgQueueFamilies {
    /// A graphics-only topology.
    pub const fn graphics_only(graphics: u32) -> Self {
        Self {
            graphics,
            async_compute: None,
        }
    }

    fn resolve(self, pass: &RgPass) -> (RgQueueAssignment, u32) {
        if pass.kind == RgPassKind::Compute
            && pass.queue == RgQueuePreference::AsyncCompute
            && let Some(family) = self.async_compute
        {
            return (RgQueueAssignment::AsyncCompute, family);
        }
        (RgQueueAssignment::Graphics, self.graphics)
    }

    fn family(self, queue: RgQueueAssignment) -> Option<u32> {
        match queue {
            RgQueueAssignment::Graphics => Some(self.graphics),
            RgQueueAssignment::AsyncCompute => self.async_compute,
        }
    }
}

/// Barriers and queue assignment compiled for one pass.
///
/// Queue-family release barriers execute after the pass body. Matching acquire barriers are in the
/// destination pass's `before_*` lists; a timeline-semaphore dependency connects the two submits.
#[derive(Clone, Debug)]
pub struct RgPassBarriers {
    /// Resolved execution queue.
    pub queue: RgQueueAssignment,
    /// Queue-family index for the selected queue.
    pub queue_family: u32,
    /// Source passes whose queue submissions must signal before this pass may acquire ownership.
    pub wait_for_passes: Vec<usize>,
    /// Image barriers recorded before the pass.
    pub before_images: Vec<vk::ImageMemoryBarrier2<'static>>,
    /// Buffer barriers recorded before the pass.
    pub before_buffers: Vec<vk::BufferMemoryBarrier2<'static>>,
    /// Queue-family release image barriers recorded after the pass.
    pub after_images: Vec<vk::ImageMemoryBarrier2<'static>>,
    /// Queue-family release buffer barriers recorded after the pass.
    pub after_buffers: Vec<vk::BufferMemoryBarrier2<'static>>,
}

/// Cross-frame state for one exclusive imported image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgExternalState {
    /// Current image layout.
    pub layout: vk::ImageLayout,
    /// Queue family that owns the image, or `None` before its first graph use.
    pub queue_family: Option<u32>,
    /// Queue identity that most recently accessed the image.
    pub queue: Option<RgQueueAssignment>,
    stage: vk::PipelineStageFlags2,
    access: vk::AccessFlags2,
    was_write: bool,
    touched: bool,
}

/// Cross-frame byte-range ownership and access state for one exclusive buffer.
#[derive(Clone, Debug, Default)]
pub struct RgExternalBufferState {
    accesses: Vec<RgBufferAccessState>,
}

impl RgExternalState {
    /// Creates first-use state for an image in `layout` with no established queue owner.
    pub fn new(layout: vk::ImageLayout) -> Self {
        let (stage, access) = if layout == vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL {
            (
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            )
        } else {
            (
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::empty(),
            )
        };
        Self {
            layout,
            queue_family: None,
            queue: None,
            stage,
            access,
            was_write: false,
            touched: layout != vk::ImageLayout::UNDEFINED,
        }
    }

    pub(crate) fn with_layout(mut self, layout: vk::ImageLayout) -> Self {
        self.layout = layout;
        self
    }
}

/// One contiguous run of passes recorded into a single queue command buffer.
#[derive(Clone, Debug)]
pub struct RgPassBatch {
    /// Selected queue.
    pub queue: RgQueueAssignment,
    /// Half-open pass-index range in declaration order.
    pub passes: std::ops::Range<usize>,
    /// Earlier batches on the other queue that must signal before this batch executes.
    pub wait_for_batches: Vec<usize>,
    /// Entry ownership-release image barriers recorded by this synthetic prologue batch.
    pub entry_release_images: Vec<vk::ImageMemoryBarrier2<'static>>,
    /// Entry ownership-release buffer barriers recorded by this synthetic prologue batch.
    pub entry_release_buffers: Vec<vk::BufferMemoryBarrier2<'static>>,
}

/// Immutable queue/batch/barrier compilation for one graph instance.
#[derive(Clone, Debug)]
pub struct RgSubmissionPlan {
    /// Pass-local barriers and assignments.
    pub passes: Vec<RgPassBarriers>,
    /// Contiguous command-buffer batches.
    pub batches: Vec<RgPassBatch>,
    exit_resources: Vec<RgResourceState>,
}

#[derive(Clone, Copy)]
struct RgEntryRelease {
    destination_pass: usize,
    source_family: u32,
    source_queue: RgQueueAssignment,
    barrier: RgQueueReleaseBarrier,
}

impl RgSubmissionPlan {
    /// Number of graphics command buffers required to record this plan.
    pub fn graphics_batch_count(&self) -> usize {
        self.batches
            .iter()
            .filter(|batch| batch.queue == RgQueueAssignment::Graphics)
            .count()
    }

    /// Number of compute command buffers required to record this plan.
    pub fn compute_batch_count(&self) -> usize {
        self.batches
            .iter()
            .filter(|batch| batch.queue == RgQueueAssignment::AsyncCompute)
            .count()
    }
}

/// Queue command buffers supplied for a compiled graph plan.
pub struct RgBatchCommandBuffers<'a> {
    /// One primary command buffer per graphics batch.
    pub graphics: &'a [vk::CommandBuffer],
    /// One primary command buffer per async-compute batch.
    pub compute: &'a [vk::CommandBuffer],
}

/// A recorded batch ready for timeline-linked queue submission.
#[derive(Clone, Debug)]
pub struct RgRecordedBatch {
    /// Selected queue.
    pub queue: RgQueueAssignment,
    /// The passes the batch covers, as `first…last` — what a hang report names it by.
    pub label: String,
    /// Recorded primary command buffer.
    pub command_buffer: vk::CommandBuffer,
    /// Earlier batch indices whose timeline points this batch waits for.
    pub wait_for_batches: Vec<usize>,
    /// Half-open pass range recorded in the command buffer.
    pub passes: std::ops::Range<usize>,
}

/// How a pass body is recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RgPassKind {
    /// A graphics pass: the graph opens `cmd_begin_rendering` around the body.
    Graphics,
    /// Graphics commands whose body opens and closes multiple rendering scopes.
    GraphicsCommands,
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
    /// Byte range for a buffer access; `None` means the complete resource.
    pub buffer_range: Option<RgBufferRange>,
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
    ///
    /// `&'static str`, not `String`: the graph is rebuilt every frame, and every pass in the tree
    /// names itself with a literal, so owning the name would allocate once per pass per frame.
    pub name: &'static str,
    /// Whether this is a graphics or compute pass.
    pub kind: RgPassKind,
    /// Preferred queue; resolved against the device topology without changing the pass.
    pub queue: RgQueuePreference,
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
    pub fn graphics(name: &'static str, render_area: vk::Extent2D) -> Self {
        Self {
            name,
            kind: RgPassKind::Graphics,
            queue: RgQueuePreference::Graphics,
            accesses: Vec::new(),
            colors: Vec::new(),
            depth: None,
            render_area,
            execute: None,
        }
    }

    /// A compute pass with the given name, no render area (compute passes open no
    /// rendering scope).
    ///
    /// It runs on the graphics queue unless it opts into [`RgQueuePreference::AsyncCompute`]
    /// through [`RgPass::queue`]. Independent execution is a per-pass claim that every
    /// dependency the pass produces is *declared*, because that is what the graph turns into
    /// the cross-queue release/acquire pair — a pass whose consumers reach its output some
    /// other way has no such handoff, and naming a graphics stage from a compute-only queue
    /// is invalid besides.
    pub fn compute(name: &'static str) -> Self {
        Self {
            name,
            kind: RgPassKind::Compute,
            queue: RgQueuePreference::Graphics,
            accesses: Vec::new(),
            colors: Vec::new(),
            depth: None,
            render_area: vk::Extent2D::default(),
            execute: None,
        }
    }

    /// Selects the queue this pass prefers. Only a compute pass can leave the graphics
    /// queue, and only where the device exposes an independent compute family.
    #[must_use]
    pub fn queue(mut self, queue: RgQueuePreference) -> Self {
        self.queue = queue;
        self
    }

    /// Declares a non-attachment `(resource, usage)` access.
    #[must_use]
    pub fn access(mut self, resource: RgResource, usage: RgUsage) -> Self {
        self.accesses.push(RgAccess {
            resource,
            usage,
            buffer_range: None,
        });
        self
    }

    /// Declares a byte-ranged buffer access.
    #[must_use]
    pub fn access_buffer(
        mut self,
        resource: RgResource,
        range: RgBufferRange,
        usage: RgUsage,
    ) -> Self {
        self.accesses.push(RgAccess {
            resource,
            usage,
            buffer_range: Some(range),
        });
        self
    }

    /// Graphics commands that manage multiple dynamic-rendering scopes in one body.
    pub fn graphics_commands(name: &'static str) -> Self {
        Self {
            name,
            kind: RgPassKind::GraphicsCommands,
            queue: RgQueuePreference::Graphics,
            accesses: Vec::new(),
            colors: Vec::new(),
            depth: None,
            render_area: vk::Extent2D::default(),
            execute: None,
        }
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
/// carry `external_state`, an index into [`RenderGraph::external_states`]. The slot
/// seeds layout, access scope, and queue ownership on import and receives the resolved exit state.
#[derive(Clone, Debug)]
struct RgResourceState {
    is_image: bool,
    image: vk::Image,
    view: vk::ImageView,
    buffer: vk::Buffer,
    buffer_size: vk::DeviceSize,
    buffer_usage: vk::BufferUsageFlags,
    buffer_lifetime: RgBufferLifetime,
    buffer_accesses: Vec<RgBufferAccessState>,
    aspect: vk::ImageAspectFlags,
    layout: vk::ImageLayout,
    last_stage: vk::PipelineStageFlags2,
    last_access: vk::AccessFlags2,
    last_was_write: bool,
    touched: bool,
    external_state: Option<usize>,
    external_buffer_state: Option<usize>,
    queue_family: Option<u32>,
    queue: Option<RgQueueAssignment>,
    last_pass: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct RgBufferAccessState {
    start: vk::DeviceSize,
    end: vk::DeviceSize,
    stage: vk::PipelineStageFlags2,
    access: vk::AccessFlags2,
    is_write: bool,
    queue_family: u32,
    queue: RgQueueAssignment,
    last_pass: Option<usize>,
}

impl Default for RgResourceState {
    fn default() -> Self {
        Self {
            is_image: false,
            image: vk::Image::null(),
            view: vk::ImageView::null(),
            buffer: vk::Buffer::null(),
            buffer_size: vk::WHOLE_SIZE,
            buffer_usage: vk::BufferUsageFlags::empty(),
            buffer_lifetime: RgBufferLifetime::Imported,
            buffer_accesses: Vec::new(),
            aspect: vk::ImageAspectFlags::COLOR,
            layout: vk::ImageLayout::UNDEFINED,
            last_stage: vk::PipelineStageFlags2::TOP_OF_PIPE,
            last_access: vk::AccessFlags2::empty(),
            last_was_write: false,
            touched: false,
            external_state: None,
            external_buffer_state: None,
            queue_family: None,
            queue: None,
            last_pass: None,
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

#[cfg(test)]
mod tests;
