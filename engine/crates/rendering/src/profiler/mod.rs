//! The GPU/CPU profiler: per-pass GPU timestamps + pipeline statistics, the CPU span recorder, the
//! calibrated-timestamp correlation, and the bounded capture state machine, armed through the
//! render graph's [`RgTimestamps`] / [`CpuRecorder`] hooks.
//!
//! [`ProfilerMode::Off`] allocates no query pools, reads no VMA budget, and leaves the recorders
//! unarmed, so every scope is a cheap branch. The read-back is non-blocking: a slot's pool is read
//! [`crate::MAX_FRAMES_IN_FLIGHT`] frames later, after that slot's fence has signalled.

mod capture;
mod cpu;

use ash::vk;

use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::{Device, checked};

pub use capture::*;
pub use cpu::*;

/// Upper bound on GPU scopes the profiler times per frame — top-level passes plus any
/// nested sub-scopes (the scene pass opens child scopes for its opaque / submission phases).
pub const MAX_PROFILED_SCOPES: u32 = 160;

/// The pipeline-statistics counters captured per pass, in ascending
/// `VkQueryPipelineStatisticFlagBits` bit order (the order `vkGetQueryPoolResults`
/// returns them). The read-back decodes positionally, so this set and [`PIPELINE_STATS_COUNT`]
/// stay in lockstep.
pub fn pipeline_stats_flags() -> vk::QueryPipelineStatisticFlags {
    vk::QueryPipelineStatisticFlags::INPUT_ASSEMBLY_VERTICES
        | vk::QueryPipelineStatisticFlags::VERTEX_SHADER_INVOCATIONS
        | vk::QueryPipelineStatisticFlags::CLIPPING_INVOCATIONS
        | vk::QueryPipelineStatisticFlags::CLIPPING_PRIMITIVES
        | vk::QueryPipelineStatisticFlags::FRAGMENT_SHADER_INVOCATIONS
        | vk::QueryPipelineStatisticFlags::COMPUTE_SHADER_INVOCATIONS
}

/// The number of pipeline-statistics counters in [`pipeline_stats_flags`].
pub const PIPELINE_STATS_COUNT: usize = 6;

/// How much per-frame instrumentation the GPU profiler captures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProfilerMode {
    /// No queries, no VMA budget read — the present-only baseline cost.
    #[default]
    Off,
    /// Per-pass GPU timestamps + throughput counters + VMA budget.
    Timestamps,
    /// [`ProfilerMode::Timestamps`] plus per-pass pipeline-statistics (deepest).
    PipelineStats,
}

/// Raw pipeline-statistics counts for one pass. The consumer derives the ratios:
/// overdraw (`fragment_invocations / pixels`), culling efficiency
/// (`clipping_primitives / clipping_invocations`), vertex reuse
/// (`vertex_invocations / input_vertices`); `compute_invocations` sizes the GI/lighting
/// compute passes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PipelineStats {
    /// Input-assembly vertices.
    pub input_vertices: u64,
    /// Vertex-shader invocations.
    pub vertex_invocations: u64,
    /// Clipping-stage primitive-test invocations.
    pub clipping_invocations: u64,
    /// Primitives surviving clipping.
    pub clipping_primitives: u64,
    /// Fragment-shader invocations.
    pub fragment_invocations: u64,
    /// Compute-shader invocations.
    pub compute_invocations: u64,
    /// Render-area pixels for the overdraw ratio (0 for compute / no query).
    pub pixels: u64,
}

/// One GPU scope's measured time for a frame, plus its place in the scope tree.
/// `gpu_ms` is the wall-clock span between begin/end timestamps — relative, since
/// sibling scopes can overlap on the GPU. `start_ns`/`end_ns` are frame-relative (from
/// the earliest begin) unless calibrated, in which case they sit on the CPU clock axis.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PassTiming {
    /// The scope name.
    pub name: String,
    /// The wall-clock GPU span (ms).
    pub gpu_ms: f32,
    /// Frame-relative begin (ns), or absolute host ns when correlated.
    pub start_ns: u64,
    /// Frame-relative end (ns), or absolute host ns when correlated.
    pub end_ns: u64,
    /// The enclosing scope's index in this list, or `-1` at top level.
    pub parent_index: i32,
    /// Nesting depth.
    pub depth: u32,
    /// Whether `stats` is populated (PipelineStats-mode top-level passes).
    pub has_stats: bool,
    /// Pipeline statistics, populated only when `has_stats`.
    pub stats: PipelineStats,
}

/// One recorded GPU scope: its name and nesting. The i-th record owns query slots `2i`
/// (begin) and `2i+1` (end). Kept flat-and-tagged, not a literal tree — async compute
/// makes a single nested wall-clock tree ambiguous, so the consumer decodes the tree.
#[derive(Clone, Debug, Default)]
pub struct ScopeRecord {
    /// The scope name.
    pub name: String,
    /// The enclosing scope's record index, or `-1` at top level.
    pub parent_index: i32,
    /// Nesting depth.
    pub depth: u32,
    /// The pipeline-stats query slot (top-level passes only), or `-1`.
    pub stats_slot: i32,
    /// Render-area pixels at stats time, for the overdraw ratio.
    pub pixels: u64,
}

/// The GPU timestamp recorder armed per frame by [`GpuProfiler`]. A scope grabs the
/// next free query-slot pair on begin and end, pushing a [`ScopeRecord`] so the nesting
/// *is* the hierarchy. A `None` pool disables recording (the unarmed `Off` case). The
/// render graph and pass bodies only record into it (the "no pass writes a query by
/// hand" analogue).
#[derive(Default)]
pub struct RgTimestamps {
    /// `None` => timestamp capture disabled this frame.
    pub pool: Option<vk::QueryPool>,
    /// Total query slots in the pool (2 per scope).
    pub capacity: u32,
    /// One record per scope, in begin order.
    pub records: Vec<ScopeRecord>,
    /// Next free query slot (begin = `next_slot`, end = `+1`).
    pub next_slot: u32,
    /// Innermost open scope's record index, or `-1`.
    pub open_scope: i32,
    /// Current nesting depth.
    pub depth: u32,
    /// `None` => no pipeline-stats queries this frame.
    pub stats_pool: Option<vk::QueryPool>,
    /// Total stats query slots.
    pub stats_capacity: u32,
    /// Next free stats slot.
    pub next_stats_slot: u32,
}

impl RgTimestamps {
    /// Whether recording is armed (a pool is bound).
    pub fn armed(&self) -> bool {
        self.pool.is_some()
    }

    /// Opens a GPU scope: writes a begin timestamp and pushes a [`ScopeRecord`] under
    /// the current open scope. Returns the record index, or `None` when inactive (no
    /// pool, or the pool is full — overflow truncates gracefully as a begin-order
    /// prefix of the tree, so parents always precede children).
    ///
    /// The caller must pair this with [`RgTimestamps::end_scope`] after the body. On a
    /// graphics pass the begin timestamp uses `TOP_OF_PIPE`, the end `BOTTOM_OF_PIPE`.
    pub fn begin_scope(
        &mut self,
        raw: &ash::Device,
        cmd: vk::CommandBuffer,
        name: &str,
    ) -> Option<usize> {
        let pool = self.pool?;
        if self.next_slot + 1 >= self.capacity {
            return None;
        }
        let begin_slot = self.next_slot;
        self.next_slot += 2;
        let prev_parent = self.open_scope;
        let record_index = self.records.len();
        self.records.push(ScopeRecord {
            name: name.to_string(),
            parent_index: prev_parent,
            depth: self.depth,
            stats_slot: -1,
            pixels: 0,
        });
        self.open_scope = record_index as i32;
        self.depth += 1;
        // SAFETY: the ash seam. `cmd` is recording; `pool`/`begin_slot` are valid (slot
        // pair reserved above, within `capacity`).
        unsafe {
            raw.cmd_write_timestamp2(cmd, vk::PipelineStageFlags2::TOP_OF_PIPE, pool, begin_slot);
        }
        Some(record_index)
    }

    /// Closes the scope `begin_scope` opened (identified by its `record_index`): writes
    /// the end timestamp and restores the open-scope cursor. A `None` index (an inactive
    /// scope) is a no-op. `begin_slot` is `2 * record_index` (slots are reserved in
    /// begin order), so the end slot is `2 * record_index + 1`.
    pub fn end_scope(
        &mut self,
        raw: &ash::Device,
        cmd: vk::CommandBuffer,
        record_index: Option<usize>,
    ) {
        let Some(index) = record_index else { return };
        let Some(pool) = self.pool else { return };
        let begin_slot = (index as u32) * 2;
        let parent = self.records[index].parent_index;
        self.open_scope = parent;
        self.depth = self.depth.saturating_sub(1);
        // SAFETY: the ash seam. `cmd` is recording; `pool`/`begin_slot + 1` are valid.
        unsafe {
            raw.cmd_write_timestamp2(
                cmd,
                vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
                pool,
                begin_slot + 1,
            );
        }
    }

    /// Reserves a pipeline-stats query slot for the top-level pass whose scope is at
    /// `record_index`, stamping its render-area pixels. Returns the stats slot, or
    /// `None` when stats are not armed or the stats pool is full.
    pub fn reserve_stats_slot(&mut self, record_index: usize, pixels: u64) -> Option<u32> {
        self.stats_pool?;
        if self.next_stats_slot >= self.stats_capacity {
            return None;
        }
        let slot = self.next_stats_slot;
        self.next_stats_slot += 1;
        self.records[record_index].stats_slot = slot as i32;
        self.records[record_index].pixels = pixels;
        Some(slot)
    }
}

/// VK_EXT_calibrated_timestamps state: the offset that projects a GPU tick onto the CPU
/// steady_clock axis. `correlated` is false until a sample lands (or stays false when
/// the extension/host domain is absent) — the read-back then keeps GPU spans on their
/// own frame-relative axis.
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuCalibration {
    /// Extension present + a host domain matching the steady clock.
    pub available: bool,
    /// The calibrateable host domain (the `CLOCK_MONOTONIC` axis `cpu_now_ns` samples).
    pub host_domain: vk::TimeDomainEXT,
    /// Additive ns offset, device-ns → host-ns.
    pub device_to_host_ns_offset: i64,
    /// Sample confidence; larger = looser correlation.
    pub max_deviation_ns: u64,
    /// Whether a valid offset has been sampled this session.
    pub correlated: bool,
    /// The frame serial of the last sample, gating the periodic re-sample.
    pub last_calibrated_serial: u64,
}

/// The GPU profiler: per-frame timestamp/stats query pools, the recorded scopes per
/// slot, and the last completed read-back.
pub struct GpuProfiler {
    /// The active capture level.
    pub mode: ProfilerMode,
    /// Whether the query pools are allocated.
    pub pools_ready: bool,
    /// Opt-in: instrument pass interiors as nested sub-scopes.
    pub sub_scopes: bool,
    /// The calibrated-timestamp correlation state.
    pub calibration: GpuCalibration,
    timestamp_pools: [Option<vk::QueryPool>; MAX_FRAMES_IN_FLIGHT],
    stats_pools: [Option<vk::QueryPool>; MAX_FRAMES_IN_FLIGHT],
    /// The scopes recorded into slot `i` this cycle, consumed when slot `i` is read back.
    recorded_scopes: [Vec<ScopeRecord>; MAX_FRAMES_IN_FLIGHT],
    /// The last completed read-back (the scope tree, flat).
    pub last_timings: Vec<PassTiming>,
    /// The raw span of the last read-back (ms).
    pub last_gpu_total_ms: f32,
    /// ns per timestamp tick (the device limit).
    pub timestamp_period: f32,
    /// The common valid-bit mask used for every recorded queue.
    pub timestamp_mask: u64,
    /// The graphics queue exposes timestamp bits.
    pub timestamps_supported: bool,
    /// The `pipelineStatisticsQuery` device feature present.
    pub pipeline_stats_supported: bool,
}

impl Default for GpuProfiler {
    fn default() -> Self {
        Self {
            mode: ProfilerMode::Off,
            pools_ready: false,
            sub_scopes: false,
            calibration: GpuCalibration::default(),
            timestamp_pools: [None; MAX_FRAMES_IN_FLIGHT],
            stats_pools: [None; MAX_FRAMES_IN_FLIGHT],
            recorded_scopes: std::array::from_fn(|_| Vec::new()),
            last_timings: Vec::new(),
            last_gpu_total_ms: 0.0,
            timestamp_period: 1.0,
            timestamp_mask: u64::MAX,
            timestamps_supported: false,
            pipeline_stats_supported: false,
        }
    }
}

impl GpuProfiler {
    /// Seeds the device-derived timestamp facts (period / valid-bits mask / support
    /// flags + the calibrated-timestamp availability and host domain) onto a fresh
    /// profiler — the renderer-init capability probe.
    pub fn with_facts(
        timestamp_period: f32,
        timestamp_mask: u64,
        timestamps_supported: bool,
        pipeline_stats_supported: bool,
        calibration_available: bool,
        host_domain: vk::TimeDomainEXT,
    ) -> Self {
        Self {
            timestamp_period,
            timestamp_mask,
            timestamps_supported,
            pipeline_stats_supported,
            calibration: GpuCalibration {
                available: calibration_available,
                host_domain,
                ..GpuCalibration::default()
            },
            ..Self::default()
        }
    }

    /// Re-samples the device and host clocks together and stores the offset that
    /// projects a GPU tick onto the CPU `CLOCK_MONOTONIC` axis. Cheap (no queue work);
    /// called once per frame while
    /// profiling, but only actually samples once a session and then ~once a second
    /// (every 64 frames) to track drift. A no-op when calibration is unavailable,
    /// leaving `correlated = false` (the own-axis fallback).
    pub fn calibrate(&mut self, device: &Device, frame_serial: u64) {
        if !self.calibration.available {
            return;
        }
        if self.calibration.correlated
            && frame_serial.wrapping_sub(self.calibration.last_calibrated_serial) < 64
        {
            return;
        }
        let Some((tick_raw, host_ns, max_dev)) =
            device.sample_calibrated_timestamps(self.calibration.host_domain)
        else {
            return;
        };
        self.calibration.device_to_host_ns_offset = device_to_host_offset(
            tick_raw,
            host_ns,
            self.timestamp_mask,
            self.timestamp_period,
        );
        self.calibration.max_deviation_ns = max_dev;
        self.calibration.correlated = true;
        self.calibration.last_calibrated_serial = frame_serial;
    }

    /// Allocates the per-frame timestamp pools (and the stats pools when supported).
    /// Idempotent — a no-op once `pools_ready`.
    ///
    /// # Errors
    ///
    /// Propagates a [`crate::Error::Vk`] from `vkCreateQueryPool`.
    pub fn allocate_pools(&mut self, device: &Device) -> crate::Result<()> {
        if self.pools_ready {
            return Ok(());
        }
        let raw = device.raw();
        for pool in &mut self.timestamp_pools {
            let info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::TIMESTAMP)
                .query_count(2 * MAX_PROFILED_SCOPES);
            // SAFETY: the ash seam. The create-info is valid; the pool is owned + freed
            // in `destroy_pools`.
            let created = checked(
                unsafe { raw.create_query_pool(&info, None) },
                "create_query_pool(timestamp)",
            )?;
            *pool = Some(created);
        }
        if self.pipeline_stats_supported {
            for pool in &mut self.stats_pools {
                let info = vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::PIPELINE_STATISTICS)
                    .query_count(MAX_PROFILED_SCOPES)
                    .pipeline_statistics(pipeline_stats_flags());
                // SAFETY: the ash seam, as above.
                let created = checked(
                    unsafe { raw.create_query_pool(&info, None) },
                    "create_query_pool(stats)",
                )?;
                *pool = Some(created);
            }
        }
        self.pools_ready = true;
        Ok(())
    }

    /// Destroys the query pools and clears the recorded scopes. Run under `wait_idle`
    /// (teardown) before the device drops.
    pub fn destroy_pools(&mut self, device: &Device) {
        let raw = device.raw();
        for pool in self
            .timestamp_pools
            .iter_mut()
            .chain(self.stats_pools.iter_mut())
        {
            if let Some(p) = pool.take() {
                // SAFETY: the ash seam. The pool was created here; no query is in flight
                // (teardown runs under `wait_idle`).
                unsafe { raw.destroy_query_pool(p, None) };
            }
        }
        for records in &mut self.recorded_scopes {
            records.clear();
        }
        self.pools_ready = false;
    }

    /// The timestamp pool for frame slot `slot`.
    pub fn timestamp_pool(&self, slot: usize) -> Option<vk::QueryPool> {
        self.timestamp_pools[slot]
    }

    /// The stats pool for frame slot `slot`.
    pub fn stats_pool(&self, slot: usize) -> Option<vk::QueryPool> {
        self.stats_pools[slot]
    }

    /// Sets the profiler mode, degrading a request the device cannot satisfy: no
    /// timestamps ⇒ off; no pipeline statistics ⇒ plain timestamps; pool-alloc failure
    /// ⇒ off. Clears the last read-back when turning off.
    pub fn set_mode(&mut self, device: &Device, mut mode: ProfilerMode) {
        if mode != ProfilerMode::Off && !self.timestamps_supported {
            mode = ProfilerMode::Off;
        }
        if mode == ProfilerMode::PipelineStats && !self.pipeline_stats_supported {
            mode = ProfilerMode::Timestamps;
        }
        if mode != ProfilerMode::Off && !self.pools_ready && self.allocate_pools(device).is_err() {
            mode = ProfilerMode::Off;
        }
        self.mode = mode;
        if mode == ProfilerMode::Off {
            self.last_timings.clear();
            self.last_gpu_total_ms = 0.0;
            self.calibration.correlated = false;
        }
    }

    /// Binds this frame's recorder: rebinds the slot's pool/records/cursor for a fresh
    /// frame. Returns an armed [`RgTimestamps`] (a `None` pool when the mode is `Off`),
    /// taking ownership of the slot's record vec (returned by [`GpuProfiler::stash_recorder`]).
    pub fn frame_recorder(&mut self, slot: usize) -> RgTimestamps {
        if self.mode == ProfilerMode::Off || !self.pools_ready {
            return RgTimestamps::default();
        }
        let stats_pool = if self.mode == ProfilerMode::PipelineStats {
            self.stats_pools[slot]
        } else {
            None
        };
        RgTimestamps {
            pool: self.timestamp_pools[slot],
            capacity: 2 * MAX_PROFILED_SCOPES,
            records: Vec::new(),
            next_slot: 0,
            open_scope: -1,
            depth: 0,
            stats_pool,
            stats_capacity: if stats_pool.is_some() {
                MAX_PROFILED_SCOPES
            } else {
                0
            },
            next_stats_slot: 0,
        }
    }

    /// Stashes a frame's recorded scopes into the slot, to be read back
    /// [`MAX_FRAMES_IN_FLIGHT`] frames later (after the slot's fence signals).
    pub fn stash_recorder(&mut self, slot: usize, recorder: RgTimestamps) {
        self.recorded_scopes[slot] = recorder.records;
    }

    /// Reads back slot's timestamp pool (its GPU work completed at the begin-frame fence
    /// wait, so this never blocks) into the last-timings + the EMA GPU frame time.
    ///
    /// Returns the smoothed EMA GPU frame time given the prior value, so the caller can
    /// fold it into the renderer's `gpu_frame_ms` without exposing the field here.
    pub fn readback(&mut self, device: &Device, slot: usize, prior_gpu_frame_ms: f32) -> f32 {
        let records = std::mem::take(&mut self.recorded_scopes[slot]);
        let Some(pool) = self.timestamp_pools[slot] else {
            self.recorded_scopes[slot] = records;
            return prior_gpu_frame_ms;
        };
        if records.is_empty() {
            return prior_gpu_frame_ms;
        }
        let scope_count = records.len();
        let query_count = 2 * scope_count;
        // One `[value, availability]` u64 pair per query (TYPE_64 | WITH_AVAILABILITY). The
        // element type is `[u64; 2]` so ash passes the query count (`query_count`) as the count
        // and `size_of::<[u64; 2]>()` (16 bytes) as the stride — a plain `u64` element would
        // mis-pass `2 * query_count` as the count and an 8-byte stride
        // (`VUID-vkGetQueryPoolResults-stride-08993` / `-dataSize-00817`). The flat decode then
        // reads it as `[u64]` (`raw[4*i .. 4*i+4]` per scope = begin value/avail, end value/avail).
        let mut pairs = vec![[0u64; 2]; query_count];
        let device_raw = device.raw();
        // SAFETY: the ash seam. `pool` holds `query_count` written queries; the result buffer
        // holds `query_count` 16-byte pairs (matching the WITH_AVAILABILITY stride).
        let r = unsafe {
            device_raw.get_query_pool_results::<[u64; 2]>(
                pool,
                0,
                &mut pairs,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WITH_AVAILABILITY,
            )
        };
        if let Err(code) = r
            && code != vk::Result::NOT_READY
        {
            self.recorded_scopes[slot] = records;
            return prior_gpu_frame_ms; // keep the last good read-back
        }
        // Flatten the `[value, availability]` pairs into the `[u64]` layout the decoders index.
        let raw: Vec<u64> = pairs.into_iter().flatten().collect();

        let stats = self.read_stats(device, slot, &records);

        let timings = decode_timings(
            &records,
            &raw,
            self.timestamp_mask,
            self.timestamp_period,
            &self.calibration,
            stats.as_deref(),
        );
        let total_ms = frame_span_ms(&records, &raw, self.timestamp_mask, self.timestamp_period);
        self.last_timings = timings;
        self.last_gpu_total_ms = total_ms;
        // A frame this long approaches platform GPU-watchdog territory (device-loss risk);
        // name the heaviest passes so the offender is identifiable from the log alone.
        if total_ms > 500.0 {
            let mut heaviest: Vec<(&str, f32)> = self
                .last_timings
                .iter()
                .map(|t| (t.name.as_str(), t.gpu_ms))
                .collect();
            heaviest.sort_by(|a, b| b.1.total_cmp(&a.1));
            heaviest.truncate(5);
            tracing::warn!(total_ms, ?heaviest, "GPU frame ran long");
        }
        if prior_gpu_frame_ms == 0.0 {
            total_ms
        } else {
            prior_gpu_frame_ms * 0.9 + total_ms * 0.1
        }
    }

    /// Reads the pipeline-stats pool when this frame recorded any stats slot. Returns
    /// the raw stats words (`MAX_PROFILED_SCOPES * (PIPELINE_STATS_COUNT + 1)`), or
    /// `None` when no stats were recorded (a timestamps-only frame must not read an
    /// unreset pool — a validation error).
    fn read_stats(
        &self,
        device: &Device,
        slot: usize,
        records: &[ScopeRecord],
    ) -> Option<Vec<u64>> {
        let pool = self.stats_pools[slot]?;
        if !records.iter().any(|r| r.stats_slot >= 0) {
            return None;
        }
        // One `[stat_0 .. stat_{N-1}, availability]` record per query, the element type so ash
        // passes the query count (`MAX_PROFILED_SCOPES`) as the count and the record's byte size
        // as the stride — a flat `u64` element would mis-pass the element count as the query count
        // and an 8-byte stride (`VUID-vkGetQueryPoolResults-stride-08993` / `-dataSize-00817`).
        const STRIDE: usize = PIPELINE_STATS_COUNT + 1;
        let mut records_raw = vec![[0u64; STRIDE]; MAX_PROFILED_SCOPES as usize];
        // SAFETY: the ash seam. `pool` was reset + has stats queries written this frame;
        // the buffer holds one (stats + availability) record per slot.
        let _ = unsafe {
            device.raw().get_query_pool_results::<[u64; STRIDE]>(
                pool,
                0,
                &mut records_raw,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WITH_AVAILABILITY,
            )
        };
        // Flatten into the `[u64]` layout `decode_timings` indexes (`slot * STRIDE + k`).
        Some(records_raw.into_iter().flatten().collect())
    }
}

/// The earliest-begin → latest-end frame span (ms) across all scopes with available
/// timestamps. NOT a sum — sibling/async scopes overlap and a parent brackets its
/// children, so the nested last-record-end is wrong.
fn frame_span_ms(records: &[ScopeRecord], raw: &[u64], mask: u64, period: f32) -> f32 {
    let mut span_begin = 0u64;
    let mut span_end = 0u64;
    let mut valid = false;
    for i in 0..records.len() {
        if raw[4 * i + 1] != 0 && raw[4 * i + 3] != 0 {
            let b = raw[4 * i] & mask;
            let e = raw[4 * i + 2] & mask;
            if !valid {
                span_begin = b;
                span_end = e;
                valid = true;
            }
            span_begin = span_begin.min(b);
            span_end = span_end.max(e);
        }
    }
    if valid && span_end >= span_begin {
        ((span_end - span_begin) as f64 * period as f64 / 1.0e6) as f32
    } else {
        0.0
    }
}

/// The additive ns offset projecting a device tick onto the host clock:
/// `hostNs - deviceNs`, where `deviceNs = (tick & mask) * period`. Pure arithmetic so
/// the calibration math is unit-testable without a device.
fn device_to_host_offset(tick_raw: u64, host_ns: u64, mask: u64, period: f32) -> i64 {
    let device_ns = (tick_raw & mask) as f64 * period as f64;
    (host_ns as f64 - device_ns) as i64
}

/// Decodes the raw timestamp + stats words into [`PassTiming`]s, projecting onto the
/// CPU clock axis when correlated, else onto a frame-relative axis (the own-axis
/// fallback). Pure arithmetic — the unit tests drive it with synthetic query words.
fn decode_timings(
    records: &[ScopeRecord],
    raw: &[u64],
    mask: u64,
    period: f32,
    calibration: &GpuCalibration,
    stats: Option<&[u64]>,
) -> Vec<PassTiming> {
    // The frame-relative origin is the earliest available begin (own-axis fallback).
    let mut span_begin = 0u64;
    let mut span_valid = false;
    for i in 0..records.len() {
        if raw[4 * i + 1] != 0 && raw[4 * i + 3] != 0 {
            let b = raw[4 * i] & mask;
            if !span_valid {
                span_begin = b;
                span_valid = true;
            }
            span_begin = span_begin.min(b);
        }
    }

    let mut out = Vec::with_capacity(records.len());
    for (i, rec) in records.iter().enumerate() {
        let mut t = PassTiming {
            name: rec.name.clone(),
            parent_index: rec.parent_index,
            depth: rec.depth,
            ..Default::default()
        };
        if raw[4 * i + 1] != 0 && raw[4 * i + 3] != 0 {
            let b = raw[4 * i] & mask;
            let e = raw[4 * i + 2] & mask;
            let ticks = e.saturating_sub(b);
            t.gpu_ms = (ticks as f64 * period as f64 / 1.0e6) as f32;
            if calibration.correlated {
                let offset = calibration.device_to_host_ns_offset as f64;
                t.start_ns = (b as f64 * period as f64 + offset) as u64;
                t.end_ns = (e as f64 * period as f64 + offset) as u64;
            } else {
                if span_valid && b >= span_begin {
                    t.start_ns = ((b - span_begin) as f64 * period as f64) as u64;
                }
                if span_valid && e >= span_begin {
                    t.end_ns = ((e - span_begin) as f64 * period as f64) as u64;
                }
            }
        }
        if let Some(stats) = stats
            && rec.stats_slot >= 0
        {
            let base = rec.stats_slot as usize * (PIPELINE_STATS_COUNT + 1);
            if stats[base + PIPELINE_STATS_COUNT] != 0 {
                t.has_stats = true;
                t.stats = PipelineStats {
                    input_vertices: stats[base],
                    vertex_invocations: stats[base + 1],
                    clipping_invocations: stats[base + 2],
                    clipping_primitives: stats[base + 3],
                    fragment_invocations: stats[base + 4],
                    compute_invocations: stats[base + 5],
                    pixels: rec.pixels,
                };
            }
        }
        out.push(t);
    }
    out
}

#[cfg(test)]
mod tests;
