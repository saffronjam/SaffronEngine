//! Mesh, texture, SDF, and LUT uploads: the staging-copy sequences that produce the
//! [`Arc`]`<`[`crate::GpuMesh`]`>` / [`Arc`]`<`[`crate::GpuTexture`]`>` the asset layer and
//! scene draw consume.
//!
//! The graphics queue is externally synchronized, so it lives behind [`GpuQueue`] (an
//! `Arc<Mutex<vk::Queue>>`) and a one-off submit takes the lock for the submit only — the
//! fence wait is outside it, so a long upload does not stall a sibling submit. A command
//! pool is not thread-safe, so each [`Uploader`] owns its own; a worker thread builds its
//! own uploader with a clone of the same queue.

mod accel;
mod bake;
mod bake_pipelines;
mod barriers;
mod hierarchy;
mod lut;
mod mesh;
mod sdf;
mod staging;
mod texture;

#[cfg(test)]
mod fixtures;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ash::vk;
use saffron_geometry::Sdf;

use crate::resources::DeviceResources;
use crate::{Device, Result, checked};
use accel::accel_timestamp_pool;
use bake_pipelines::BakePipelines;

pub use texture::TextureMipLevel;

#[cfg(test)]
pub(crate) use hierarchy::hierarchy_for_upload;

/// A per-mesh SDF bake request handed to [`Uploader::upload_mesh`]: the asset's
/// `resolution_scale` (densifies/coarsens the fine grid) and an optional sidecar cache
/// directory. A `None` request (the gizmo/preview meshes) bakes no field. The bake itself
/// is a GPU jump-flood at upload time, cached to `<cache_dir>/<meshHash>.sdf`.
#[derive(Clone, Debug, Default)]
pub struct SdfBake {
    /// Voxel-density multiplier for the fine grid (default `1.0`, longest axis capped).
    pub resolution_scale: f32,
    /// The `assets/cache` directory the baked field is read from / written to (a content
    /// hash of the mesh keys it). `None` bakes every time (no sidecar).
    pub cache_dir: Option<PathBuf>,
}

/// Where a mesh's signed distance fields come from at upload.
///
/// A mesh either bakes from its own triangles on device (imported assets — the sidecar
/// cache keys on content), carries fields something else already derived (a plant family's
/// cooked field, from the aggregate occupancy in family space — its triangle stream is
/// prototype-local, so a triangle bake would be wrong-space), or has none.
#[derive(Clone, Copy)]
pub enum SdfSource<'a> {
    /// No fields; the mesh casts no distance-field occlusion.
    None,
    /// GPU jump-flood bake from the uploaded triangles, sidecar-cached.
    Bake(&'a SdfBake),
    /// Pre-derived fields, uploaded as-is.
    Cooked(&'a [Sdf]),
}

/// The externally-synchronized graphics queue, shared behind a mutex.
///
/// The frame loop's submit/present and the worker thread's upload submits all take this
/// lock. Cloning the `Arc` hands a second thread the same queue under the same lock.
#[derive(Clone)]
pub struct GpuQueue {
    inner: Arc<Mutex<vk::Queue>>,
}

/// How many times a diagnostic query tries for the queue lock before giving up on it.
const QUEUE_LOCK_ATTEMPTS: u32 = 20;

/// How long a diagnostic query waits between attempts at the queue lock.
const QUEUE_LOCK_BACKOFF: std::time::Duration = std::time::Duration::from_millis(10);

// SAFETY: a `vk::Queue` is a raw handle; the `Mutex` provides the external
// synchronization Vulkan requires for queue submission.
unsafe impl Send for GpuQueue {}
// SAFETY: as above — every access goes through the `Mutex`.
unsafe impl Sync for GpuQueue {}

impl GpuQueue {
    /// Wraps the device's graphics queue for shared, externally-synchronized use.
    pub(crate) fn new(queue: vk::Queue) -> Self {
        Self {
            inner: Arc::new(Mutex::new(queue)),
        }
    }

    /// Submits `submits` on the queue under the lock, signaling `fence`. The lock is
    /// held only for the submit call.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if `vkQueueSubmit2` fails.
    pub(crate) fn submit2(
        &self,
        raw: &ash::Device,
        submits: &[vk::SubmitInfo2<'_>],
        fence: vk::Fence,
        context: &'static str,
    ) -> Result<()> {
        let queue = self.inner.lock().expect("gpu queue mutex");
        // SAFETY: the ash seam. The queue is externally synchronized by the mutex guard
        // held across the call; the submit-infos + fence are valid for the call.
        checked(
            unsafe { raw.queue_submit2(*queue, submits, fence) },
            context,
        )
    }

    /// Logs the queue's last-reached diagnostic checkpoints after a device loss, so the log
    /// names the wedged submission. A no-op when the extension is absent.
    ///
    /// Takes the queue's external-synchronization lock without ever blocking on it: this also runs
    /// on the hang-watchdog thread, and `vkDeviceWaitIdle` holds that lock for as long as the GPU
    /// is stuck — the one situation in which the report must still come out.
    pub(crate) fn log_device_loss_checkpoints(
        &self,
        checkpoints: Option<&crate::checkpoints::Checkpoints>,
    ) {
        let Some(checkpoints) = checkpoints else {
            return;
        };
        for attempt in 0..QUEUE_LOCK_ATTEMPTS {
            let Ok(queue) = self.inner.try_lock() else {
                if attempt + 1 < QUEUE_LOCK_ATTEMPTS {
                    std::thread::sleep(QUEUE_LOCK_BACKOFF);
                }
                continue;
            };
            for line in checkpoints.last_reached(*queue) {
                tracing::error!("device loss checkpoint: {line}");
            }
            return;
        }
        tracing::error!("device loss checkpoints unavailable: the queue is held by a blocked call");
    }

    /// Whether the device reports itself lost, asked with an empty submit: it enqueues no work and
    /// waits for nothing, so it is the cheapest legal way to learn that the device is gone. `false`
    /// while the queue's external-synchronization lock is held, so the caller asks again rather
    /// than blocking behind a wedged wait.
    pub(crate) fn reports_device_lost(&self, raw: &ash::Device) -> bool {
        let Ok(queue) = self.inner.try_lock() else {
            return false;
        };
        // SAFETY: the ash seam. The queue is externally synchronized by the guard held across the
        // call; an empty batch list with a null fence submits nothing.
        let probe = unsafe { raw.queue_submit2(*queue, &[], vk::Fence::null()) };
        probe == Err(vk::Result::ERROR_DEVICE_LOST)
    }

    /// Presents one swapchain image under the same external-synchronization lock as submits.
    pub(crate) fn present(
        &self,
        loader: &ash::khr::swapchain::Device,
        info: &vk::PresentInfoKHR<'_>,
    ) -> std::result::Result<bool, vk::Result> {
        let queue = self.inner.lock().expect("gpu queue mutex");
        // SAFETY: the ash seam. The queue is externally synchronized by the mutex guard
        // held across the call.
        unsafe { loader.queue_present(*queue, info) }
    }

    /// Waits for the logical device while excluding concurrent queue submissions.
    pub(crate) fn wait_device_idle(&self, raw: &ash::Device) -> Result<()> {
        let _queue = self.inner.lock().expect("gpu queue mutex");
        checked(unsafe { raw.device_wait_idle() }, "device_wait_idle")
    }

    /// Waits for this queue alone under its external-synchronization lock.
    pub(crate) fn wait_queue_idle(&self, raw: &ash::Device) -> Result<()> {
        let queue = *self.inner.lock().expect("gpu queue mutex");
        checked(unsafe { raw.queue_wait_idle(queue) }, "queue_wait_idle")
    }
}

/// The one-off upload helper: a dedicated command pool plus the shared queue.
///
/// One [`Uploader`] per thread — Vulkan command pools are not thread-safe, so the
/// thumbnail worker constructs its own with a clone of the same [`GpuQueue`]. The
/// pool's buffers are short-lived (allocated, recorded, submitted, freed per call).
/// [`Drop`] frees the pool.
pub struct Uploader {
    resources: Arc<DeviceResources>,
    queue: GpuQueue,
    command_pool: vk::CommandPool,
    /// The acceleration-structure dispatch for building a per-mesh BLAS at upload time. `None` on
    /// a software device — the mesh's `blas` then stays `None` and rendering takes the shadow-map
    /// path.
    accel: Option<ash::khr::acceleration_structure::Device>,
    /// Whether the device advertises `VK_EXT_opacity_micromap`, which decides whether a BLAS may
    /// be built granting instances permission to disable a micromap. Without the extension that
    /// permission is an invalid flag rather than an inert one.
    omm_supported: bool,
    /// The micromap dispatch, present only when the device enabled `VK_EXT_opacity_micromap`.
    omm: Option<ash::ext::opacity_micromap::Device>,
    /// The cluster-AS builder, present only when the device enabled
    /// `VK_NV_cluster_acceleration_structure`; an assembly prototype's structure then
    /// composes from its cooked clusters instead of the KHR triangle build.
    cluster: Option<crate::rt_cluster::ClusterBlasBuilder>,
    /// The device's `maxOpacity4StateSubdivisionLevel`; a cooked row above it is device-loss class.
    omm_max_subdivision: u32,
    /// The two GPU jump-flood bake compute pipelines (voxelize → JFA) the SDF bake dispatches on
    /// the one-off command buffer; the sign pass is on the host. Owned here rather than by the
    /// renderer's frame PSO cache because the bake runs on the upload path. `None` when the
    /// pipelines fail to build: the mesh then uploads with no field.
    bake: Option<BakePipelines>,
    /// Two-timestamp pool bracketing the out-of-graph acceleration-structure submits. The static
    /// BLAS build and its compaction run on this private one-off pool, so no graph pass scope
    /// covers them, and they scale with content rather than with frame rate.
    accel_timestamps: Option<(vk::QueryPool, f32)>,
}

// SAFETY: the pool handle is owned by this `Uploader` and used only from the thread
// that holds it (one `Uploader` per thread); the `Arc`/`GpuQueue` are `Send`.
unsafe impl Send for Uploader {}

impl Uploader {
    /// Creates an uploader with its own one-off command pool on the graphics family.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if the command pool cannot be created.
    pub fn new(device: &Device, queue: &GpuQueue) -> Result<Self> {
        let info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. The create-info is valid; the pool is owned and freed
        // in `Drop`.
        let command_pool = checked(
            unsafe { device.raw().create_command_pool(&info, None) },
            "create_command_pool (uploader)",
        )?;
        let resources = Arc::clone(device.resources());
        // Build the two bake compute pipelines off the runtime shader dir. A failure
        // (missing/invalid SPIR-V) is logged, not fatal — meshes then upload without a field.
        let bake = match BakePipelines::new(&resources) {
            Ok(bake) => Some(bake),
            Err(err) => {
                tracing::warn!("SDF bake pipelines unavailable: {err}");
                None
            }
        };
        Ok(Self {
            resources,
            queue: queue.clone(),
            command_pool,
            accel: device.accel_dispatch().cloned(),
            omm_supported: device.omm_supported(),
            omm: device.omm_dispatch().cloned(),
            cluster: crate::rt_cluster::ClusterBlasBuilder::new(device),
            omm_max_subdivision: device.omm_max_subdivision(),
            bake,
            accel_timestamps: accel_timestamp_pool(device),
        })
    }

    /// The ash device this uploader records against.
    fn raw(&self) -> &ash::Device {
        self.resources.device()
    }

    /// The VMA allocator this uploader stages through.
    fn allocator(&self) -> &vk_mem::Allocator {
        self.resources.allocator()
    }

    /// Allocates a primary one-off command buffer, records `record` into it, submits
    /// it on the shared queue, and blocks on a fresh fence (never `device.waitIdle`,
    /// which would drain the in-flight scene frame). Frees the buffer + fence. `label`
    /// names the submission in the slow-buffer warning: a one-off whose GPU execution
    /// nears the platform watchdog risks a device loss, so anything past half a second
    /// logs.
    fn with_one_off_commands<R>(&self, label: &'static str, record: R) -> Result<()>
    where
        R: FnOnce(vk::CommandBuffer),
    {
        self.with_one_off_commands_timed(label, false, record)
    }

    /// Accumulates GPU time into the resources' accel-build total when `timed` and the device can.
    ///
    /// Only the acceleration-structure submits ask for this: timing every one-off would charge
    /// staging copies and SDF bakes to a number read as "structure build time".
    fn with_one_off_commands_timed<R>(
        &self,
        label: &'static str,
        timed: bool,
        record: R,
    ) -> Result<()>
    where
        R: FnOnce(vk::CommandBuffer),
    {
        let timing = timed.then_some(self.accel_timestamps.as_ref()).flatten();
        let record = |cmd: vk::CommandBuffer| {
            if let Some((pool, _)) = timing {
                // SAFETY: the ash seam. Resets both queries and stamps the opening one on a
                // buffer this call owns for its whole lifetime.
                unsafe {
                    self.raw().cmd_reset_query_pool(cmd, *pool, 0, 2);
                    self.raw().cmd_write_timestamp2(
                        cmd,
                        vk::PipelineStageFlags2::TOP_OF_PIPE,
                        *pool,
                        0,
                    );
                }
            }
            record(cmd);
            if let Some((pool, _)) = timing {
                // SAFETY: the ash seam. Stamps the closing query on the same owned buffer.
                unsafe {
                    self.raw().cmd_write_timestamp2(
                        cmd,
                        vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
                        *pool,
                        1,
                    );
                }
            }
        };
        self.submit_one_off(label, record)?;
        if timing.is_some() {
            self.accumulate_accel_time();
        }
        Ok(())
    }

    /// Reads the bracketed span out of the pool and adds it to the running total. The submit was
    /// already waited, so both queries resolve without a stall; a failed read is dropped rather
    /// than counted as zero, which would read as a fast build rather than an unmeasured one.
    fn accumulate_accel_time(&self) {
        let Some((pool, period)) = self.accel_timestamps.as_ref() else {
            return;
        };
        let mut stamps = [0_u64; 2];
        // SAFETY: the ash seam. The submit completed on its fence, so both queries are available.
        let read = unsafe {
            self.raw().get_query_pool_results(
                *pool,
                0,
                &mut stamps,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
            )
        };
        if read.is_ok() {
            let elapsed = stamps[1].saturating_sub(stamps[0]);
            self.resources
                .add_accel_build_nanos((elapsed as f64 * f64::from(*period)) as u64);
        }
    }

    fn submit_one_off<R>(&self, label: &'static str, record: R) -> Result<()>
    where
        R: FnOnce(vk::CommandBuffer),
    {
        let started = std::time::Instant::now();
        let raw = self.raw();
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One primary buffer from this uploader's own pool.
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc_info) },
            "allocate_command_buffers (one-off)",
        )?[0];

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let recorded = (|| -> Result<()> {
            // SAFETY: the ash seam. Begin/record/end on the freshly allocated buffer.
            checked(
                unsafe { raw.begin_command_buffer(cmd, &begin) },
                "begin_command_buffer (one-off)",
            )?;
            if let Some(checkpoints) = self.resources.checkpoints() {
                checkpoints.mark(cmd, label);
            }
            record(cmd);
            // SAFETY: the ash seam. Ends the recording opened above.
            checked(
                unsafe { raw.end_command_buffer(cmd) },
                "end_command_buffer (one-off)",
            )?;
            // Registered across the wait, so a submission that never completes is still named.
            let _watch = crate::watchdog::watch(label, 0);
            self.submit_and_wait(cmd)
        })();

        // SAFETY: the ash seam. The submit fence was waited (or never submitted), so
        // the buffer is idle and freed exactly once.
        unsafe { raw.free_command_buffers(self.command_pool, &[cmd]) };
        let elapsed = started.elapsed();
        if elapsed.as_millis() > 500 {
            tracing::warn!(
                label,
                ms = elapsed.as_secs_f32() * 1000.0,
                "one-off GPU submission ran long"
            );
        }
        recorded
    }

    /// Submits one already-recorded buffer on the shared queue with a fresh fence and
    /// waits on *its* completion. The submit takes the queue mutex; the wait does not.
    fn submit_and_wait(&self, cmd: vk::CommandBuffer) -> Result<()> {
        let raw = self.raw();
        // SAFETY: the ash seam. A default (unsignaled) fence, destroyed below.
        let fence = checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "create_fence (one-off)",
        )?;

        let cmd_info = vk::CommandBufferSubmitInfo::default().command_buffer(cmd);
        let cmd_infos = [cmd_info];
        let submit = vk::SubmitInfo2::default().command_buffer_infos(&cmd_infos);
        let submits = [submit];

        let result = self
            .queue
            .submit2(raw, &submits, fence, "queue_submit2 (one-off)")
            .and_then(|()| {
                // SAFETY: the ash seam. The fence belongs to this device; the wait blocks
                // until the one-off submit completes.
                checked(
                    unsafe { raw.wait_for_fences(&[fence], true, u64::MAX) },
                    "wait_for_fences (one-off)",
                )
            });

        // SAFETY: the ash seam. The fence was waited (or the submit failed before
        // signaling it), so it is idle and destroyed exactly once.
        unsafe { raw.destroy_fence(fence, None) };
        if result.as_ref().is_err_and(crate::Error::is_device_loss) {
            self.queue
                .log_device_loss_checkpoints(self.resources.checkpoints());
            self.resources.log_device_fault();
        }
        result
    }

    /// Frees an image + its allocation directly (the error-path cleanup before a
    /// `GpuTexture` ever takes ownership).
    fn destroy_image(&self, image: vk::Image, mut allocation: vk_mem::Allocation) {
        // SAFETY: the VMA seam. The image was created on this allocator and not yet
        // owned by a `GpuTexture`; freed exactly once on the error path.
        unsafe { self.allocator().destroy_image(image, &mut allocation) };
    }
}

impl Drop for Uploader {
    fn drop(&mut self) {
        // SAFETY: the ash seam. All one-off buffers are freed per call (none in
        // flight); the pool is destroyed exactly once. The `Arc<DeviceResources>`
        // keeps the device alive for the call.
        unsafe {
            if let Some((pool, _)) = self.accel_timestamps {
                self.resources.device().destroy_query_pool(pool, None);
            }
            self.resources
                .device()
                .destroy_command_pool(self.command_pool, None);
        }
    }
}

/// Narrows one finite f32 to an IEEE binary16 (round-to-nearest-even). Subnormals are
/// flushed where the source underflows; finite magnitudes above the f16 max saturate
/// to ±inf, matching what the GPU produces sampling an f16 texture.
pub(crate) fn float_to_half(value: f32) -> u16 {
    let mut bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    bits &= 0x7fff_ffff;
    if bits >= 0x7f80_0000 {
        // inf / nan: keep nan non-zero so it stays nan.
        let mant: u16 = if bits > 0x7f80_0000 { 0x0200 } else { 0 };
        return sign | 0x7c00 | mant;
    }
    if bits >= 0x4780_0000 {
        return sign | 0x7c00; // overflow -> inf
    }
    if bits < 0x3880_0000 {
        // subnormal/zero in f16: round the value scaled into the denormal range.
        let mant = (bits & 0x007f_ffff) | 0x0080_0000;
        let shift = 113_i32 - (bits >> 23) as i32;
        let rounded = if shift < 24 { mant >> shift } else { 0 };
        let half = (rounded + 0x0000_0fff + ((rounded >> 13) & 1)) >> 13;
        return sign | half as u16;
    }
    let rebiased = bits.wrapping_add(0xc800_0000); // exponent rebias (127 -> 15)
    let rounded = (rebiased + 0x0000_0fff + ((rebiased >> 13) & 1)) >> 13;
    sign | rounded as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The known IEEE half encodings: exact representables, the f16-max overflow to +inf, and
    /// the sign bit.
    #[test]
    fn float_to_half_matches_known_encodings() {
        assert_eq!(float_to_half(0.0), 0x0000);
        assert_eq!(float_to_half(-0.0), 0x8000);
        assert_eq!(float_to_half(1.0), 0x3c00);
        assert_eq!(float_to_half(2.0), 0x4000);
        assert_eq!(float_to_half(0.5), 0x3800);
        assert_eq!(float_to_half(-1.0), 0xbc00);
        // The largest finite half (65504.0) is exactly representable.
        assert_eq!(float_to_half(65504.0), 0x7bff);
        // Above the f16 max saturates to +inf; a real inf stays inf.
        assert_eq!(float_to_half(1.0e30), 0x7c00);
        assert_eq!(float_to_half(f32::INFINITY), 0x7c00);
        assert_eq!(float_to_half(f32::NEG_INFINITY), 0xfc00);
        // NaN stays NaN (a non-zero mantissa with the inf exponent).
        let nan = float_to_half(f32::NAN);
        assert_eq!(nan & 0x7c00, 0x7c00);
        assert_ne!(nan & 0x03ff, 0, "NaN keeps a non-zero mantissa");
    }
}
