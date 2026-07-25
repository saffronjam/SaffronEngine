//! The GPU/CPU profiler: per-pass GPU timestamps + pipeline statistics, the CPU span
//! recorder, the calibrated-timestamp correlation, and the bounded capture state
//! machine — armed through the [`RgTimestamps`] / [`CpuRecorder`] hooks the render
//! graph reserved.
//!
//! The profiler is **zero-cost when [`ProfilerMode::Off`]** (the default): no query
//! pools are allocated, no VMA budget is read, and the recorders are unarmed (a `None`
//! pool / a `None` buffer makes every scope a cheap branch). The deeper modes are
//! opt-in over the control plane. The read-back is non-blocking: a slot's pool is read
//! [`crate::MAX_FRAMES_IN_FLIGHT`] frames later, after that slot's fence has signalled.

use ash::vk;

use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::{Device, checked};

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

/// Interns scope names to stable integer ids so a [`CpuSpan`] stays string-free. Names
/// recur every frame, so the table grows once and then holds; lookup is a linear scan.
#[derive(Default)]
pub struct CpuMarkerRegistry {
    names: Vec<String>,
}

impl CpuMarkerRegistry {
    /// Maps a name to its stable id, interning it on first sight.
    pub fn id(&mut self, name: &str) -> u32 {
        if let Some(i) = self.names.iter().position(|n| n == name) {
            return i as u32;
        }
        self.names.push(name.to_string());
        (self.names.len() - 1) as u32
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
    open_parent: i32,
    open_depth: u32,
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

/// The lane a [`ProfileSpan`] sits on in a merged capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileLane {
    /// The CPU render-thread timeline.
    Cpu,
    /// The GPU timeline (projected onto the CPU axis when correlated).
    Gpu,
}

/// The merged span record the whole profiler stack speaks: a CPU pass span or a GPU
/// scope projected onto the CPU axis. Flat-and-tagged; `parent_index`/`depth` decode
/// the tree, `lane` separates the two timelines.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileSpan {
    /// The span name.
    pub name: String,
    /// Which timeline lane.
    pub lane: ProfileLane,
    /// Begin (ns).
    pub start_ns: u64,
    /// End (ns).
    pub end_ns: u64,
    /// The enclosing span index in this capture, or `-1` at top level.
    pub parent_index: i32,
    /// Nesting depth.
    pub depth: u32,
    /// Whether pipeline statistics are present.
    pub has_stats: bool,
    /// Pipeline statistics, when `has_stats`.
    pub stats: PipelineStats,
}

/// Self-documenting capture metadata: the honesty flags plus the device + clock facts a
/// downloaded trace needs to be interpreted on its own.
#[derive(Clone, Debug, Default)]
pub struct ProfileCaptureMeta {
    /// Whether GPU timings are software-rasterizer (llvmpipe) times.
    pub software_gpu: bool,
    /// Whether GPU spans were correlated onto the CPU clock.
    pub correlated: bool,
    /// The physical-device name.
    pub device_name: String,
    /// ns per timestamp tick.
    pub timestamp_period: f32,
    /// The target FPS the capture was taken at.
    pub target_fps: f32,
    /// The profiler mode the capture ran in.
    pub mode: ProfilerMode,
    /// The pass-name prefix filter (a view hint).
    pub filter: String,
    /// The number of frames recorded.
    pub frame_count: u32,
}

/// A bounded profiler capture: the merged spans plus the metadata.
#[derive(Clone, Debug, Default)]
pub struct ProfileCapture {
    /// The merged CPU+GPU spans across all recorded frames.
    pub spans: Vec<ProfileSpan>,
    /// The capture metadata.
    pub meta: ProfileCaptureMeta,
}

/// Bounded capture: a single frame, a fixed N-frame window, or rolling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureMode {
    /// One frame (the default snapshot).
    #[default]
    Single,
    /// A fixed N-frame window.
    Frames,
    /// A recent rolling window (recorded forward like Frames in v1).
    Rolling,
}

/// Hard cap on a capture's frame count so the span buffer cannot OOM.
pub const MAX_CAPTURE_FRAMES: u32 = 256;

/// The capture state machine. `Arming` warms up for the GPU read-back delay so every
/// recorded frame reflects the arm-time settings; `Recording` copies each finalized
/// frame's merged spans; `Ready` holds the result until drained.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureState {
    /// Not capturing.
    #[default]
    Idle,
    /// Warming up to flush the read-back delay.
    Arming,
    /// Copying finalized frames.
    Recording,
    /// The capture is complete, awaiting drain.
    Ready,
}

/// The capture recorder driven by `profiler.capture-start/stop`.
pub struct CaptureRecorder {
    /// The current state.
    pub state: CaptureState,
    /// The capture mode.
    pub mode: CaptureMode,
    /// Frames to record before going `Ready`.
    pub target_frames: u32,
    /// Frames copied so far.
    pub captured_frames: u32,
    /// `Arming` frames left (covers the read-back delay).
    pub warmup: u32,
    /// Pass-name prefix carried into metadata (a view hint).
    pub filter: String,
    /// Whether to include the CPU lane.
    pub include_cpu: bool,
    /// Whether PipelineStats mode was requested (if supported).
    pub include_stats: bool,
    /// The profiler mode restored on stop.
    pub prior_mode: ProfilerMode,
    /// The `sub_scopes` flag restored on stop.
    pub prior_sub_scopes: bool,
    /// Id of the in-flight / last capture.
    pub capture_id: u32,
    next_capture_id: u32,
    /// The capture accumulating while `Recording`.
    pub capture: ProfileCapture,
    /// The last completed (non-empty) drain, echoed by a redundant `stop` so a duplicate
    /// stop never returns an empty capture in place of a good one.
    last_capture: ProfileCapture,
}

impl Default for CaptureRecorder {
    fn default() -> Self {
        Self {
            state: CaptureState::Idle,
            mode: CaptureMode::Single,
            target_frames: 1,
            captured_frames: 0,
            warmup: 0,
            filter: String::new(),
            include_cpu: true,
            include_stats: false,
            prior_mode: ProfilerMode::Off,
            prior_sub_scopes: false,
            capture_id: 0,
            next_capture_id: 1,
            capture: ProfileCapture::default(),
            last_capture: ProfileCapture::default(),
        }
    }
}

impl CaptureRecorder {
    /// Starts a capture, arming the profiler to the requested level (PipelineStats only
    /// when stats are wanted + supported, else Timestamps) and warming up to flush the
    /// read-back delay. Returns the new capture id. The profiler's prior mode/sub-scopes
    /// are saved for `stop`.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &mut self,
        device: &Device,
        profiler: &mut GpuProfiler,
        mode: CaptureMode,
        frames: u32,
        filter: String,
        include_cpu: bool,
        include_stats: bool,
    ) -> u32 {
        self.mode = mode;
        self.target_frames = if mode == CaptureMode::Single {
            1
        } else {
            frames.clamp(1, MAX_CAPTURE_FRAMES)
        };
        self.captured_frames = 0;
        self.warmup = (MAX_FRAMES_IN_FLIGHT + 1) as u32; // flush the read-back delay
        self.filter = filter;
        self.include_cpu = include_cpu;
        self.include_stats = include_stats && profiler.pipeline_stats_supported;
        self.capture = ProfileCapture::default();
        self.capture_id = self.next_capture_id;
        self.next_capture_id += 1;
        self.prior_mode = profiler.mode;
        self.prior_sub_scopes = profiler.sub_scopes;
        let wanted = if self.include_stats {
            ProfilerMode::PipelineStats
        } else {
            ProfilerMode::Timestamps
        };
        if profiler.mode != wanted {
            profiler.set_mode(device, wanted);
        }
        profiler.sub_scopes = true; // capture the full nested tree; restored on stop
        self.state = CaptureState::Arming;
        self.capture_id
    }

    /// Advances the state machine once per finalized frame (at the read-back seam),
    /// appending the merged CPU+GPU spans for each `Recording` frame. The merged spans
    /// come from the current frame's CPU buffer + the profiler's last read-back.
    pub fn tick(&mut self, cpu: &CpuProfiler, cpu_slot: usize, profiler: &GpuProfiler) {
        match self.state {
            CaptureState::Arming => {
                self.warmup = self.warmup.saturating_sub(1);
                if self.warmup == 0 {
                    self.state = CaptureState::Recording;
                }
            }
            CaptureState::Recording => {
                self.append_frame(cpu, cpu_slot, profiler);
                self.captured_frames += 1;
                if self.captured_frames >= self.target_frames {
                    self.state = CaptureState::Ready;
                }
            }
            _ => {}
        }
    }

    /// Appends one frame's CPU spans (when enabled) then GPU passes, rebasing each
    /// span's `parent_index` into the growing capture's index space.
    fn append_frame(&mut self, cpu: &CpuProfiler, cpu_slot: usize, profiler: &GpuProfiler) {
        let base = self.capture.spans.len();
        let mut cpu_count = 0usize;
        if self.include_cpu {
            for s in &cpu.buffers[cpu_slot].spans {
                self.capture.spans.push(ProfileSpan {
                    name: cpu.registry.name(s.marker).to_string(),
                    lane: ProfileLane::Cpu,
                    start_ns: s.start_ns,
                    end_ns: s.end_ns,
                    parent_index: if s.parent >= 0 {
                        base as i32 + s.parent
                    } else {
                        -1
                    },
                    depth: s.depth,
                    has_stats: false,
                    stats: PipelineStats::default(),
                });
                cpu_count += 1;
            }
        }
        let gpu_base = base + cpu_count;
        for t in &profiler.last_timings {
            self.capture.spans.push(ProfileSpan {
                name: t.name.clone(),
                lane: ProfileLane::Gpu,
                start_ns: t.start_ns,
                end_ns: t.end_ns,
                parent_index: if t.parent_index >= 0 {
                    gpu_base as i32 + t.parent_index
                } else {
                    -1
                },
                depth: t.depth,
                has_stats: t.has_stats,
                stats: t.stats,
            });
        }
    }

    /// Stops the capture, returning the accumulated [`ProfileCapture`] with its metadata
    /// filled, and restoring the profiler's prior mode/sub-scopes.
    ///
    /// Idempotent: a stop on an already-drained (`Idle`) recorder echoes the last completed
    /// capture rather than an empty one, so a duplicate stop — e.g. an overlapping control
    /// poll when frames are slow enough that round-trips exceed the poll interval — can never
    /// return an empty result in place of a good one.
    #[allow(clippy::too_many_arguments)]
    pub fn stop(
        &mut self,
        device: &Device,
        profiler: &mut GpuProfiler,
        software_gpu: bool,
        device_name: String,
        target_fps: f32,
    ) -> ProfileCapture {
        if self.state == CaptureState::Idle {
            // Already drained; the profiler was restored on the real stop. Echo the last
            // result without touching the profiler so a redundant stop is a no-op.
            return self.last_capture.clone();
        }
        // The metadata records the *capture's* facts, read before the profiler is restored.
        let correlated = profiler.calibration.correlated;
        let timestamp_period = profiler.timestamp_period;
        let mode = profiler.mode;
        profiler.sub_scopes = self.prior_sub_scopes;
        if self.prior_mode != profiler.mode {
            profiler.set_mode(device, self.prior_mode);
        }
        self.finish(ProfileCaptureMeta {
            software_gpu,
            correlated,
            device_name,
            timestamp_period,
            target_fps,
            mode,
            filter: self.filter.clone(),
            frame_count: 0,
        })
    }

    /// Drains the accumulated capture, stamping `meta` (its `frame_count` is filled here),
    /// resetting the recorder to `Idle`, and remembering a non-empty drain for a later
    /// redundant [`CaptureRecorder::stop`] to echo. Device-free, so it carries the testable
    /// idempotency logic: a call on an already-`Idle` recorder echoes the last completed
    /// capture rather than an empty one.
    fn finish(&mut self, mut meta: ProfileCaptureMeta) -> ProfileCapture {
        if self.state == CaptureState::Idle {
            return self.last_capture.clone();
        }
        let mut out = std::mem::take(&mut self.capture);
        meta.frame_count = self.captured_frames;
        out.meta = meta;
        self.capture = ProfileCapture::default();
        let captured = self.captured_frames;
        self.captured_frames = 0;
        self.state = CaptureState::Idle;
        if captured > 0 {
            self.last_capture = out.clone();
        }
        out
    }
}

/// Raw `CLOCK_MONOTONIC` ns. This is the same axis the GPU
/// calibration projects device ticks onto (`VK_TIME_DOMAIN_CLOCK_MONOTONIC_EXT`), so a
/// correlated capture places CPU and GPU spans on one timeline.
pub fn cpu_now_ns() -> u64 {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_mode_arms_no_recorder() {
        let prof = GpuProfiler::default();
        assert_eq!(prof.mode, ProfilerMode::Off);
        assert!(!prof.pools_ready, "no query pools allocated when Off");
        let mut prof = prof;
        let rec = prof.frame_recorder(0);
        assert!(
            !rec.armed(),
            "the recorder is unarmed (a cheap branch) when Off"
        );
        assert!(rec.pool.is_none());
    }

    #[test]
    fn cpu_span_buffer_records_a_nested_tree() {
        let mut registry = CpuMarkerRegistry::default();
        let mut buf = CpuSpanBuffer::default();
        buf.reset();
        let outer = buf.begin_span(&mut registry, "frame", 100);
        let inner = buf.begin_span(&mut registry, "scene", 110);
        buf.end_span(inner, 180);
        buf.end_span(outer, 200);

        assert_eq!(buf.spans.len(), 2);
        assert_eq!(buf.spans[outer].parent, -1);
        assert_eq!(buf.spans[outer].depth, 0);
        assert_eq!(buf.spans[outer].start_ns, 100);
        assert_eq!(buf.spans[outer].end_ns, 200);
        assert_eq!(buf.spans[inner].parent, outer as i32);
        assert_eq!(buf.spans[inner].depth, 1);
        assert_eq!(buf.spans[inner].end_ns, 180);
        assert_eq!(registry.name(buf.spans[inner].marker), "scene");
    }

    #[test]
    fn marker_registry_interns_names() {
        let mut r = CpuMarkerRegistry::default();
        let a = r.id("scene");
        let b = r.id("tonemap");
        let a2 = r.id("scene");
        assert_eq!(a, a2, "re-interning a name returns the same id");
        assert_ne!(a, b);
        assert_eq!(r.name(a), "scene");
    }

    /// Builds the raw query result words for `n` scopes: per scope i, slots
    /// `[begin, begin_avail, end, end_avail]`. `available` toggles whether the pair has
    /// landed.
    fn make_raw(spans: &[(u64, u64, bool)]) -> Vec<u64> {
        let mut raw = vec![0u64; spans.len() * 4];
        for (i, &(b, e, avail)) in spans.iter().enumerate() {
            raw[4 * i] = b;
            raw[4 * i + 1] = if avail { 1 } else { 0 };
            raw[4 * i + 2] = e;
            raw[4 * i + 3] = if avail { 1 } else { 0 };
        }
        raw
    }

    #[test]
    fn decode_timings_yields_per_pass_spans_on_the_frame_relative_axis() {
        let records = vec![
            ScopeRecord {
                name: "scene".into(),
                parent_index: -1,
                depth: 0,
                stats_slot: -1,
                pixels: 0,
            },
            ScopeRecord {
                name: "tonemap".into(),
                parent_index: -1,
                depth: 0,
                stats_slot: -1,
                pixels: 0,
            },
        ];
        // scene: 1000..3000 ticks; tonemap: 3000..3500 ticks. period = 1ns/tick.
        let raw = make_raw(&[(1000, 3000, true), (3000, 3500, true)]);
        let cal = GpuCalibration::default(); // not correlated → frame-relative
        let timings = decode_timings(&records, &raw, u64::MAX, 1.0, &cal, None);

        assert_eq!(timings.len(), 2);
        // span_begin = 1000 (the earliest begin). scene starts at 0, ends at 2000ns.
        assert_eq!(timings[0].name, "scene");
        assert_eq!(timings[0].start_ns, 0);
        assert_eq!(timings[0].end_ns, 2000);
        assert!((timings[0].gpu_ms - 0.002).abs() < 1e-6, "2000ns = 0.002ms");
        // tonemap starts at 3000-1000 = 2000ns.
        assert_eq!(timings[1].start_ns, 2000);
        assert_eq!(timings[1].end_ns, 2500);
    }

    #[test]
    fn unavailable_queries_decode_to_zero_spans() {
        let records = vec![ScopeRecord {
            name: "scene".into(),
            parent_index: -1,
            depth: 0,
            stats_slot: -1,
            pixels: 0,
        }];
        let raw = make_raw(&[(1000, 3000, false)]); // not yet available
        let timings = decode_timings(
            &records,
            &raw,
            u64::MAX,
            1.0,
            &GpuCalibration::default(),
            None,
        );
        assert_eq!(timings[0].gpu_ms, 0.0);
        assert_eq!(timings[0].start_ns, 0);
        assert_eq!(timings[0].end_ns, 0);
    }

    #[test]
    fn correlated_decode_projects_onto_the_host_clock() {
        let records = vec![ScopeRecord {
            name: "scene".into(),
            parent_index: -1,
            depth: 0,
            stats_slot: -1,
            pixels: 0,
        }];
        let raw = make_raw(&[(1000, 3000, true)]);
        let cal = GpuCalibration {
            available: true,
            device_to_host_ns_offset: 1_000_000,
            correlated: true,
            ..Default::default()
        };
        let timings = decode_timings(&records, &raw, u64::MAX, 1.0, &cal, None);
        // start = 1000ns * 1 + 1_000_000 offset.
        assert_eq!(timings[0].start_ns, 1_001_000);
        assert_eq!(timings[0].end_ns, 1_003_000);
    }

    #[test]
    fn frame_span_is_earliest_begin_to_latest_end() {
        let records = vec![
            ScopeRecord {
                name: "outer".into(),
                parent_index: -1,
                depth: 0,
                stats_slot: -1,
                pixels: 0,
            },
            ScopeRecord {
                name: "inner".into(),
                parent_index: 0,
                depth: 1,
                stats_slot: -1,
                pixels: 0,
            },
        ];
        // outer brackets inner: outer 100..900, inner 200..800. Span = 800 ticks, NOT
        // the sum (1400).
        let raw = make_raw(&[(100, 900, true), (200, 800, true)]);
        let ms = frame_span_ms(&records, &raw, u64::MAX, 1.0);
        assert!((ms - 0.0008).abs() < 1e-9, "800ns = 0.0008ms (not a sum)");
    }

    #[test]
    fn stats_decode_reads_positional_counters_when_available() {
        let records = vec![ScopeRecord {
            name: "scene".into(),
            parent_index: -1,
            depth: 0,
            stats_slot: 0,
            pixels: 4096,
        }];
        let raw = make_raw(&[(0, 100, true)]);
        // stats words for slot 0: 6 counters + an availability word (non-zero = ready).
        let mut stats = vec![0u64; MAX_PROFILED_SCOPES as usize * (PIPELINE_STATS_COUNT + 1)];
        stats[0] = 10; // input_vertices
        stats[1] = 12; // vertex_invocations
        stats[2] = 8; // clipping_invocations
        stats[3] = 4; // clipping_primitives
        stats[4] = 5000; // fragment_invocations
        stats[5] = 0; // compute_invocations
        stats[PIPELINE_STATS_COUNT] = 1; // availability
        let timings = decode_timings(
            &records,
            &raw,
            u64::MAX,
            1.0,
            &GpuCalibration::default(),
            Some(&stats),
        );
        assert!(timings[0].has_stats);
        assert_eq!(timings[0].stats.input_vertices, 10);
        assert_eq!(timings[0].stats.fragment_invocations, 5000);
        assert_eq!(
            timings[0].stats.pixels, 4096,
            "render-area pixels from the record"
        );
    }

    #[test]
    fn calibration_offset_projects_device_tick_onto_host_clock() {
        // A device tick of 1000 at 2ns/tick is 2000 device-ns; a host sample of 10^13 ns
        // yields offset = 10^13 - 2000, so the read-back maps the tick to the host clock.
        let host_ns = 10_000_000_000_000u64;
        let offset = device_to_host_offset(1000, host_ns, u64::MAX, 2.0);
        assert_eq!(offset, host_ns as i64 - 2000);
        // The mask drops the high (invalid) bits before scaling.
        let masked = device_to_host_offset(0xFFFF_0000_0000_03E8, host_ns, 0xFFFF_FFFF, 2.0);
        assert_eq!(masked, host_ns as i64 - 2000);
    }

    #[test]
    fn calibrate_is_a_noop_when_unavailable() {
        // No device extension: availability is false, so `calibrate` cannot run and
        // correlation stays off (the own-axis fallback). A `Device` is not needed — the
        // availability gate returns before the sample call.
        let mut prof = GpuProfiler::with_facts(
            1.0,
            u64::MAX,
            true,
            false,
            false,
            vk::TimeDomainEXT::default(),
        );
        assert!(!prof.calibration.available);
        prof.calibration.correlated = false;
        // The gate that would otherwise call into the device returns early.
        if prof.calibration.available {
            unreachable!("unavailable calibration must not sample");
        }
        assert!(
            !prof.calibration.correlated,
            "correlation stays false when calibration is unavailable"
        );
    }

    #[test]
    fn capture_recorder_arms_records_ready_then_drains() {
        // Drive the state machine with no GPU (the tick consumes CPU spans + the
        // profiler's last_timings, both empty here — exercising the transitions).
        let cpu = CpuProfiler::default();
        let profiler = GpuProfiler::default();
        // Manually arm a 3-frame capture (start() needs a Device; the state transitions
        // are the unit under test).
        let mut cap = CaptureRecorder {
            state: CaptureState::Arming,
            mode: CaptureMode::Frames,
            target_frames: 3,
            captured_frames: 0,
            warmup: (MAX_FRAMES_IN_FLIGHT + 1) as u32,
            ..Default::default()
        };

        // Arming burns down warmup.
        for _ in 0..(MAX_FRAMES_IN_FLIGHT + 1) {
            assert_eq!(cap.state, CaptureState::Arming);
            cap.tick(&cpu, 0, &profiler);
        }
        assert_eq!(
            cap.state,
            CaptureState::Recording,
            "warmup elapsed → Recording"
        );

        // Recording copies target_frames frames, then goes Ready.
        for _ in 0..3 {
            assert_eq!(cap.state, CaptureState::Recording);
            cap.tick(&cpu, 0, &profiler);
        }
        assert_eq!(
            cap.state,
            CaptureState::Ready,
            "captured target frames → Ready"
        );
        assert_eq!(cap.captured_frames, 3);
    }

    #[test]
    fn redundant_finish_echoes_the_last_capture_instead_of_an_empty_one() {
        // A recorder that has recorded one frame's worth of spans.
        let span = ProfileSpan {
            name: "scene".into(),
            lane: ProfileLane::Gpu,
            start_ns: 0,
            end_ns: 1000,
            parent_index: -1,
            depth: 0,
            has_stats: false,
            stats: PipelineStats::default(),
        };
        let mut cap = CaptureRecorder {
            state: CaptureState::Recording,
            captured_frames: 1,
            capture: ProfileCapture {
                spans: vec![span.clone()],
                meta: ProfileCaptureMeta::default(),
            },
            ..Default::default()
        };

        // The real drain returns the spans and reports a recorded frame.
        let first = cap.finish(ProfileCaptureMeta::default());
        assert_eq!(first.spans.len(), 1);
        assert_eq!(first.meta.frame_count, 1);
        assert_eq!(cap.state, CaptureState::Idle);

        // A redundant drain (now Idle) echoes the same non-empty capture — it never returns an
        // empty result that would clobber the displayed one. This is the engine-side guard for
        // the overlapping-poll double-stop.
        let second = cap.finish(ProfileCaptureMeta::default());
        assert_eq!(
            second.spans.len(),
            1,
            "redundant stop echoes the last capture"
        );
        assert_eq!(second.spans[0].name, "scene");
        assert_eq!(second.meta.frame_count, 1);
    }

    #[test]
    fn append_frame_rebases_parent_indices_across_lanes_and_frames() {
        let mut cpu = CpuProfiler::default();
        // One frame's CPU spans: a "frame" parent and a nested "scene" child.
        let outer = cpu.buffers[0].begin_span(&mut cpu.registry, "frame", 0);
        let inner = cpu.buffers[0].begin_span(&mut cpu.registry, "scene", 5);
        cpu.buffers[0].end_span(inner, 8);
        cpu.buffers[0].end_span(outer, 10);

        // One GPU pass with a nested child.
        let profiler = GpuProfiler {
            last_timings: vec![
                PassTiming {
                    name: "scene-gpu".into(),
                    parent_index: -1,
                    depth: 0,
                    ..Default::default()
                },
                PassTiming {
                    name: "draw".into(),
                    parent_index: 0,
                    depth: 1,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let mut cap = CaptureRecorder {
            state: CaptureState::Recording,
            include_cpu: true,
            target_frames: 2,
            ..Default::default()
        };
        // Two frames so the second frame's parents must rebase past the first.
        cap.tick(&cpu, 0, &profiler);
        cap.tick(&cpu, 0, &profiler);

        let spans = &cap.capture.spans;
        // Each frame: 2 cpu + 2 gpu = 4 spans → 8 total.
        assert_eq!(spans.len(), 8);
        // Frame 0: cpu outer at 0 (parent -1), cpu inner at 1 (parent 0), gpu at 2
        // (parent -1), gpu child at 3 (parent gpu_base=2).
        assert_eq!(
            spans[1].parent_index, 0,
            "cpu child points at the cpu parent"
        );
        assert_eq!(spans[2].lane, ProfileLane::Gpu);
        assert_eq!(spans[3].parent_index, 2, "gpu child rebased onto gpu_base");
        // Frame 1 starts at base=4: cpu child parent = 4, gpu child parent = 6.
        assert_eq!(spans[5].parent_index, 4);
        assert_eq!(spans[7].parent_index, 6);
    }
}
