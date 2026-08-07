//! The CPU-side span recorder: the marker registry, the per-frame span buffer, and the
//! monotonic clock the GPU timestamps correlate against.

use super::*;

/// Interns scope names to stable integer ids so a [`CpuSpan`] stays string-free.
///
/// Every pass interns on every frame and the name set is fixed after the first, so the hot path is
/// one hash lookup — a linear scan over the table would be quadratic in the pass count.
#[derive(Default)]
pub struct CpuMarkerRegistry {
    pub(super) names: Vec<String>,
    ids: std::collections::HashMap<String, u32>,
}

impl CpuMarkerRegistry {
    /// Maps a name to its stable id, interning it on first sight.
    pub fn id(&mut self, name: &str) -> u32 {
        if let Some(id) = self.ids.get(name) {
            return *id;
        }
        let id = u32::try_from(self.names.len()).expect("profile marker count fits u32");
        self.names.push(name.to_string());
        self.ids.insert(name.to_string(), id);
        id
    }

    /// The name for an id.
    pub fn name(&self, id: u32) -> &str {
        &self.names[id as usize]
    }
}

/// A recorded CPU span: a `[start_ns, end_ns)` interval on the render thread for one
/// pass, with its nesting. steady_clock ns; the origin is arbitrary but shared within
/// a frame, so spans are directly comparable.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuSpan {
    /// The index into [`CpuMarkerRegistry`].
    pub marker: u32,
    /// Begin (ns).
    pub start_ns: u64,
    /// End (ns).
    pub end_ns: u64,
    /// Nesting depth.
    pub depth: u32,
    /// The enclosing span index in the same buffer, or `-1` at top level.
    pub parent: i32,
}

/// One frame-in-flight's CPU-span sink plus the open-scope cursor. Recording is on the
/// single render thread, so the open parent/depth live here, not in a thread-local.
#[derive(Default)]
pub struct CpuSpanBuffer {
    /// The recorded spans.
    pub spans: Vec<CpuSpan>,
    pub(super) open_parent: i32,
    pub(super) open_depth: u32,
}

impl CpuSpanBuffer {
    /// Clears the buffer for a fresh frame.
    pub fn reset(&mut self) {
        self.spans.clear();
        self.open_parent = -1;
        self.open_depth = 0;
    }

    /// Opens a span under the current open scope, recording the steady-clock begin.
    /// Returns the span index, paired with [`CpuSpanBuffer::end_span`].
    pub fn begin_span(
        &mut self,
        registry: &mut CpuMarkerRegistry,
        name: &str,
        now_ns: u64,
    ) -> usize {
        let marker = registry.id(name);
        let index = self.spans.len();
        self.spans.push(CpuSpan {
            marker,
            start_ns: now_ns,
            end_ns: 0,
            depth: self.open_depth,
            parent: self.open_parent,
        });
        self.open_parent = index as i32;
        self.open_depth += 1;
        index
    }

    /// Closes a span opened by [`CpuSpanBuffer::begin_span`], recording the end.
    pub fn end_span(&mut self, index: usize, now_ns: u64) {
        let prev_parent = self.spans[index].parent;
        self.spans[index].end_ns = now_ns;
        self.open_parent = prev_parent;
        self.open_depth = self.open_depth.saturating_sub(1);
    }
}

/// The CPU side of the profiler: the persistent name registry plus one span buffer per
/// frame-in-flight, mirroring [`GpuProfiler`]'s slot discipline.
#[derive(Default)]
pub struct CpuProfiler {
    /// The interned scope-name registry.
    pub registry: CpuMarkerRegistry,
    /// One span buffer per frame-in-flight.
    pub buffers: [CpuSpanBuffer; MAX_FRAMES_IN_FLIGHT],
}
