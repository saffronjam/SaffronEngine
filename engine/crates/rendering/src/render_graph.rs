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
    pub name: String,
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
    pub fn graphics(name: impl Into<String>, render_area: vk::Extent2D) -> Self {
        Self {
            name: name.into(),
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
    pub fn compute(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: RgPassKind::Compute,
            queue: RgQueuePreference::AsyncCompute,
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
    pub fn graphics_commands(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
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
        RgUsage::StorageReadWriteCompute => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: true,
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
        RgUsage::TransferRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COPY,
            access: vk::AccessFlags2::TRANSFER_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::TransferWrite => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COPY,
            access: vk::AccessFlags2::TRANSFER_WRITE,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: true,
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
        RgUsage::ShaderDeviceAddressRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::ALL_COMMANDS,
            access: vk::AccessFlags2::SHADER_READ,
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
        RgUsage::IndirectCountRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::DRAW_INDIRECT,
            access: vk::AccessFlags2::INDIRECT_COMMAND_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
    }
}

fn required_buffer_usage(usage: RgUsage) -> Option<vk::BufferUsageFlags> {
    match usage {
        RgUsage::StorageWriteCompute
        | RgUsage::StorageReadCompute
        | RgUsage::StorageReadFragment
        | RgUsage::StorageReadWriteCompute => Some(vk::BufferUsageFlags::STORAGE_BUFFER),
        RgUsage::TransferRead => Some(vk::BufferUsageFlags::TRANSFER_SRC),
        RgUsage::TransferWrite => Some(vk::BufferUsageFlags::TRANSFER_DST),
        RgUsage::VertexInputRead => Some(vk::BufferUsageFlags::VERTEX_BUFFER),
        RgUsage::IndexInputRead => Some(vk::BufferUsageFlags::INDEX_BUFFER),
        RgUsage::ShaderDeviceAddressRead => Some(vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS),
        RgUsage::IndirectCommandRead | RgUsage::IndirectCountRead => {
            Some(vk::BufferUsageFlags::INDIRECT_BUFFER)
        }
        RgUsage::AccelStructBuildRead => {
            Some(vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR)
        }
        RgUsage::ColorWrite
        | RgUsage::DepthWrite
        | RgUsage::SampledRead
        | RgUsage::StorageImageRwCompute
        | RgUsage::SampledReadCompute => None,
    }
}

fn validate_declared_access(resource: &RgResourceState, usage: RgUsage) {
    match required_buffer_usage(usage) {
        Some(required) => {
            assert!(
                !resource.is_image,
                "buffer usage declared for a graph image"
            );
            assert!(
                resource.buffer_usage.is_empty() || resource.buffer_usage.contains(required),
                "graph buffer was not allocated with the Vulkan usage required by {usage:?}"
            );
        }
        None => assert!(resource.is_image, "image usage declared for a graph buffer"),
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
    buffer: Vec<vk::BufferMemoryBarrier2<'static>>,
    releases: Vec<RgQueueRelease>,
}

#[derive(Clone, Copy)]
enum RgQueueReleaseBarrier {
    Image(vk::ImageMemoryBarrier2<'static>),
    Buffer(vk::BufferMemoryBarrier2<'static>),
    Dependency,
}

#[derive(Clone, Copy)]
struct RgQueueRelease {
    pass: Option<usize>,
    source_family: u32,
    source_queue: RgQueueAssignment,
    barrier: RgQueueReleaseBarrier,
}

impl DerivedBarriers {
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.image.is_empty() && self.buffer.is_empty()
    }
}

/// Derives a barrier for one `(resource, usage)`, appends it to `barriers`, and
/// advances the resource state.
///
/// The hazard rule: a hazard exists when a write touches
/// an already-touched resource (write-after-anything) or a read follows a write
/// (read-after-write). Images barrier on a layout change *or* a hazard; buffers on
/// a hazard only. A read after a read with no layout change emits nothing.
fn apply_access_queued(
    r: &mut RgResourceState,
    target: RgUsageInfo,
    range: Option<RgBufferRange>,
    pass: usize,
    queue: RgQueueAssignment,
    queue_family: u32,
    barriers: &mut DerivedBarriers,
) {
    if !r.is_image {
        apply_buffer_access(r, target, range, pass, queue, queue_family, barriers);
        return;
    }
    let hazard = (target.is_write && r.touched) || (!target.is_write && r.last_was_write);
    let layout_change = target.layout != vk::ImageLayout::UNDEFINED && r.layout != target.layout;
    let queue_change = r.queue.is_some_and(|previous| previous != queue);
    let ownership_transfer =
        queue_change && r.queue_family.is_some_and(|family| family != queue_family);
    let new_layout = if layout_change {
        target.layout
    } else {
        r.layout
    };
    let subresource_range = vk::ImageSubresourceRange {
        aspect_mask: r.aspect,
        base_mip_level: 0,
        level_count: vk::REMAINING_MIP_LEVELS,
        base_array_layer: 0,
        layer_count: vk::REMAINING_ARRAY_LAYERS,
    };
    if queue_change {
        let source_family = r
            .queue_family
            .expect("a queue-owned resource has a queue family");
        let release_barrier = if ownership_transfer {
            RgQueueReleaseBarrier::Image(
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(r.last_stage)
                    .src_access_mask(r.last_access)
                    .dst_stage_mask(vk::PipelineStageFlags2::NONE)
                    .dst_access_mask(vk::AccessFlags2::empty())
                    .old_layout(r.layout)
                    .new_layout(new_layout)
                    .src_queue_family_index(source_family)
                    .dst_queue_family_index(queue_family)
                    .image(r.image)
                    .subresource_range(subresource_range),
            )
        } else {
            RgQueueReleaseBarrier::Dependency
        };
        if let Some(source_pass) = r.last_pass {
            barriers.releases.push(RgQueueRelease {
                pass: Some(source_pass),
                source_family,
                source_queue: r.queue.expect("queue change has a source queue"),
                barrier: release_barrier,
            });
        } else {
            barriers.releases.push(RgQueueRelease {
                pass: None,
                source_family,
                source_queue: r.queue.expect("queue change has a source queue"),
                barrier: release_barrier,
            });
        }
        if ownership_transfer || layout_change || hazard {
            barriers.image.push(
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::NONE)
                    .src_access_mask(vk::AccessFlags2::empty())
                    .dst_stage_mask(target.stage)
                    .dst_access_mask(target.access)
                    .old_layout(r.layout)
                    .new_layout(new_layout)
                    .src_queue_family_index(if ownership_transfer {
                        source_family
                    } else {
                        vk::QUEUE_FAMILY_IGNORED
                    })
                    .dst_queue_family_index(if ownership_transfer {
                        queue_family
                    } else {
                        vk::QUEUE_FAMILY_IGNORED
                    })
                    .image(r.image)
                    .subresource_range(subresource_range),
            );
        }
    } else if layout_change || hazard {
        barriers.image.push(
            vk::ImageMemoryBarrier2::default()
                .src_stage_mask(r.last_stage)
                .src_access_mask(r.last_access)
                .dst_stage_mask(target.stage)
                .dst_access_mask(target.access)
                .old_layout(r.layout)
                .new_layout(new_layout)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(r.image)
                .subresource_range(subresource_range),
        );
    }
    if layout_change {
        r.layout = target.layout;
    }

    r.last_stage = target.stage;
    r.last_access = target.access;
    r.last_was_write = target.is_write;
    r.touched = true;
    r.queue_family = Some(queue_family);
    r.queue = Some(queue);
    r.last_pass = Some(pass);
}

fn apply_buffer_access(
    r: &mut RgResourceState,
    target: RgUsageInfo,
    range: Option<RgBufferRange>,
    pass: usize,
    queue: RgQueueAssignment,
    queue_family: u32,
    barriers: &mut DerivedBarriers,
) {
    let (start, end) = buffer_range_bounds(r.buffer_size, range);
    let mut retained = Vec::with_capacity(r.buffer_accesses.len() + 1);
    let mut inherited_read_stage = target.stage;
    let mut inherited_read_access = target.access;

    for previous in r.buffer_accesses.drain(..) {
        let overlap_start = previous.start.max(start);
        let overlap_end = previous.end.min(end);
        if overlap_start >= overlap_end {
            retained.push(previous);
            continue;
        }

        if previous.start < overlap_start {
            retained.push(RgBufferAccessState {
                end: overlap_start,
                ..previous
            });
        }
        if overlap_end < previous.end {
            retained.push(RgBufferAccessState {
                start: overlap_end,
                ..previous
            });
        }

        let queue_change = previous.queue != queue;
        let ownership_transfer = queue_change && previous.queue_family != queue_family;
        let hazard = previous.is_write || target.is_write;
        let barrier_offset = overlap_start;
        let barrier_size = overlap_end - overlap_start;
        if queue_change {
            let release_barrier = if ownership_transfer {
                RgQueueReleaseBarrier::Buffer(
                    vk::BufferMemoryBarrier2::default()
                        .src_stage_mask(previous.stage)
                        .src_access_mask(previous.access)
                        .dst_stage_mask(vk::PipelineStageFlags2::NONE)
                        .dst_access_mask(vk::AccessFlags2::empty())
                        .src_queue_family_index(previous.queue_family)
                        .dst_queue_family_index(queue_family)
                        .buffer(r.buffer)
                        .offset(barrier_offset)
                        .size(barrier_size),
                )
            } else {
                RgQueueReleaseBarrier::Dependency
            };
            barriers.releases.push(RgQueueRelease {
                pass: previous.last_pass,
                source_family: previous.queue_family,
                source_queue: previous.queue,
                barrier: release_barrier,
            });
            if ownership_transfer || hazard {
                barriers.buffer.push(
                    vk::BufferMemoryBarrier2::default()
                        .src_stage_mask(vk::PipelineStageFlags2::NONE)
                        .src_access_mask(vk::AccessFlags2::empty())
                        .dst_stage_mask(target.stage)
                        .dst_access_mask(target.access)
                        .src_queue_family_index(if ownership_transfer {
                            previous.queue_family
                        } else {
                            vk::QUEUE_FAMILY_IGNORED
                        })
                        .dst_queue_family_index(if ownership_transfer {
                            queue_family
                        } else {
                            vk::QUEUE_FAMILY_IGNORED
                        })
                        .buffer(r.buffer)
                        .offset(barrier_offset)
                        .size(barrier_size),
                );
            }
        } else if hazard {
            barriers.buffer.push(
                vk::BufferMemoryBarrier2::default()
                    .src_stage_mask(previous.stage)
                    .src_access_mask(previous.access)
                    .dst_stage_mask(target.stage)
                    .dst_access_mask(target.access)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(r.buffer)
                    .offset(barrier_offset)
                    .size(barrier_size),
            );
        }

        if !target.is_write && !previous.is_write && !queue_change {
            inherited_read_stage |= previous.stage;
            inherited_read_access |= previous.access;
        }
    }

    retained.push(RgBufferAccessState {
        start,
        end,
        stage: inherited_read_stage,
        access: inherited_read_access,
        is_write: target.is_write,
        queue_family,
        queue,
        last_pass: Some(pass),
    });
    r.buffer_accesses = retained;
    r.last_stage = target.stage;
    r.last_access = target.access;
    r.last_was_write = target.is_write;
    r.touched = true;
}

fn buffer_range_bounds(
    buffer_size: vk::DeviceSize,
    range: Option<RgBufferRange>,
) -> (vk::DeviceSize, vk::DeviceSize) {
    match range {
        Some(range) => {
            let end = range
                .offset
                .checked_add(range.size)
                .expect("RgBufferRange validates its end");
            assert!(
                buffer_size == vk::WHOLE_SIZE || end <= buffer_size,
                "render-graph buffer access exceeds its declared size"
            );
            (range.offset, end)
        }
        None => (0, buffer_size),
    }
}

fn buffer_state_covers(resource: &RgResourceState, range: Option<RgBufferRange>) -> bool {
    let (start, end) = buffer_range_bounds(resource.buffer_size, range);
    let mut spans = resource
        .buffer_accesses
        .iter()
        .filter(|state| state.end > start && state.start < end)
        .map(|state| (state.start.max(start), state.end.min(end)))
        .collect::<Vec<_>>();
    spans.sort_unstable_by_key(|span| span.0);
    let mut cursor = start;
    for (span_start, span_end) in spans {
        if span_start > cursor {
            return false;
        }
        cursor = cursor.max(span_end);
        if cursor >= end {
            return true;
        }
    }
    false
}

/// A frame's render graph: imported resources plus the passes over them. Rebuilt
/// every frame (cheap) and recorded by [`RenderGraph::execute`].
#[derive(Default)]
pub struct RenderGraph {
    resources: Vec<RgResourceState>,
    passes: Vec<RgPass>,
    external_states: Vec<RgExternalState>,
    external_buffer_states: Vec<RgExternalBufferState>,
}

impl RenderGraph {
    /// A fresh, empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a cross-frame image-state slot for [`RenderGraph::import_image`].
    pub fn alloc_external_state(&mut self, initial: RgExternalState) -> usize {
        self.external_states.push(initial);
        self.external_states.len() - 1
    }

    /// The resolved image state currently in a slot.
    pub fn external_state(&self, slot: usize) -> RgExternalState {
        self.external_states[slot]
    }

    /// Allocates a cross-frame buffer-state slot for [`RenderGraph::import_buffer`].
    pub fn alloc_external_buffer_state(&mut self, initial: RgExternalBufferState) -> usize {
        self.external_buffer_states.push(initial);
        self.external_buffer_states.len() - 1
    }

    /// The resolved byte-range state currently in a buffer slot.
    pub fn external_buffer_state(&self, slot: usize) -> RgExternalBufferState {
        self.external_buffer_states[slot].clone()
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
            external_state: external,
            ..RgResourceState::default()
        };
        if let Some(slot) = external {
            let state = self.external_states[slot];
            r.layout = state.layout;
            r.last_stage = state.stage;
            r.last_access = state.access;
            r.last_was_write = state.was_write;
            r.touched = state.touched;
            r.queue_family = state.queue_family;
            r.queue = state.queue;
        } else {
            seed_image_state(&mut r);
        }
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
    pub fn import_buffer(&mut self, buffer: vk::Buffer, external: Option<usize>) -> RgResource {
        let resource = self.register_buffer(RgBufferResource {
            buffer,
            size: vk::WHOLE_SIZE,
            usage: vk::BufferUsageFlags::empty(),
            lifetime: RgBufferLifetime::Imported,
        });
        if let Some(slot) = external {
            let state = &mut self.resources[resource.index as usize];
            state.external_buffer_state = Some(slot);
            state.buffer_accesses = self.external_buffer_states[slot].accesses.clone();
        }
        resource
    }

    /// Registers an imported or graph-allocated buffer in the unified resource table.
    ///
    /// A graph allocator supplies transient and persistent handles with their exact size and
    /// creation usages. External buffers use [`RgBufferLifetime::Imported`].
    pub fn register_buffer(&mut self, resource: RgBufferResource) -> RgResource {
        assert!(resource.size != 0, "render-graph buffers must be non-empty");
        if let Some((index, state)) = self
            .resources
            .iter_mut()
            .enumerate()
            .find(|(_, state)| !state.is_image && state.buffer == resource.buffer)
        {
            if state.buffer_lifetime == RgBufferLifetime::Imported {
                state.buffer_lifetime = resource.lifetime;
            } else {
                assert!(
                    state.buffer_lifetime == resource.lifetime
                        || resource.lifetime == RgBufferLifetime::Imported,
                    "one Vulkan buffer cannot have two graph-owned lifetimes"
                );
            }
            if state.buffer_size == vk::WHOLE_SIZE {
                state.buffer_size = resource.size;
            } else if resource.size != vk::WHOLE_SIZE {
                assert_eq!(
                    state.buffer_size, resource.size,
                    "one Vulkan buffer cannot have conflicting declared sizes"
                );
            }
            state.buffer_usage |= resource.usage;
            return RgResource {
                index: index as u32,
            };
        }
        let r = RgResourceState {
            is_image: false,
            buffer: resource.buffer,
            buffer_size: resource.size,
            buffer_usage: resource.usage,
            buffer_lifetime: resource.lifetime,
            ..RgResourceState::default()
        };
        self.resources.push(r);
        RgResource {
            index: (self.resources.len() - 1) as u32,
        }
    }

    /// Allocates and registers a graph-owned buffer through the frame-safe resource pool.
    pub fn create_buffer(
        &mut self,
        resources: &mut crate::RenderGraphResources,
        frame: usize,
        key: &'static str,
        desc: RgBufferDesc,
    ) -> crate::Result<RgResource> {
        let resource = resources.allocate_graph_buffer(frame, key, desc)?;
        let handle = resource.buffer;
        let graph_resource = self.register_buffer(resource);
        let index = graph_resource.index as usize;
        if self.resources[index].external_buffer_state.is_none() {
            let slot = self.alloc_external_buffer_state(resources.buffer_state(handle));
            self.resources[index].external_buffer_state = Some(slot);
            self.resources[index].buffer_accesses = self.external_buffer_states[slot].accesses.clone();
        }
        Ok(graph_resource)
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

    /// Exact declaration for a registered buffer resource, or `None` for an image.
    pub fn buffer_resource(&self, resource: RgResource) -> Option<RgBufferResource> {
        let state = &self.resources[resource.index as usize];
        (!state.is_image).then_some(RgBufferResource {
            buffer: state.buffer,
            size: state.buffer_size,
            usage: state.buffer_usage,
            lifetime: state.buffer_lifetime,
        })
    }

    pub(crate) fn resolved_graph_buffer_states(
        &self,
    ) -> Vec<(vk::Buffer, RgExternalBufferState)> {
        self.resources
            .iter()
            .filter(|resource| {
                !resource.is_image && resource.buffer_lifetime != RgBufferLifetime::Imported
            })
            .filter_map(|resource| {
                resource.external_buffer_state.map(|slot| {
                    (
                        resource.buffer,
                        self.external_buffer_states[slot].clone(),
                    )
                })
            })
            .collect()
    }

    /// Resolves queue preferences without changing pass declarations.
    pub fn queue_assignments(&self, families: RgQueueFamilies) -> Vec<RgQueueAssignment> {
        self.passes
            .iter()
            .map(|pass| self.resolve_queue(pass, families).0)
            .collect()
    }

    fn resolve_queue(&self, pass: &RgPass, families: RgQueueFamilies) -> (RgQueueAssignment, u32) {
        let resolved = families.resolve(pass);
        if resolved.0 == RgQueueAssignment::AsyncCompute
            && (pass.accesses.is_empty()
                || !pass.accesses.iter().all(|access| {
                    let resource = &self.resources[access.resource.index as usize];
                    if resource.is_image {
                        resource.external_state.is_some() && resource.queue.is_some()
                    } else {
                        resource.buffer_lifetime != RgBufferLifetime::Imported
                            || (resource.external_buffer_state.is_some()
                                && buffer_state_covers(resource, access.buffer_range))
                    }
                }))
        {
            return (RgQueueAssignment::Graphics, families.graphics);
        }
        resolved
    }

    /// Compiles pass-local synchronization and queue ownership for a queue topology.
    ///
    /// This pure preview leaves the graph's execution state untouched. The frame submitter uses
    /// the returned queue assignments, emits `after_*` releases on source queues, signals a
    /// timeline semaphore, then emits matching `before_*` acquires on destination queues.
    pub fn barrier_schedule(&self, families: RgQueueFamilies) -> Vec<RgPassBarriers> {
        self.compile_barriers(families).0
    }

    /// Compiles barriers into maximal contiguous queue batches.
    pub fn submission_plan(&self, families: RgQueueFamilies) -> RgSubmissionPlan {
        let (passes, exit_resources, entry_releases) = self.compile_barriers(families);
        let mut batches = Vec::<RgPassBatch>::new();
        let mut entry_batch_queues = Vec::<RgQueueAssignment>::new();
        for release in &entry_releases {
            assert_eq!(
                families.family(release.source_queue),
                Some(release.source_family),
                "persisted queue identity and family must match the device topology"
            );
            if entry_batch_queues.contains(&release.source_queue) {
                continue;
            }
            entry_batch_queues.push(release.source_queue);
            batches.push(RgPassBatch {
                queue: release.source_queue,
                passes: 0..0,
                wait_for_batches: Vec::new(),
                entry_release_images: Vec::new(),
                entry_release_buffers: Vec::new(),
            });
        }
        let mut pass_to_batch = Vec::with_capacity(passes.len());
        for (pass_index, pass) in passes.iter().enumerate() {
            let batch_index = if let Some(last) = batches.last_mut()
                && last.queue == pass.queue
            {
                last.passes.end = pass_index + 1;
                batches.len() - 1
            } else {
                batches.push(RgPassBatch {
                    queue: pass.queue,
                    passes: pass_index..pass_index + 1,
                    wait_for_batches: Vec::new(),
                    entry_release_images: Vec::new(),
                    entry_release_buffers: Vec::new(),
                });
                batches.len() - 1
            };
            pass_to_batch.push(batch_index);
        }
        for release in entry_releases {
            let release_batch = entry_batch_queues
                .iter()
                .position(|queue| *queue == release.source_queue)
                .expect("entry release queue was batched");
            match release.barrier {
                RgQueueReleaseBarrier::Image(barrier) => {
                    batches[release_batch].entry_release_images.push(barrier);
                }
                RgQueueReleaseBarrier::Buffer(barrier) => {
                    batches[release_batch].entry_release_buffers.push(barrier);
                }
                RgQueueReleaseBarrier::Dependency => {}
            }
            let destination_batch = pass_to_batch[release.destination_pass];
            if !batches[destination_batch]
                .wait_for_batches
                .contains(&release_batch)
            {
                batches[destination_batch]
                    .wait_for_batches
                    .push(release_batch);
            }
        }
        for (pass_index, pass) in passes.iter().enumerate() {
            let batch_index = pass_to_batch[pass_index];
            for &source_pass in &pass.wait_for_passes {
                let source_batch = pass_to_batch[source_pass];
                if source_batch != batch_index
                    && !batches[batch_index]
                        .wait_for_batches
                        .contains(&source_batch)
                {
                    batches[batch_index].wait_for_batches.push(source_batch);
                }
            }
        }
        RgSubmissionPlan {
            passes,
            batches,
            exit_resources,
        }
    }

    fn compile_barriers(
        &self,
        families: RgQueueFamilies,
    ) -> (
        Vec<RgPassBarriers>,
        Vec<RgResourceState>,
        Vec<RgEntryRelease>,
    ) {
        let mut resources = self.resources.clone();
        let assignments = self
            .passes
            .iter()
            .map(|pass| self.resolve_queue(pass, families))
            .collect::<Vec<_>>();
        let mut schedule = assignments
            .iter()
            .map(|&(queue, queue_family)| RgPassBarriers {
                queue,
                queue_family,
                wait_for_passes: Vec::new(),
                before_images: Vec::new(),
                before_buffers: Vec::new(),
                after_images: Vec::new(),
                after_buffers: Vec::new(),
            })
            .collect::<Vec<_>>();

        let mut entry_releases = Vec::new();
        for (pass_index, pass) in self.passes.iter().enumerate() {
            let queue_family = assignments[pass_index].1;
            let barriers = derive_pass_barriers_for(
                &mut resources,
                pass,
                pass_index,
                assignments[pass_index].0,
                queue_family,
            );
            schedule[pass_index].before_images = barriers.image;
            schedule[pass_index].before_buffers = barriers.buffer;
            for release in barriers.releases {
                if let Some(source_pass) = release.pass {
                    if !schedule[pass_index].wait_for_passes.contains(&source_pass) {
                        schedule[pass_index].wait_for_passes.push(source_pass);
                    }
                    match release.barrier {
                        RgQueueReleaseBarrier::Image(barrier) => {
                            schedule[source_pass].after_images.push(barrier);
                        }
                        RgQueueReleaseBarrier::Buffer(barrier) => {
                            schedule[source_pass].after_buffers.push(barrier);
                        }
                        RgQueueReleaseBarrier::Dependency => {}
                    }
                } else {
                    entry_releases.push(RgEntryRelease {
                        destination_pass: pass_index,
                        source_family: release.source_family,
                        source_queue: release.source_queue,
                        barrier: release.barrier,
                    });
                }
            }
        }
        (schedule, resources, entry_releases)
    }

    /// Derives the barriers a pass needs from its declared accesses and attachments,
    /// advancing the resource table. Color/depth attachments are treated as the
    /// matching write usage; an MSAA resolve target is a second write of that kind.
    /// Pure logic — no GPU — so it is the unit-tested core.
    #[cfg(test)]
    fn derive_pass_barriers(
        &mut self,
        pass: &RgPass,
        pass_index: usize,
        queue: RgQueueAssignment,
        queue_family: u32,
    ) -> DerivedBarriers {
        derive_pass_barriers_for(&mut self.resources, pass, pass_index, queue, queue_family)
    }
}

fn derive_pass_barriers_for(
    resources: &mut [RgResourceState],
    pass: &RgPass,
    pass_index: usize,
    queue: RgQueueAssignment,
    queue_family: u32,
) -> DerivedBarriers {
    let mut barriers = DerivedBarriers::default();
    for access in &pass.accesses {
        let resource = &resources[access.resource.index as usize];
        validate_declared_access(resource, access.usage);
        assert!(
            !resource.is_image || access.buffer_range.is_none(),
            "an image access cannot declare a buffer byte range"
        );
        apply_access_queued(
            &mut resources[access.resource.index as usize],
            usage_info(access.usage),
            access.buffer_range,
            pass_index,
            queue,
            queue_family,
            &mut barriers,
        );
    }
    for att in &pass.colors {
        assert!(
            resources[att.resource.index as usize].is_image,
            "a color attachment must be an image"
        );
        apply_access_queued(
            &mut resources[att.resource.index as usize],
            usage_info(RgUsage::ColorWrite),
            None,
            pass_index,
            queue,
            queue_family,
            &mut barriers,
        );
        if let Some(resolve) = att.resolve {
            assert!(
                resources[resolve.index as usize].is_image,
                "a color resolve attachment must be an image"
            );
            apply_access_queued(
                &mut resources[resolve.index as usize],
                usage_info(RgUsage::ColorWrite),
                None,
                pass_index,
                queue,
                queue_family,
                &mut barriers,
            );
        }
    }
    if let Some(depth) = &pass.depth {
        assert!(
            resources[depth.resource.index as usize].is_image,
            "a depth attachment must be an image"
        );
        apply_access_queued(
            &mut resources[depth.resource.index as usize],
            usage_info(RgUsage::DepthWrite),
            None,
            pass_index,
            queue,
            queue_family,
            &mut barriers,
        );
        if let Some(resolve) = depth.resolve {
            assert!(
                resources[resolve.index as usize].is_image,
                "a depth resolve attachment must be an image"
            );
            apply_access_queued(
                &mut resources[resolve.index as usize],
                usage_info(RgUsage::DepthWrite),
                None,
                pass_index,
                queue,
                queue_family,
                &mut barriers,
            );
        }
    }
    barriers
}

impl RenderGraph {
    /// Records a compiled multi-queue plan into one primary command buffer per batch.
    pub fn record_submission_plan_profiled(
        &mut self,
        device: &Device,
        plan: RgSubmissionPlan,
        commands: RgBatchCommandBuffers<'_>,
        recorders: &mut ProfileRecorders<'_>,
    ) -> crate::Result<Vec<RgRecordedBatch>> {
        self.record_compiled_plan_profiled(device, plan, commands, recorders, true)
    }

    fn record_compiled_plan_profiled(
        &mut self,
        device: &Device,
        plan: RgSubmissionPlan,
        commands: RgBatchCommandBuffers<'_>,
        recorders: &mut ProfileRecorders<'_>,
        manage_command_buffers: bool,
    ) -> crate::Result<Vec<RgRecordedBatch>> {
        if commands.graphics.len() != plan.graphics_batch_count()
            || commands.compute.len() != plan.compute_batch_count()
            || plan.passes.len() != self.passes.len()
        {
            return Err(crate::Error::InvalidUploadData(
                "render-graph submission plan does not match its command buffers".to_owned(),
            ));
        }
        self.resources = plan.exit_resources;
        let raw = device.raw();
        let mut passes = std::mem::take(&mut self.passes)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>();
        let mut graphics_index = 0;
        let mut compute_index = 0;
        let mut recorded = Vec::with_capacity(plan.batches.len());
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        for batch in &plan.batches {
            let command_buffer = match batch.queue {
                RgQueueAssignment::Graphics => {
                    let command = commands.graphics[graphics_index];
                    graphics_index += 1;
                    command
                }
                RgQueueAssignment::AsyncCompute => {
                    let command = commands.compute[compute_index];
                    compute_index += 1;
                    command
                }
            };
            if manage_command_buffers {
                crate::checked(
                    unsafe { raw.begin_command_buffer(command_buffer, &begin) },
                    "begin render-graph batch",
                )?;
            }

            for pass_index in batch.passes.clone() {
                let pass = passes[pass_index]
                    .take()
                    .expect("each render-graph pass belongs to one batch");
                let barriers = &plan.passes[pass_index];
                let gpu_timestamps_supported = batch.queue == RgQueueAssignment::Graphics
                    || device
                        .compute_timestamp_valid_bits
                        .is_some_and(|bits| bits != 0);
                let cpu_index = recorders.cpu.as_mut().map(|(registry, buffer)| {
                    buffer.begin_span(registry, &pass.name, cpu_now_ns())
                });
                let gpu_index = gpu_timestamps_supported
                    .then(|| {
                        recorders.gpu.as_mut().and_then(|timestamps| {
                            timestamps.begin_scope(raw, command_buffer, &pass.name)
                        })
                    })
                    .flatten();
                emit_barriers(
                    raw,
                    command_buffer,
                    &barriers.before_images,
                    &barriers.before_buffers,
                );
                if batch.queue == RgQueueAssignment::Graphics
                    && let (Some(index), Some(timestamps)) =
                        (gpu_index, recorders.gpu.as_mut())
                {
                    let pixels =
                        u64::from(pass.render_area.width) * u64::from(pass.render_area.height);
                    let _ = timestamps.reserve_stats_slot(index, pixels);
                }
                {
                    let gpu = if gpu_timestamps_supported {
                        recorders.gpu.as_deref_mut()
                    } else {
                        None
                    };
                    let mut nested = NestedScopeRecorder::new(
                        raw,
                        command_buffer,
                        gpu,
                        recorders.cpu.as_mut().map(|(r, b)| (&mut **r, &mut **b)),
                    );
                    match pass.kind {
                        RgPassKind::Graphics => {
                            self.record_graphics(device, command_buffer, pass, &mut nested);
                        }
                        RgPassKind::GraphicsCommands | RgPassKind::Compute => {
                            if let Some(body) = pass.execute {
                                body(command_buffer, &mut nested);
                            }
                        }
                    }
                }
                emit_barriers(
                    raw,
                    command_buffer,
                    &barriers.after_images,
                    &barriers.after_buffers,
                );
                if gpu_timestamps_supported && let Some(timestamps) = recorders.gpu.as_mut() {
                    timestamps.end_scope(raw, command_buffer, gpu_index);
                }
                if let (Some(index), Some((_, buffer))) = (cpu_index, recorders.cpu.as_mut()) {
                    buffer.end_span(index, cpu_now_ns());
                }
            }
            emit_barriers(
                raw,
                command_buffer,
                &batch.entry_release_images,
                &batch.entry_release_buffers,
            );
            if manage_command_buffers {
                crate::checked(
                    unsafe { raw.end_command_buffer(command_buffer) },
                    "end render-graph batch",
                )?;
            }
            recorded.push(RgRecordedBatch {
                queue: batch.queue,
                command_buffer,
                wait_for_batches: batch.wait_for_batches.clone(),
                passes: batch.passes.clone(),
            });
        }
        self.write_external_states();
        Ok(recorded)
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
        let plan =
            self.submission_plan(RgQueueFamilies::graphics_only(device.graphics_queue_family));
        debug_assert_eq!(plan.compute_batch_count(), 0);
        debug_assert!(plan.graphics_batch_count() <= 1);
        let graphics = (plan.graphics_batch_count() != 0).then_some(cmd);
        self.record_compiled_plan_profiled(
            device,
            plan,
            RgBatchCommandBuffers {
                graphics: graphics.as_slice(),
                compute: &[],
            },
            recorders,
            false,
        )
        .expect("graphics-only render-graph recording has matching command buffers");
    }

    fn write_external_states(&mut self) {
        for r in &self.resources {
            if let Some(slot) = r.external_state {
                self.external_states[slot] = RgExternalState {
                    layout: r.layout,
                    queue_family: r.queue_family,
                    queue: r.queue,
                    stage: r.last_stage,
                    access: r.last_access,
                    was_write: r.last_was_write,
                    touched: r.touched,
                };
            }
            if let Some(slot) = r.external_buffer_state {
                self.external_buffer_states[slot] = RgExternalBufferState {
                    accesses: r.buffer_accesses.clone(),
                };
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

fn emit_barriers(
    raw: &ash::Device,
    command_buffer: vk::CommandBuffer,
    images: &[vk::ImageMemoryBarrier2<'static>],
    buffers: &[vk::BufferMemoryBarrier2<'static>],
) {
    if images.is_empty() && buffers.is_empty() {
        return;
    }
    let dependency = vk::DependencyInfo::default()
        .image_memory_barriers(images)
        .buffer_memory_barriers(buffers);
    unsafe { raw.cmd_pipeline_barrier2(command_buffer, &dependency) };
}

#[cfg(test)]
mod tests {
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

    fn compute_on_graphics(name: &str) -> RgPass {
        let mut pass = RgPass::compute(name);
        pass.queue = RgQueuePreference::Graphics;
        pass
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
        let state = graph
            .alloc_external_buffer_state(owned_buffer_state(RgQueueAssignment::AsyncCompute, 5));
        let buffer = graph.import_buffer(vk::Buffer::null(), Some(state));
        let range = RgBufferRange::new(32, 64).unwrap();
        graph.add_pass(RgPass::compute("count").access_buffer(
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
        graph.add_pass(RgPass::compute("count").access(buffer, RgUsage::StorageWriteCompute));
        graph.add_pass(RgPass::compute("consume").access(buffer, RgUsage::StorageReadCompute));

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
            RgPass::compute("tess-factor")
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
    fn submission_plan_batches_contiguous_queues_and_links_dependencies() {
        let mut graph = RenderGraph::new();
        let first_state = graph
            .alloc_external_buffer_state(owned_buffer_state(RgQueueAssignment::AsyncCompute, 5));
        let first = graph.import_buffer(vk::Buffer::from_raw(1), Some(first_state));
        let second_state =
            graph.alloc_external_buffer_state(owned_buffer_state(RgQueueAssignment::Graphics, 2));
        let second = graph.import_buffer(vk::Buffer::from_raw(2), Some(second_state));
        let whole = RgBufferRange::new(0, 256).unwrap();
        graph.add_pass(RgPass::compute("async-produce").access_buffer(
            first,
            whole,
            RgUsage::StorageWriteCompute,
        ));
        graph.add_pass(
            compute_on_graphics("graphics-transfer")
                .access_buffer(first, whole, RgUsage::StorageReadCompute)
                .access_buffer(second, whole, RgUsage::StorageWriteCompute),
        );
        graph.add_pass(RgPass::compute("async-consume").access_buffer(
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
        graph.add_pass(RgPass::compute("compute-a").access(image, RgUsage::SampledReadCompute));
        graph.add_pass(RgPass::compute("compute-b").access(image, RgUsage::SampledReadCompute));
        graph.add_pass(compute_on_graphics("graphics-a"));
        graph.add_pass(compute_on_graphics("graphics-b"));

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
        graph
            .add_pass(RgPass::compute("async-write").access(image, RgUsage::StorageImageRwCompute));

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
        graph.add_pass(
            compute_on_graphics("graphics-read").access(image, RgUsage::SampledReadCompute),
        );

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
                name: self.name.clone(),
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
}
