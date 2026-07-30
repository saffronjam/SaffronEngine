//! Nested profiler scopes for pass bodies: a lifetime-safe handle that brackets sub-phases of a
//! pass's work in child GPU + CPU timestamp scopes, nested under the graph's per-pass scope.

use ash::vk;

use crate::profiler::{CpuMarkerRegistry, CpuSpanBuffer, RgTimestamps, cpu_now_ns};

/// A handle threaded to every pass body, carrying the armed profiler recorders for the
/// current pass. Nested scopes are opened through [`NestedScopeRecorder::scope`]; the
/// recorder is a cheap set of `None`s when profiling is unarmed, so a body's `scope`
/// calls compile down to the closure invocation alone in the common case.
pub struct NestedScopeRecorder<'a> {
    raw: &'a ash::Device,
    cmd: vk::CommandBuffer,
    gpu: Option<&'a mut RgTimestamps>,
    cpu: Option<(&'a mut CpuMarkerRegistry, &'a mut CpuSpanBuffer)>,
}

impl<'a> NestedScopeRecorder<'a> {
    /// Builds the recorder from the graph's per-pass borrows. `None` recorders make
    /// [`NestedScopeRecorder::scope`] a transparent wrapper around its closure.
    pub fn new(
        raw: &'a ash::Device,
        cmd: vk::CommandBuffer,
        gpu: Option<&'a mut RgTimestamps>,
        cpu: Option<(&'a mut CpuMarkerRegistry, &'a mut CpuSpanBuffer)>,
    ) -> Self {
        Self { raw, cmd, gpu, cpu }
    }

    /// Brackets `f` in a child GPU + CPU timestamp scope named `name`, nesting under the
    /// enclosing pass scope. The begin/end timestamps use `TOP_OF_PIPE`/`BOTTOM_OF_PIPE`
    /// like the pass scopes and are legal inside a dynamic-rendering scope. Returns `f`'s
    /// value. A no-op wrapper when the recorders are unarmed.
    pub fn scope<R>(&mut self, name: &str, f: impl FnOnce(vk::CommandBuffer) -> R) -> R {
        let gpu_index = self
            .gpu
            .as_deref_mut()
            .and_then(|ts| ts.begin_scope(self.raw, self.cmd, name));
        let cpu_index = self
            .cpu
            .as_mut()
            .map(|(registry, buffer)| buffer.begin_span(registry, name, cpu_now_ns()));
        let result = f(self.cmd);
        if let Some(ts) = self.gpu.as_deref_mut() {
            ts.end_scope(self.raw, self.cmd, gpu_index);
        }
        if let (Some(index), Some((_, buffer))) = (cpu_index, self.cpu.as_mut()) {
            buffer.end_span(index, cpu_now_ns());
        }
        result
    }
}
