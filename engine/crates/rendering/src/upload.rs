//! The mesh and texture upload paths — the staging-copy-to-device-local sequences
//! that produce the [`Arc`]`<`[`GpuMesh`]`>` / [`Arc`]`<`[`GpuTexture`]`>` the asset
//! layer and scene draw consume.
//!
//! These hang off an [`Uploader`] that owns the one-off command pool and a clone of
//! the externally-synchronized [`GpuQueue`] — the README §5 first shared-mutable site,
//! so the worker thread can upload off the main thread.
//!
//! # The submit mutex (README §5)
//!
//! The graphics queue is externally synchronized: the frame loop and the thumbnail
//! worker both submit on it. So the queue lives behind [`GpuQueue`] (an
//! `Arc<Mutex<vk::Queue>>`), and the one-off submit here takes the lock for the
//! submit2 only — the fence wait is outside the lock so a long upload does not stall
//! a sibling submit. A command pool is *not* thread-safe, so an [`Uploader`] owns its
//! own pool; the worker thread builds its own [`Uploader`] with the same shared queue.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ash::vk;
use saffron_geometry::glam::Vec3;
use saffron_geometry::{
    GridDesc, Mesh, MeshConditioning, MorphData, MorphDelta, PortableVirtualHierarchy, Sdf,
    Submesh, VertexSkin, bake_grid, build_min_max_pyramid, sdf_chunk_cores, sdf_set_from_bytes,
    sdf_set_to_bytes, validate_portable_virtual_hierarchy,
};
use vk_mem::Alloc;

use crate::descriptors::Descriptors;
use crate::resources::{
    ConditioningBuffers, DeviceResources, GpuLut, GpuMesh, GpuMeshParts, GpuSdf, GpuSdfParts,
    GpuTexture, GpuTextureParts, Image, Image3D, ImageDesc, MinMaxPyramid, MorphBuffers,
};
use crate::{Device, Error, GradeUniform, Pipeline, Result, checked};

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

fn validate_upload_hierarchy(mesh: &Mesh, hierarchy: &PortableVirtualHierarchy) -> Result<()> {
    validate_portable_virtual_hierarchy(hierarchy)
        .map_err(|error| Error::InvalidUploadData(error.to_string()))?;
    if hierarchy.prototypes.is_empty() {
        return Err(Error::InvalidUploadData(
            "portable hierarchy has no geometry prototype".to_owned(),
        ));
    }
    // The uploaded vertex stream is the prototypes' streams concatenated in prototype-id
    // order; the submesh table concatenates the per-prototype ranges the same way (a
    // single-prototype mesh with no authored submeshes uploads one implicit full range).
    let vertex_total: usize = hierarchy
        .prototypes
        .iter()
        .map(|prototype| prototype.vertex_count as usize)
        .sum();
    let submesh_total: usize = hierarchy
        .prototypes
        .iter()
        .map(|prototype| prototype.submesh_count as usize)
        .sum();
    if vertex_total != mesh.vertices.len() || submesh_total != mesh.submeshes.len().max(1) {
        return Err(Error::InvalidUploadData(
            "portable hierarchy does not describe the uploaded mesh".to_owned(),
        ));
    }
    Ok(())
}

/// Builds the assembly-part table for a hierarchy that places prototypes through uses —
/// `None` for the trivial single-prototype, single-identity-use shape every plain mesh
/// cooks to (its executor path stays base-free).
fn assembly_from_hierarchy(
    hierarchy: &PortableVirtualHierarchy,
) -> Result<Option<crate::MeshAssembly>> {
    const IDENTITY_BITS: [i32; 16] = [
        65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536,
    ];
    let trivial = hierarchy.prototypes.len() == 1
        && hierarchy.micro_instances.len() == 1
        && hierarchy.micro_instances[0].prototype == 0
        && hierarchy.micro_instances[0].transform_bits == IDENTITY_BITS;
    if trivial {
        return Ok(None);
    }
    if hierarchy.micro_instances.is_empty() {
        return Err(Error::InvalidUploadData(
            "assembly hierarchy places no prototype uses".to_owned(),
        ));
    }
    // The parts table is indexed by prototype id, so the hierarchy's table must be
    // id-ordered (the cooker emits it that way).
    for (index, prototype) in hierarchy.prototypes.iter().enumerate() {
        if prototype.id as usize != index {
            return Err(Error::InvalidUploadData(
                "assembly hierarchy prototype table is not id-ordered".to_owned(),
            ));
        }
    }
    let mut prototypes = Vec::with_capacity(hierarchy.prototypes.len());
    let mut uses = Vec::with_capacity(hierarchy.micro_instances.len());
    // Each use carries its part's structural semantic tag so the wind branch modes
    // pick the response per part (trunk 0 … blade 7; absent parts read trunk).
    let semantic_by_part: std::collections::HashMap<u128, u32> = hierarchy
        .deformation
        .iter()
        .map(|region| (region.part, u32::from(region.semantic.0)))
        .collect();
    let mut vertex_base = 0_u64;
    for prototype in &hierarchy.prototypes {
        let first_use = u32::try_from(uses.len())
            .map_err(|_| Error::InvalidUploadData("assembly use table overflow".to_owned()))?;
        for instance in hierarchy
            .micro_instances
            .iter()
            .filter(|instance| instance.prototype == prototype.id)
        {
            let mut transform = [0.0_f32; 12];
            for (slot, bits) in transform
                .iter_mut()
                .zip(instance.transform_bits.iter().take(12))
            {
                *slot = *bits as f32 / 65_536.0;
            }
            uses.push(crate::GpuAssemblyUseRecord {
                transform,
                prototype: prototype.id,
                reserved: [
                    semantic_by_part.get(&instance.part).copied().unwrap_or(0),
                    0,
                    0,
                ],
            });
        }
        let use_count = u32::try_from(uses.len())
            .map_err(|_| Error::InvalidUploadData("assembly use table overflow".to_owned()))?
            - first_use;
        if use_count == 0 {
            return Err(Error::InvalidUploadData(
                "assembly hierarchy leaves a prototype unused".to_owned(),
            ));
        }
        // The executor's vertex base counts vertices (its fetch multiplies by the stride).
        let base = u32::try_from(vertex_base).map_err(|_| {
            Error::InvalidUploadData("assembly vertex stream exceeds u32 bases".to_owned())
        })?;
        prototypes.push(crate::GpuAssemblyPrototypeRecord {
            first_use,
            use_count,
            vertex_base: base,
            reserved: 0,
        });
        vertex_base = vertex_base
            .checked_add(u64::from(prototype.vertex_count))
            .ok_or_else(|| {
                Error::InvalidUploadData("assembly vertex stream overflow".to_owned())
            })?;
    }
    // The mask table: authored combinations, or one implicit all-active combination
    // for an assembly cooked without variation data.
    let mask_words = uses.len().div_ceil(32);
    let (combinations, masks) = if hierarchy.combinations.is_empty() {
        let mut words = vec![0_u32; mask_words];
        for use_index in 0..uses.len() {
            words[use_index / 32] |= 1 << (use_index % 32);
        }
        (vec![(0, 0)], words)
    } else {
        let mut identities = Vec::with_capacity(hierarchy.combinations.len());
        let mut words = Vec::with_capacity(hierarchy.combinations.len() * mask_words);
        for combination in &hierarchy.combinations {
            if combination.active_words.len() != mask_words {
                return Err(Error::InvalidUploadData(
                    "assembly combination mask does not span the use table".to_owned(),
                ));
            }
            identities.push((combination.variation, combination.phenotype));
            words.extend_from_slice(&combination.active_words);
        }
        (identities, words)
    };
    Ok(Some(crate::MeshAssembly {
        prototypes,
        uses,
        combinations,
        masks,
    }))
}

#[cfg(test)]
pub(crate) fn hierarchy_for_upload(
    mesh: &Mesh,
    skin: &[VertexSkin],
) -> Result<PortableVirtualHierarchy> {
    let input = saffron_geometry::PortableHierarchyInput::from_mesh(mesh, skin)
        .map_err(|error| Error::InvalidUploadData(error.to_string()))?;
    saffron_geometry::cook_portable_virtual_hierarchy(&input)
        .map_err(|error| Error::InvalidUploadData(error.to_string()))
}

/// One prefiltered RGBA8 mip supplied to [`Uploader::upload_texture_mips`].
#[derive(Clone, Copy, Debug)]
pub struct TextureMipLevel<'a> {
    /// Tightly packed RGBA8 texels.
    pub rgba: &'a [u8],
    /// Level width.
    pub width: u32,
    /// Level height.
    pub height: u32,
}

fn valid_prefiltered_mip_chain(mips: &[TextureMipLevel<'_>]) -> bool {
    let Some(base) = mips.first() else {
        return false;
    };
    base.width != 0
        && base.height != 0
        && mips.len() == mip_count(base.width, base.height) as usize
        && mips.iter().enumerate().all(|(level, mip)| {
            let expected_bytes = (mip.width as usize)
                .checked_mul(mip.height as usize)
                .and_then(|texels| texels.checked_mul(4));
            mip.width == base.width.checked_shr(level as u32).unwrap_or(0).max(1)
                && mip.height == base.height.checked_shr(level as u32).unwrap_or(0).max(1)
                && expected_bytes == Some(mip.rgba.len())
        })
}

/// The externally-synchronized graphics queue, shared behind a mutex.
///
/// README §5's first `Arc<Mutex>` site: the frame loop's submit/present and the worker
/// thread's upload submits all take this lock. Cloning the `Arc` hands a second
/// thread the same queue under the same lock.
#[derive(Clone)]
pub struct GpuQueue {
    inner: Arc<Mutex<vk::Queue>>,
}

// SAFETY: a `vk::Queue` is a raw handle; the `Mutex` provides the external
// synchronization Vulkan requires for queue submission. Sharing it across threads is
// exactly the README §5 contract (the worker thread submits uploads on it).
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
    /// The acceleration-structure dispatch for building a per-mesh BLAS at upload time when
    /// RT is supported. `None` on a software device —
    /// the mesh's `blas` then stays `None` and the engine renders via the shadow-map path.
    accel: Option<ash::khr::acceleration_structure::Device>,
    /// The two GPU jump-flood bake compute pipelines (voxelize → JFA) the SDF bake dispatches
    /// on the one-off command buffer; the sign pass is on the host. Owned here (not the
    /// renderer's frame PSO cache) because the bake runs on the upload path — including the
    /// thumbnail worker's own [`Uploader`]. `None` if the bake pipelines fail to build (a
    /// missing shader): the mesh then uploads with no field, not a fatal error.
    bake: Option<BakePipelines>,
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
            bake,
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
        result
    }

    /// Builds the per-mesh BLAS once (a synchronous one-off submit, like the upload copy)
    /// when RT is supported, returning a shared [`crate::AccelerationStructure`]. The build
    /// scratch is held across the submit then dropped. `None` on a software device.
    fn build_mesh_blas(
        &self,
        vertex_buffer: vk::Buffer,
        vertex_count: u32,
        index_buffer: vk::Buffer,
        index_count: u32,
    ) -> Result<Option<Arc<crate::AccelerationStructure>>> {
        let Some(dispatch) = self.accel.as_ref() else {
            return Ok(None);
        };
        if index_count < 3 {
            return Ok(None);
        }
        let raw = self.raw();
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One primary buffer from this uploader's own pool.
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc_info) },
            "allocate_command_buffers (blas)",
        )?[0];
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let built = (|| -> Result<crate::MeshBlasBuild> {
            // SAFETY: the ash seam. Begin the freshly allocated buffer.
            checked(
                unsafe { raw.begin_command_buffer(cmd, &begin) },
                "begin_command_buffer (blas)",
            )?;
            let build = crate::record_mesh_blas_build(
                &self.resources,
                dispatch,
                cmd,
                vertex_buffer,
                vertex_count,
                index_buffer,
                index_count,
            )?;
            // SAFETY: the ash seam. Ends the recording opened above.
            checked(
                unsafe { raw.end_command_buffer(cmd) },
                "end_command_buffer (blas)",
            )?;
            self.submit_and_wait(cmd)?;
            Ok(build)
        })();
        // SAFETY: the ash seam. The submit was waited (or never happened), so the buffer is
        // idle and freed exactly once.
        unsafe { raw.free_command_buffers(self.command_pool, &[cmd]) };
        // The scratch is no longer needed once the build submit completed; drop it.
        built.map(|build| {
            drop(build.scratch);
            Some(Arc::new(build.blas))
        })
    }

    /// Uploads a mesh's vertex + index streams (and the optional [`VertexSkin`]
    /// stream) into device-local buffers, returning a shared [`GpuMesh`].
    ///
    /// One staging buffer holds `[vertices | indices | skin]`; copies fan it out to
    /// the device-local buffers. The skin stream, when present, must parallel the
    /// vertices (one [`VertexSkin`] per vertex); it carries `STORAGE` usage too (the
    /// compute skinning prepass reads it).
    ///
    /// When `sdf_bake` is present the per-mesh signed distance field is GPU jump-flood baked
    /// (or read from the sidecar cache) from the mesh geometry, uploaded as the sparse SDST
    /// v2 brick atlas + indirection volume into the bindless SDF arrays of `descriptors`,
    /// and stored on the returned mesh — so the field lives exactly as long as the mesh and
    /// the lighting cone-trace can index it by its bindless slot. A failed bake/upload is
    /// logged, not fatal — the mesh renders without a field.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyMesh`] for an empty mesh, [`Error::SkinMismatch`] when a
    /// skin stream does not parallel the vertices, or [`Error::Vk`] for a failing
    /// Vulkan/VMA call. Resources allocated before a failure are freed before return.
    pub fn upload_mesh(
        &self,
        descriptors: &Descriptors,
        mesh: &Mesh,
        hierarchy: &PortableVirtualHierarchy,
        skin: &[VertexSkin],
        morph: Option<&MorphData>,
        sdf_bake: Option<&SdfBake>,
    ) -> Result<Arc<GpuMesh>> {
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            return Err(Error::EmptyMesh);
        }
        if !skin.is_empty() && skin.len() != mesh.vertices.len() {
            return Err(Error::SkinMismatch {
                skin: skin.len(),
                vertices: mesh.vertices.len(),
            });
        }
        validate_upload_hierarchy(mesh, hierarchy)?;

        let vertex_bytes = std::mem::size_of_val(mesh.vertices.as_slice()) as vk::DeviceSize;
        let index_bytes = std::mem::size_of_val(mesh.indices.as_slice()) as vk::DeviceSize;
        let skin_bytes = std::mem::size_of_val(skin) as vk::DeviceSize;

        // One staging buffer holds the three streams concatenated.
        let mut staging =
            StagingBuffer::new(self.allocator(), vertex_bytes + index_bytes + skin_bytes)?;
        {
            let bytes = staging.mapped_slice();
            let vb = vertex_bytes as usize;
            let ib = index_bytes as usize;
            bytes[..vb].copy_from_slice(bytemuck::cast_slice(&mesh.vertices));
            bytes[vb..vb + ib].copy_from_slice(bytemuck::cast_slice(&mesh.indices));
            if !skin.is_empty() {
                bytes[vb + ib..].copy_from_slice(bytemuck::cast_slice(skin));
            }
        }
        staging.flush();

        // Compute bounds; the complete CPU vertex stream is retained for surface queries.
        let mut bounds_min = Vec3::splat(f32::MAX);
        let mut bounds_max = Vec3::splat(f32::MIN);
        let mut cpu_positions = Vec::with_capacity(mesh.vertices.len());
        for vertex in &mesh.vertices {
            bounds_min = bounds_min.min(vertex.position);
            bounds_max = bounds_max.max(vertex.position);
            cpu_positions.push(vertex.position);
        }

        // When RT is on, the vertex/index buffers also feed BLAS builds: they need shader
        // device address + AS-build-input usage.
        let rt_usage = if self.accel.is_some() {
            vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
                | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
        } else {
            vk::BufferUsageFlags::empty()
        };

        // The base vertex stream is also read as a storage buffer by the compute deform pre-passes:
        // skinning + morph (skinned/morph meshes), and the `displace` pre-pass, which reads the base
        // positions/normals/tangents of *any* mesh carrying a displacement material — displacement is
        // a per-material choice, not a mesh property, so every mesh's vertex buffer carries STORAGE.
        let vertex_usage =
            vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::STORAGE_BUFFER | rt_usage;

        // Allocate the device-local buffers; on a later failure free the
        // already-allocated ones (a `GpuMesh` never partially owns the set). Each
        // allocation is uniquely owned (a VMA `Allocation` is not `Copy`), so they are
        // freed directly here rather than tracked by a copied handle.
        let allocator = self.allocator();
        let vertex = make_device_buffer(allocator, vertex_bytes, vertex_usage)?;
        // STORAGE too: the tessellation emit kernel reads the base index stream as a `ByteAddressBuffer`
        // (`baseIndices`) to fetch each base triangle's corners, so a displaced mesh's index buffer is
        // bound as a storage buffer — the same per-material reason the vertex buffer carries STORAGE.
        let index = match make_device_buffer(
            allocator,
            index_bytes,
            vk::BufferUsageFlags::INDEX_BUFFER | vk::BufferUsageFlags::STORAGE_BUFFER | rt_usage,
        ) {
            Ok(buf) => buf,
            Err(err) => {
                free_one(allocator, vertex);
                return Err(err);
            }
        };
        let skin_buf = if skin.is_empty() {
            None
        } else {
            match make_device_buffer(
                allocator,
                skin_bytes,
                vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::STORAGE_BUFFER,
            ) {
                Ok(buf) => Some(buf),
                Err(err) => {
                    free_one(allocator, vertex);
                    free_one(allocator, index);
                    return Err(err);
                }
            }
        };

        // Record + submit the staging copies.
        let copy = self.with_one_off_commands("upload_mesh", |cmd| {
            // SAFETY: the ash seam. The buffers outlive the submit-wait; the staging
            // buffer is the upload source.
            unsafe {
                let raw = self.raw();
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    vertex.0,
                    &[vk::BufferCopy::default()
                        .src_offset(0)
                        .dst_offset(0)
                        .size(vertex_bytes)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    index.0,
                    &[vk::BufferCopy::default()
                        .src_offset(vertex_bytes)
                        .dst_offset(0)
                        .size(index_bytes)],
                );
                if let Some((skin_buffer, _)) = skin_buf {
                    raw.cmd_copy_buffer(
                        cmd,
                        staging.handle(),
                        skin_buffer,
                        &[vk::BufferCopy::default()
                            .src_offset(vertex_bytes + index_bytes)
                            .dst_offset(0)
                            .size(skin_bytes)],
                    );
                }
            }
        });
        drop(staging);
        if let Err(err) = copy {
            free_one(allocator, vertex);
            free_one(allocator, index);
            if let Some(buf) = skin_buf {
                free_one(allocator, buf);
            }
            return Err(err);
        }

        // Build this mesh's BLAS once (the RT geometry occlusion oracle) when RT is
        // available. A failure is logged, not fatal — the mesh renders without RT shadows.
        // An assembly's shape is its placed uses, not the concatenated prototype streams,
        // so it carries no merged BLAS (its RT representation is per-use instancing).
        let assembly = assembly_from_hierarchy(hierarchy)?;
        let blas = if assembly.is_some() {
            None
        } else {
            match self.build_mesh_blas(
                vertex.0,
                mesh.vertices.len() as u32,
                index.0,
                mesh.indices.len() as u32,
            ) {
                Ok(blas) => blas,
                Err(err) => {
                    tracing::warn!("BLAS build failed: {err}");
                    None
                }
            }
        };

        // Build the morph buffers (flat delta array + per-target ranges) when the mesh
        // carries blend shapes; free the already-owned buffers on a morph-upload failure.
        let morph_buffers = match morph.filter(|m| !m.targets.is_empty()) {
            Some(data) => match self.upload_morph_buffers(data) {
                Ok(buffers) => Some(buffers),
                Err(err) => {
                    free_one(allocator, vertex);
                    free_one(allocator, index);
                    if let Some(buf) = skin_buf {
                        free_one(allocator, buf);
                    }
                    return Err(err);
                }
            },
            None => None,
        };

        // GPU jump-flood bake (or sidecar-cache load) the per-mesh signed distance fields —
        // one tight field per primitive, and per spatial chunk of an oversized primitive — and
        // upload each into the bindless SDF arrays. A failure is logged, not fatal: the mesh
        // renders without a field (the cone-trace simply omits an instance with no SDF slot). A
        // degenerate mesh (< 1 triangle) bakes no field.
        let gpu_sdfs = match sdf_bake
            .filter(|_| mesh.indices.len() >= 3 && !cpu_positions.is_empty())
        {
            Some(bake) => {
                match self.bake_or_load_sdf(&cpu_positions, &mesh.indices, &mesh.submeshes, bake) {
                    Ok(fields) => fields
                        .iter()
                        .filter_map(|sdf| match self.upload_sdf(descriptors, sdf) {
                            Ok(field) => Some(field),
                            Err(err) => {
                                tracing::warn!("SDF upload failed: {err}");
                                None
                            }
                        })
                        .collect(),
                    Err(err) => {
                        tracing::warn!("SDF bake failed: {err}");
                        Vec::new()
                    }
                }
            }
            None => Vec::new(),
        };

        // Build + upload the watertight-conditioning buffers (edges/weld/basis). A failure is logged,
        // not fatal — nothing consumes them until Phase 3, so the mesh simply carries none.
        let conditioning_buffers = match self.upload_conditioning_buffers(mesh) {
            Ok(buffers) => buffers,
            Err(err) => {
                tracing::warn!("conditioning upload failed: {err}");
                None
            }
        };

        let parts = GpuMeshParts {
            vertex,
            index,
            skin: skin_buf,
            morph: morph_buffers,
            conditioning: conditioning_buffers,
            index_count: mesh.indices.len() as u32,
            vertex_count: mesh.vertices.len() as u32,
            submeshes: mesh.submeshes.clone(),
            bounds_min,
            bounds_max,
            cpu_vertices: mesh.vertices.clone(),
            cpu_indices: mesh.indices.clone(),
            cpu_skin: skin.to_vec(),
            blas,
            sdfs: gpu_sdfs,
            hierarchy_pages: hierarchy.pages.clone(),
            assembly,
        };
        Ok(Arc::new(GpuMesh::from_parts(&self.resources, parts)))
    }

    /// Uploads a sparse SDST v3 [`Sdf`] as three device-local `Texture3D`s — the mipped
    /// `R16_SNORM` brick atlas (`mip_count` prefiltered levels), the `R32_UINT` brick
    /// indirection volume, and the `R16_SNORM` coarse coverage volume — claims one slot in the
    /// bindless SDF arrays of `descriptors`, writes all three views at that slot (bindings
    /// 1 + 2 + 3), and wraps them as a [`GpuSdf`] owning the slot + the v3 brick metadata.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ZeroSizedImage`] for an empty field or [`Error::Vk`] for a failing
    /// Vulkan/VMA call; allocated resources are freed before return on error.
    pub fn upload_sdf(&self, descriptors: &Descriptors, sdf: &Sdf) -> Result<Arc<GpuSdf>> {
        let h = sdf.header;
        if h.dims.contains(&0) || h.indirection_dims.contains(&0) || h.coverage_dims.contains(&0) {
            return Err(Error::ZeroSizedImage);
        }
        // The brick atlas carries `mip_count` prefiltered levels (finest first): each level's
        // band-limited distance feeds the cone-footprint mip-select. Build the per-level data +
        // extents the upload copies into the mipped image's subresources.
        let mip_count = h.mip_count.max(1);
        let mip_data: Vec<Vec<i16>> = (0..mip_count)
            .map(|m| sdf.atlas_image_data_mip(m))
            .collect();
        let atlas_mips: Vec<(vk::Extent3D, &[u8])> = (0..mip_count)
            .map(|m| {
                let [w, hh, d] = sdf.atlas_image_dims_mip(m);
                (
                    vk::Extent3D {
                        width: w,
                        height: hh,
                        depth: d,
                    },
                    bytemuck::cast_slice(&mip_data[m as usize]),
                )
            })
            .collect();
        let (atlas_image, atlas_view, atlas_alloc) =
            self.create_and_upload_sdf_image(vk::Format::R16_SNORM, &atlas_mips)?;
        drop(atlas_mips);

        let [ix, iy, iz] = h.indirection_dims;
        let indir = match self.create_and_upload_sdf_image(
            vk::Format::R32_UINT,
            &[(
                vk::Extent3D {
                    width: ix,
                    height: iy,
                    depth: iz,
                },
                bytemuck::cast_slice(&sdf.indirection),
            )],
        ) {
            Ok(parts) => parts,
            Err(err) => {
                // SAFETY: the ash/VMA seam. The atlas image+view were created above and not
                // yet owned by a `GpuSdf`; free them once on this error path.
                unsafe {
                    self.raw().destroy_image_view(atlas_view, None);
                }
                self.destroy_image(atlas_image, atlas_alloc);
                return Err(err);
            }
        };
        let (indirection_image, indirection_view, indirection_alloc) = indir;

        let [cx, cy, cz] = h.coverage_dims;
        let coverage = match self.create_and_upload_sdf_image(
            vk::Format::R16_SNORM,
            &[(
                vk::Extent3D {
                    width: cx,
                    height: cy,
                    depth: cz,
                },
                bytemuck::cast_slice(&sdf.coverage),
            )],
        ) {
            Ok(parts) => parts,
            Err(err) => {
                // SAFETY: the ash/VMA seam. The atlas + indirection were created above and not
                // yet owned by a `GpuSdf`; free both once on this error path.
                unsafe {
                    let raw = self.raw();
                    raw.destroy_image_view(atlas_view, None);
                    raw.destroy_image_view(indirection_view, None);
                }
                self.destroy_image(atlas_image, atlas_alloc);
                self.destroy_image(indirection_image, indirection_alloc);
                return Err(err);
            }
        };
        let (coverage_image, coverage_view, coverage_alloc) = coverage;

        let Some(index) = descriptors.claim_sdf_slot() else {
            tracing::warn!(
                "SDF bindless array full ({}), field skipped",
                descriptors.sdf_capacity()
            );
            // SAFETY: the ash/VMA seam. All three images+views were created above and not
            // yet owned by a `GpuSdf`; free them once on this array-full path.
            unsafe {
                let raw = self.raw();
                raw.destroy_image_view(atlas_view, None);
                raw.destroy_image_view(indirection_view, None);
                raw.destroy_image_view(coverage_view, None);
            }
            self.destroy_image(atlas_image, atlas_alloc);
            self.destroy_image(indirection_image, indirection_alloc);
            self.destroy_image(coverage_image, coverage_alloc);
            return Err(Error::BindlessFull("per-mesh SDF"));
        };
        descriptors.write_sdf_texture(atlas_view, indirection_view, coverage_view, index);

        let field = GpuSdf::from_parts(
            &self.resources,
            GpuSdfParts {
                atlas_image,
                atlas_view,
                atlas_alloc,
                indirection_image,
                indirection_view,
                indirection_alloc,
                coverage_image,
                coverage_view,
                coverage_alloc,
                bindless_index: index,
                bounds_min: Vec3::from(h.bounds_min),
                bounds_max: Vec3::from(h.bounds_max),
                max_dist: h.max_dist,
                voxel_dims: h.dims,
                indirection_dims: h.indirection_dims,
                atlas_bricks: h.atlas_bricks,
                mip_count,
            },
            descriptors.sdf_free_list(),
        );
        Ok(Arc::new(field))
    }

    /// Creates a device-local sampled 3D image of `format` with `mips.len()` prefiltered mip
    /// levels, uploads each level's bytes into its subresource through one staging copy, and
    /// leaves every level `SHADER_READ_ONLY_OPTIMAL`, returning the image, its `TYPE_3D` view
    /// (spanning all levels), and allocation. The shared shape of the SDST v3 atlas (3 mips),
    /// indirection (1 mip), and coverage (1 mip) uploads — each `mips` entry is one level's
    /// `(extent, bytes)`, finest first.
    fn create_and_upload_sdf_image(
        &self,
        format: vk::Format,
        mips: &[(vk::Extent3D, &[u8])],
    ) -> Result<(vk::Image, vk::ImageView, vk_mem::Allocation)> {
        let base_extent = mips[0].0;
        let mip_levels = mips.len() as u32;

        // One staging buffer holds every mip level's bytes, concatenated; record one
        // buffer→image copy per level at its byte offset and mip subresource.
        let total: usize = mips.iter().map(|(_, b)| b.len()).sum();
        let mut staging = StagingBuffer::new(self.allocator(), (total as vk::DeviceSize).max(4))?;
        let mut offsets: Vec<vk::DeviceSize> = Vec::with_capacity(mips.len());
        {
            let dst = staging.mapped_slice();
            let mut cursor = 0usize;
            for (_, bytes) in mips {
                offsets.push(cursor as vk::DeviceSize);
                dst[cursor..cursor + bytes.len()].copy_from_slice(bytes);
                cursor += bytes.len();
            }
        }
        staging.flush();

        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_3D)
            .format(format)
            .extent(base_extent)
            .mip_levels(mip_levels)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the image is owned by the caller
        // (the returned `GpuSdf`, or freed on a later failure).
        let (image, allocation) = checked(
            unsafe { self.allocator().create_image(&info, &alloc_info) },
            "vmaCreateImage (sdf)",
        )?;

        let recorded = self.with_one_off_commands("create_and_upload_sdf_image", |cmd| {
            // SAFETY: the ash seam. The image/staging buffer outlive the submit-wait.
            unsafe {
                let raw = self.raw();
                transition_image(
                    raw,
                    cmd,
                    image,
                    mip_levels,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                for (level, ((extent, _), &offset)) in mips.iter().zip(offsets.iter()).enumerate() {
                    let region = vk::BufferImageCopy::default()
                        .buffer_offset(offset)
                        .image_subresource(vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: level as u32,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .image_extent(*extent);
                    raw.cmd_copy_buffer_to_image(
                        cmd,
                        staging.handle(),
                        image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &[region],
                    );
                }
                transition_image(
                    raw,
                    cmd,
                    image,
                    mip_levels,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER
                        | vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
            }
        });
        drop(staging);
        if let Err(err) = recorded {
            self.destroy_image(image, allocation);
            return Err(err);
        }

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_3D)
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: mip_levels,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the 3D image just uploaded.
        let view = match unsafe { self.raw().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                self.destroy_image(image, allocation);
                return Err(Error::Vk {
                    context: "create_image_view (sdf)",
                    result,
                });
            }
        };
        Ok((image, view, allocation))
    }

    /// Uploads a creative look-up table — `size³` red-fastest `[r, g, b]` triples — as an
    /// `R16G16B16A16_SFLOAT` `TYPE_3D` sampled image (`SAMPLED | TRANSFER_DST`, clamp addressed by the
    /// linear sampler at bind), returning the [`GpuLut`] the tonemap pass binds at set 0 binding 2. The
    /// half-float format removes banding on smooth skies/skin for a trivial VRAM cost and keeps the
    /// bake round-trip clean.
    ///
    /// # Errors
    ///
    /// [`Error::ZeroSizedImage`] for a zero size, or [`Error::Vk`] for a failing Vulkan/VMA call.
    pub fn upload_lut_3d(&self, rgb: &[[f32; 3]], size: u32) -> Result<Arc<GpuLut>> {
        if size == 0 {
            return Err(Error::ZeroSizedImage);
        }
        let count = (size as usize).pow(3);
        // RGBA16F, alpha = 1; narrow each channel to f16 with the same rounding the GPU produces.
        let mut half: Vec<u16> = Vec::with_capacity(count * 4);
        for texel in rgb.iter().take(count) {
            half.push(float_to_half(texel[0]));
            half.push(float_to_half(texel[1]));
            half.push(float_to_half(texel[2]));
            half.push(float_to_half(1.0));
        }
        // A short source pads to neutral opaque black rather than reading uninitialized bytes.
        half.resize(count * 4, float_to_half(0.0));
        let (image, view, allocation) = self.create_lut_3d_image(size, &half)?;
        Ok(Arc::new(GpuLut::from_parts(
            &self.resources,
            image,
            view,
            allocation,
            size,
        )))
    }

    /// Bakes the folded look — grade + view transform + creative LUT — into a `size³` display-referred
    /// table over the log2 shaper, on the GPU, and reads it back as red-fastest `[r, g, b]` f16 bits
    /// (alpha dropped). `pipeline` is [`crate::Pipelines::request_lut_bake`]; `grade` the frozen grade
    /// uniform (its `look` block carries the creative-LUT intensity + size); `creative_lut_view` the
    /// bound creative LUT (the identity default when none); `mode` the view/display transform. The
    /// transient output image, UBO, descriptor set, and readback buffer live only for this call.
    ///
    /// # Errors
    ///
    /// [`Error::Vk`] for a failing allocation/dispatch/readback.
    #[allow(clippy::too_many_arguments)]
    pub fn bake_look_lut(
        &self,
        descriptors: &Descriptors,
        pipeline: &Pipeline,
        grade: &GradeUniform,
        creative_lut_view: vk::ImageView,
        sampler: vk::Sampler,
        size: u32,
        mode: u32,
    ) -> Result<Vec<[u16; 3]>> {
        let extent = vk::Extent3D {
            width: size,
            height: size,
            depth: size,
        };
        let cell_count = (size as usize).pow(3);

        // The baked output: an rgba16f storage 3D image, read back after the dispatch.
        let output = Image3D::new(
            &self.resources,
            extent,
            vk::Format::R16G16B16A16_SFLOAT,
            1,
            vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;

        // The frozen grade uniform, host-visible so the bake dispatch reads this exact grade.
        let range = size_of::<GradeUniform>() as vk::DeviceSize;
        let mut ubo = crate::Buffer::new(
            &self.resources,
            range,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;
        ubo.mapped_bytes()
            .expect("bake grade UBO is MAPPED")
            .copy_from_slice(bytemuck::bytes_of(grade));

        // Host-readable destination for the baked volume (rgba16f → four u16 per cell).
        let read_bytes = (cell_count * 8) as vk::DeviceSize;
        let allocator = self.allocator();
        let read_alloc = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferHost,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let read_info = vk::BufferCreateInfo::default()
            .size(read_bytes.max(4))
            .usage(vk::BufferUsageFlags::TRANSFER_DST);
        // SAFETY: the VMA seam. Owned + freed below after the readback.
        let (read_buf, mut read_allocation) = checked(
            unsafe { allocator.create_buffer(&read_info, &read_alloc) },
            "vmaCreateBuffer (lut bake readback)",
        )?;

        let set = match descriptors.allocate_set(descriptors.tonemap_set_layout()) {
            Ok(set) => set,
            Err(err) => {
                // SAFETY: the VMA seam. Free the readback buffer once before the image/UBO drop.
                unsafe { allocator.destroy_buffer(read_buf, &mut read_allocation) };
                return Err(err);
            }
        };
        // binding 0: the storage output (GENERAL); binding 2: the creative LUT (SHADER_READ_ONLY).
        let out_info = [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: output.view(),
            image_layout: vk::ImageLayout::GENERAL,
        }];
        let lut_info = [vk::DescriptorImageInfo {
            sampler,
            image_view: creative_lut_view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&out_info),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&lut_info),
        ];
        // SAFETY: the ash seam. The set + views outlive the call; single-threaded here.
        unsafe { self.raw().update_descriptor_sets(&writes, &[]) };
        descriptors.write_dynamic_uniform_buffer(set, 1, ubo.handle(), range);

        let push: [u32; 2] = [size, mode];
        let groups = size.div_ceil(4);
        let handle = pipeline.handle();
        let layout = pipeline.layout();

        let recorded = self.with_one_off_commands("bake_look_lut", |cmd| {
            // SAFETY: the ash seam. Every resource outlives the submit-wait.
            unsafe {
                let raw = self.raw();
                transition_image(
                    raw,
                    cmd,
                    output.handle(),
                    1,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::GENERAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_STORAGE_WRITE,
                );
                raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                raw.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &[set],
                    &[0],
                );
                raw.cmd_push_constants(
                    cmd,
                    layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&push),
                );
                raw.cmd_dispatch(cmd, groups, groups, groups);
                transition_image(
                    raw,
                    cmd,
                    output.handle(),
                    1,
                    vk::ImageLayout::GENERAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_STORAGE_WRITE,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(extent);
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    output.handle(),
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    read_buf,
                    &[region],
                );
            }
        });

        // SAFETY: the ash seam. Free the descriptor set (the submit completed or never ran).
        unsafe {
            let _ = self
                .raw()
                .free_descriptor_sets(descriptors.descriptor_pool(), &[set]);
        }

        let result = recorded.map(|()| {
            // SAFETY: the VMA seam. Make the GPU writes host-visible, then read four u16 per cell and
            // keep the rgb (alpha dropped).
            unsafe {
                let _ = allocator.invalidate_allocation(&read_allocation, 0, read_bytes);
                let ptr = allocator.get_allocation_info(&read_allocation).mapped_data as *const u16;
                let words = std::slice::from_raw_parts(ptr, cell_count * 4);
                words
                    .chunks_exact(4)
                    .map(|c| [c[0], c[1], c[2]])
                    .collect::<Vec<[u16; 3]>>()
            }
        });

        // SAFETY: the VMA seam. Free the readback buffer once; the image + UBO Drop after.
        unsafe { allocator.destroy_buffer(read_buf, &mut read_allocation) };
        result
    }

    /// Uploads the neutral identity LUT — a `2×2×2` ramp whose corner `i` is the RGB of that corner, so
    /// a tetrahedral sample of `c ∈ [0,1]` returns `c` unchanged. The always-bound default at binding 2
    /// when no creative look is assigned, so the tonemap shader never branches on presence.
    ///
    /// # Errors
    ///
    /// [`Error::Vk`] for a failing Vulkan/VMA call.
    pub fn upload_identity_lut(&self) -> Result<Arc<GpuLut>> {
        let mut rgb = Vec::with_capacity(8);
        for z in 0..2u32 {
            for y in 0..2u32 {
                for x in 0..2u32 {
                    rgb.push([x as f32, y as f32, z as f32]);
                }
            }
        }
        self.upload_lut_3d(&rgb, 2)
    }

    /// Creates an `R16G16B16A16_SFLOAT` `TYPE_3D` image of `size³` and uploads `half` (RGBA f16 bytes,
    /// red-fastest), leaving it `SHADER_READ_ONLY_OPTIMAL`. The shared body of
    /// [`Self::upload_lut_3d`] and the bake's readback source allocation.
    fn create_lut_3d_image(
        &self,
        size: u32,
        half: &[u16],
    ) -> Result<(vk::Image, vk::ImageView, vk_mem::Allocation)> {
        let extent = vk::Extent3D {
            width: size,
            height: size,
            depth: size,
        };
        let bytes = std::mem::size_of_val(half) as vk::DeviceSize;
        let mut staging = StagingBuffer::new(self.allocator(), bytes.max(4))?;
        staging.mapped_slice()[..bytes as usize].copy_from_slice(bytemuck::cast_slice(half));
        staging.flush();

        let format = vk::Format::R16G16B16A16_SFLOAT;
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_3D)
            .format(format)
            .extent(extent)
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the image is owned by the caller (the
        // returned `GpuLut`, or freed on a later failure).
        let (image, allocation) = checked(
            unsafe { self.allocator().create_image(&info, &alloc_info) },
            "vmaCreateImage (lut)",
        )?;

        let recorded = self.with_one_off_commands("create_lut_3d_image", |cmd| {
            // SAFETY: the ash seam. The image/staging buffer outlive the submit-wait.
            unsafe {
                let raw = self.raw();
                transition_image(
                    raw,
                    cmd,
                    image,
                    1,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(extent);
                raw.cmd_copy_buffer_to_image(
                    cmd,
                    staging.handle(),
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
                transition_image(
                    raw,
                    cmd,
                    image,
                    1,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER
                        | vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
            }
        });
        drop(staging);
        if let Err(err) = recorded {
            self.destroy_image(image, allocation);
            return Err(err);
        }

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_3D)
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the 3D image just uploaded.
        let view = match unsafe { self.raw().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                self.destroy_image(image, allocation);
                return Err(Error::Vk {
                    context: "create_image_view (lut)",
                    result,
                });
            }
        };
        Ok((image, view, allocation))
    }

    /// Returns a mesh's signed distance fields (one tight field per primitive / spatial chunk)
    /// for `positions`/`indices`/`submeshes`: a sidecar-cache hit
    /// (`<cache_dir>/<meshHash>.sdfset`) when present, else a fresh GPU jump-flood
    /// [`Uploader::bake_sdf`] whose bytes are written back to the cache. The content hash keys
    /// the cache on the geometry, so a touched-but-unchanged mesh reuses its bake.
    ///
    /// # Errors
    ///
    /// [`Error::SdfBake`] when no bake pipelines are present, or [`Error::Vk`] from the bake
    /// dispatch. A malformed sidecar is ignored (re-baked), not an error.
    pub fn bake_or_load_sdf(
        &self,
        positions: &[Vec3],
        indices: &[u32],
        submeshes: &[Submesh],
        bake: &SdfBake,
    ) -> Result<Vec<Sdf>> {
        let hash = mesh_content_hash(positions, indices, bake.resolution_scale);
        let cache_path = bake
            .cache_dir
            .as_ref()
            .map(|dir| dir.join(format!("{hash:016x}.sdfset")));

        if let Some(path) = cache_path.as_ref()
            && let Ok(bytes) = std::fs::read(path)
        {
            match sdf_set_from_bytes(&bytes) {
                Ok(fields) => {
                    tracing::debug!(%hash, fields = fields.len(), "SDF sidecar cache hit");
                    return Ok(fields);
                }
                Err(err) => tracing::warn!(%hash, "SDF sidecar unreadable, re-baking: {err}"),
            }
        }

        let fields = self.bake_sdf(positions, indices, submeshes, bake.resolution_scale)?;
        if let Some(path) = cache_path.as_ref() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(err) = std::fs::write(path, sdf_set_to_bytes(&fields)) {
                tracing::warn!(%hash, "writing SDF sidecar failed: {err}");
            }
        }
        Ok(fields)
    }

    /// GPU jump-flood bakes a mesh's signed distance fields — the whole mesh partitioned once
    /// into a bounded grid of spatial cells ([`sdf_chunk_cores`]), one fine field per non-empty
    /// cell. A merged mesh's geometry is often batched by material into scene-spanning
    /// primitives, so following primitive boundaries would not localize a field; a spatial
    /// partition of the whole mesh gives each cell its own tight field regardless of how the
    /// source batched geometry — the modular decomposition (many small fields with tight AABBs,
    /// combined by `min`-over-instances) that resolves indoor GI. Each cell derives its grid
    /// from its bounds and bakes through [`Uploader::bake_region`], culling the mesh's triangles
    /// to the cell; a cell that baked no occupied brick (empty air) is dropped. Runs in ms per
    /// cell on real hardware — no import freeze. Timed by a `tracing` span.
    ///
    /// # Errors
    ///
    /// [`Error::SdfBake`] when the bake pipelines are absent; [`Error::Vk`] for a failing
    /// dispatch / allocation.
    pub fn bake_sdf(
        &self,
        positions: &[Vec3],
        indices: &[u32],
        submeshes: &[Submesh],
        resolution_scale: f32,
    ) -> Result<Vec<Sdf>> {
        let bake = self
            .bake
            .as_ref()
            .ok_or_else(|| Error::SdfBake("no bake pipelines (missing shaders)".to_owned()))?;

        // The whole mesh's global triangle list (every primitive), degenerate slivers dropped up
        // front as the `MeshBvh` oracle does: a sliver carries no surface and its unstable normal
        // would poison the sign cross-check. Submesh indices are primitive-local — Vulkan adds
        // `vertex_offset` at fetch — so the global vertex is `vertex_offset + index`. A mesh with
        // no explicit submeshes (a single-primitive source) is one range at offset 0.
        let whole = [Submesh {
            first_index: 0,
            index_count: indices.len() as u32,
            vertex_offset: 0,
            material_slot: 0,
        }];
        let prims: &[Submesh] = if submeshes.is_empty() {
            &whole
        } else {
            submeshes
        };

        let mut tris: Vec<u32> = Vec::with_capacity(indices.len());
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for prim in prims {
            let base = prim.first_index as usize;
            let end = base + prim.index_count as usize;
            let range = indices.get(base..end).unwrap_or(&[]);
            for t in range.chunks_exact(3) {
                let g = [
                    (prim.vertex_offset as u32).wrapping_add(t[0]),
                    (prim.vertex_offset as u32).wrapping_add(t[1]),
                    (prim.vertex_offset as u32).wrapping_add(t[2]),
                ];
                let (a, b, c) = (
                    positions[g[0] as usize],
                    positions[g[1] as usize],
                    positions[g[2] as usize],
                );
                let (e1, e2) = (b - a, c - a);
                if e1.cross(e2).length_squared()
                    <= 1e-10 * e1.length_squared() * e2.length_squared()
                {
                    continue;
                }
                tris.extend_from_slice(&g);
                lo = lo.min(a.min(b).min(c));
                hi = hi.max(a.max(b).max(c));
            }
        }
        if tris.len() < 3 {
            return Ok(Vec::new());
        }

        // Signed volume orients the nearest-triangle-normal sign so an inward-wound source still
        // reads negative inside (mirrors `MeshBvh`). Taken over the whole mesh and shared across
        // its spatial cells — a cell is not a closed volume, so its own signed volume is
        // meaningless.
        let mut vol6 = 0.0f64;
        for t in tris.chunks_exact(3) {
            let a = positions[t[0] as usize];
            let b = positions[t[1] as usize];
            let c = positions[t[2] as usize];
            vol6 += f64::from(a.dot(b.cross(c)));
        }
        let winding = if vol6 >= 0.0 { 1.0 } else { -1.0 };

        let cells = sdf_chunk_cores(lo, hi, resolution_scale);
        let span = tracing::info_span!("sdf_bake", cells = cells.len()).entered();
        let started = std::time::Instant::now();
        let mut fields = Vec::new();
        for (c0, c1) in cells {
            let grid = bake_grid(c0, c1, resolution_scale);
            if let Some(sdf) = self.bake_region(bake, positions, &tris, &grid, winding)? {
                fields.push(sdf);
            }
        }

        drop(span);
        tracing::debug!(
            fields = fields.len(),
            ms = started.elapsed().as_secs_f32() * 1000.0,
            "SDF GPU bake complete"
        );
        Ok(fields)
    }

    /// Bakes one SDF region over the derived `grid`: keeps only the triangles whose AABB
    /// overlaps the padded grid bounds (compacting their vertices so the geometry upload stays
    /// small), then voxelize → jump-flood → sign → compact.
    ///
    /// The GPU owns the nearest-surface search (voxelize + jump-flood) and reads back a
    /// per-voxel seed (closest surface point + packed nearest-triangle normal); the sign is
    /// resolved on the host by an outside-flood cross-checked against that normal — a
    /// connected-component fill, exact and cheap, that (unlike a single face normal) does not
    /// read a concave-corner interior voxel as outside. Returns `None` when the region covers
    /// empty air (every brick saturated to `+max`).
    ///
    /// # Errors
    ///
    /// [`Error::Vk`] for a failing bake dispatch / allocation.
    fn bake_region(
        &self,
        bake: &BakePipelines,
        positions: &[Vec3],
        tris: &[u32],
        grid: &GridDesc,
        winding: f32,
    ) -> Result<Option<Sdf>> {
        let (local_positions, local_indices) =
            compact_tris_in_bounds(positions, tris, grid.bounds_min, grid.bounds_max);
        if local_indices.len() < 3 {
            return Ok(None);
        }
        let seeds = self.run_bake_passes(bake, grid, &local_positions, &local_indices)?;
        let dense = sign_field(grid, winding, &seeds);
        let sdf = Sdf::from_dense_field(grid, &dense);
        if sdf.header.occupied_bricks == 0 {
            return Ok(None);
        }
        Ok(Some(sdf))
    }

    /// Records and submits the voxelize → JFA dispatches for `grid` over the mesh geometry,
    /// then reads the final jump-flood seed volume (closest surface point in `xyz`, the
    /// octahedral-packed nearest-triangle normal in `w`, X-fastest) back to the host for the
    /// CPU sign pass. The transient seed 3D images + the geometry storage buffers live only
    /// for this call.
    fn run_bake_passes(
        &self,
        bake: &BakePipelines,
        grid: &GridDesc,
        positions: &[Vec3],
        indices: &[u32],
    ) -> Result<Vec<[f32; 4]>> {
        let [nx, ny, nz] = grid.dims;
        let extent = vk::Extent3D {
            width: nx,
            height: ny,
            depth: nz,
        };
        let voxel_count = nx as usize * ny as usize * nz as usize;
        let tri_count = (indices.len() / 3) as u32;
        let cell = grid.cell();

        // Geometry storage buffers (positions as float4, the index stream), staged + copied.
        let pos4: Vec<[f32; 4]> = positions.iter().map(|p| [p.x, p.y, p.z, 0.0]).collect();
        let pos_bytes = std::mem::size_of_val(pos4.as_slice()) as vk::DeviceSize;
        let idx_bytes = std::mem::size_of_val(indices) as vk::DeviceSize;
        let mut staging = StagingBuffer::new(self.allocator(), pos_bytes + idx_bytes)?;
        {
            let dst = staging.mapped_slice();
            dst[..pos_bytes as usize].copy_from_slice(bytemuck::cast_slice(&pos4));
            dst[pos_bytes as usize..(pos_bytes + idx_bytes) as usize]
                .copy_from_slice(bytemuck::cast_slice(indices));
        }
        staging.flush();
        let allocator = self.allocator();
        let pos_buf =
            make_device_buffer(allocator, pos_bytes, vk::BufferUsageFlags::STORAGE_BUFFER)?;
        let idx_buf =
            match make_device_buffer(allocator, idx_bytes, vk::BufferUsageFlags::STORAGE_BUFFER) {
                Ok(buf) => buf,
                Err(err) => {
                    free_one(allocator, pos_buf);
                    return Err(err);
                }
            };

        // Transient seed / work images (storage; both seed buffers are also a transfer src,
        // since the final jump-flood result lands in A or B by parity and is read back).
        // RAII so an early `?` frees them.
        let bake_images = (|| -> Result<BakeImages> {
            let seed_key = Image3D::new(
                &self.resources,
                extent,
                vk::Format::R32_UINT,
                1,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_DST,
            )?;
            let seed_a = Image3D::new(
                &self.resources,
                extent,
                vk::Format::R32G32B32A32_SFLOAT,
                1,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC,
            )?;
            let seed_b = Image3D::new(
                &self.resources,
                extent,
                vk::Format::R32G32B32A32_SFLOAT,
                1,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC,
            )?;
            Ok(BakeImages {
                seed_key,
                seed_a,
                seed_b,
            })
        })();
        let bake_images = match bake_images {
            Ok(images) => images,
            Err(err) => {
                free_one(allocator, pos_buf);
                free_one(allocator, idx_buf);
                return Err(err);
            }
        };

        // Host-readable destination for the final seed volume (RGBA32F → four f32 per voxel).
        let read_bytes = (voxel_count * 16) as vk::DeviceSize;
        let read_alloc = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferHost,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let read_info = vk::BufferCreateInfo::default()
            .size(read_bytes)
            .usage(vk::BufferUsageFlags::TRANSFER_DST);
        // SAFETY: the VMA seam. Owned + freed below after the readback.
        let (read_buf, mut read_allocation) = match checked(
            unsafe { allocator.create_buffer(&read_info, &read_alloc) },
            "vmaCreateBuffer (sdf readback)",
        ) {
            Ok(parts) => parts,
            Err(err) => {
                free_one(allocator, pos_buf);
                free_one(allocator, idx_buf);
                return Err(err);
            }
        };

        // Allocate + write the bake descriptor set (the 6 resources). Freed before return.
        let set = match bake.allocate_set(self.raw()) {
            Ok(set) => set,
            Err(err) => {
                // SAFETY: the VMA seam. The readback buffer + geometry buffers are freed
                // once on this error path before the images drop.
                unsafe { allocator.destroy_buffer(read_buf, &mut read_allocation) };
                free_one(allocator, pos_buf);
                free_one(allocator, idx_buf);
                return Err(err);
            }
        };
        bake.write_set(self.raw(), set, pos_buf.0, idx_buf.0, &bake_images);

        let max_dim = nx.max(ny).max(nz);
        let mut steps: Vec<i32> = Vec::new();
        let mut step = (max_dim.next_power_of_two() / 2).max(1);
        loop {
            steps.push(step as i32);
            if step == 1 {
                break;
            }
            step /= 2;
        }
        // After init (writes A), each prop step alternates the result buffer B,A,B,A…; an
        // even step count leaves the result in A (final parity 0), odd in B (final parity 1).
        let final_parity = if steps.len().is_multiple_of(2) { 0 } else { 1 };

        let base_push = BakePush {
            dims: [nx, ny, nz, tri_count],
            bounds_min: [
                grid.bounds_min.x,
                grid.bounds_min.y,
                grid.bounds_min.z,
                grid.max_dist,
            ],
            cell: [cell.x, cell.y, cell.z, 0.0],
            misc: [0; 4],
        };

        let recorded = self.with_one_off_commands("run_bake_passes", |cmd| {
            // SAFETY: the ash seam. Every resource outlives the submit-wait below.
            unsafe {
                let raw = self.raw();
                // Stage the geometry into the device-local storage buffers the bake reads;
                // `record_bake`'s transfer→compute barrier makes these copies visible.
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    pos_buf.0,
                    &[vk::BufferCopy::default().size(pos_bytes)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    idx_buf.0,
                    &[vk::BufferCopy::default()
                        .src_offset(pos_bytes)
                        .dst_offset(0)
                        .size(idx_bytes)],
                );
                self.record_bake(cmd, bake, set, &bake_images, &base_push, &steps);
                // Copy the final jump-flood seed image (GENERAL) into the host-readable
                // buffer. The result lands in A for an even prop-step count, B for odd.
                let final_seed = if final_parity == 0 {
                    bake_images.seed_a.handle()
                } else {
                    bake_images.seed_b.handle()
                };
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(extent);
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    final_seed,
                    vk::ImageLayout::GENERAL,
                    read_buf,
                    &[region],
                );
            }
        });

        // SAFETY: the ash seam. Free the descriptor set (the submit completed or never ran).
        unsafe {
            let _ = self.raw().free_descriptor_sets(bake.pool, &[set]);
        }

        let result = recorded.and_then(|()| {
            // SAFETY: the VMA seam. Make the GPU writes host-visible, then read the seed
            // volume (four f32 per voxel: closest surface point xyz + packed normal w).
            let seeds = unsafe {
                let _ = allocator.invalidate_allocation(&read_allocation, 0, read_bytes);
                let ptr = allocator
                    .get_allocation_info(&read_allocation)
                    .mapped_data
                    .cast::<[f32; 4]>();
                if ptr.is_null() {
                    return Err(Error::SdfBake("readback buffer not mapped".to_owned()));
                }
                std::slice::from_raw_parts(ptr, voxel_count).to_vec()
            };
            Ok(seeds)
        });

        drop(staging);
        // SAFETY: the VMA seam. The readback + geometry buffers are freed exactly once after
        // the submit completed; the transient images drop with `bake_images`.
        unsafe { allocator.destroy_buffer(read_buf, &mut read_allocation) };
        free_one(allocator, pos_buf);
        free_one(allocator, idx_buf);
        drop(bake_images);
        result
    }

    /// Records the voxelize → JFA(init + propagate) dispatch sequence into `cmd` with the
    /// barriers between every storage-image write and its next read. The final seed image
    /// (A or B by parity) is left in `GENERAL` ready for the readback copy; the host resolves
    /// the sign from it.
    ///
    /// # Safety
    ///
    /// `cmd` must be recording; every resource in `images`/`set` must outlive the submit.
    unsafe fn record_bake(
        &self,
        cmd: vk::CommandBuffer,
        bake: &BakePipelines,
        set: vk::DescriptorSet,
        images: &BakeImages,
        base_push: &BakePush,
        steps: &[i32],
    ) {
        let raw = self.raw();
        // SAFETY: the caller's recording contract.
        unsafe {
            // Storage images to GENERAL. seed_key receives a transfer clear first (so its
            // barrier targets the CLEAR stage + transfer-write access); the rest are first
            // touched by the compute passes.
            let compute_rw =
                vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE;
            for (image, dst_stage, dst_access) in [
                (
                    images.seed_key.handle(),
                    vk::PipelineStageFlags2::CLEAR,
                    vk::AccessFlags2::TRANSFER_WRITE,
                ),
                (
                    images.seed_a.handle(),
                    vk::PipelineStageFlags2::COMPUTE_SHADER,
                    compute_rw,
                ),
                (
                    images.seed_b.handle(),
                    vk::PipelineStageFlags2::COMPUTE_SHADER,
                    compute_rw,
                ),
            ] {
                transition_image(
                    raw,
                    cmd,
                    image,
                    1,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::GENERAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    dst_stage,
                    dst_access,
                );
            }
            // Seed the key image with 0xFFFFFFFF (no triangle), the InterlockedMin identity.
            let clear = vk::ClearColorValue {
                uint32: [u32::MAX; 4],
            };
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            raw.cmd_clear_color_image(
                cmd,
                images.seed_key.handle(),
                vk::ImageLayout::GENERAL,
                &clear,
                &[range],
            );
            // The geometry copies + the key clear (all transfer writes) → compute reads.
            transfer_to_compute_barrier(raw, cmd);

            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                bake.pipeline_layout,
                0,
                &[set],
                &[],
            );

            let push = |cmd: vk::CommandBuffer, p: &BakePush| {
                raw.cmd_push_constants(
                    cmd,
                    bake.pipeline_layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(p),
                );
            };
            let groups = |n: u32, local: u32| n.div_ceil(local);
            let voxel_groups = (
                groups(base_push.dims[0], 4),
                groups(base_push.dims[1], 4),
                groups(base_push.dims[2], 4),
            );

            // Voxelize: one thread per triangle.
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, bake.voxelize);
            let mut vox = *base_push;
            vox.misc = [0; 4];
            push(cmd, &vox);
            raw.cmd_dispatch(cmd, groups(base_push.dims[3], 64).max(1), 1, 1);
            compute_to_compute_barrier(raw, cmd);

            // JFA init: decode the winning triangle per voxel into seed buffer A.
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, bake.jfa);
            let mut init = *base_push;
            init.misc = [0, 0, 1, 0];
            push(cmd, &init);
            raw.cmd_dispatch(cmd, voxel_groups.0, voxel_groups.1, voxel_groups.2);
            compute_to_compute_barrier(raw, cmd);

            // JFA propagate: step N/2 … 1, ping-ponging A/B by parity.
            let mut parity = 0i32;
            let last = steps.len().saturating_sub(1);
            for (i, &s) in steps.iter().enumerate() {
                let mut prop = *base_push;
                prop.misc = [s, parity, 0, 0];
                push(cmd, &prop);
                raw.cmd_dispatch(cmd, voxel_groups.0, voxel_groups.1, voxel_groups.2);
                if i < last {
                    compute_to_compute_barrier(raw, cmd);
                }
                parity ^= 1;
            }

            // The final jump-flood seed write → the readback transfer copy.
            compute_to_transfer_barrier(raw, cmd);
        }
    }

    /// Builds the device-local morph buffers from [`MorphData`]: the flat `MorphDelta`
    /// array (each target's deltas concatenated) and the per-target `[first_delta,
    /// delta_count]` ranges, both `STORAGE` for the morph compute pass. Frees the delta
    /// buffer if the range buffer or the copy fails.
    fn upload_morph_buffers(&self, data: &MorphData) -> Result<MorphBuffers> {
        let mut deltas: Vec<MorphDelta> = Vec::new();
        let mut ranges: Vec<[u32; 2]> = Vec::with_capacity(data.targets.len());
        for target in &data.targets {
            let first = deltas.len() as u32;
            ranges.push([first, target.deltas.len() as u32]);
            deltas.extend_from_slice(&target.deltas);
        }
        let delta_count = deltas.len() as u32;
        let target_count = ranges.len() as u32;

        // Each device buffer must be non-empty even when a count is zero (a rest-only morph
        // mesh); pad to one record. The shader never reads past the real counts.
        let delta_used = std::mem::size_of_val(deltas.as_slice());
        let range_used = std::mem::size_of_val(ranges.as_slice());
        let delta_bytes = delta_used.max(std::mem::size_of::<MorphDelta>()) as vk::DeviceSize;
        let range_bytes = range_used.max(std::mem::size_of::<[u32; 2]>()) as vk::DeviceSize;

        let mut staging = StagingBuffer::new(self.allocator(), delta_bytes + range_bytes)?;
        {
            let bytes = staging.mapped_slice();
            if delta_used > 0 {
                bytes[..delta_used].copy_from_slice(bytemuck::cast_slice(&deltas));
            }
            if range_used > 0 {
                let base = delta_bytes as usize;
                bytes[base..base + range_used].copy_from_slice(bytemuck::cast_slice(&ranges));
            }
        }
        staging.flush();

        let allocator = self.allocator();
        let deltas_buf =
            make_device_buffer(allocator, delta_bytes, vk::BufferUsageFlags::STORAGE_BUFFER)?;
        let ranges_buf = match make_device_buffer(
            allocator,
            range_bytes,
            vk::BufferUsageFlags::STORAGE_BUFFER,
        ) {
            Ok(buf) => buf,
            Err(err) => {
                free_one(allocator, deltas_buf);
                return Err(err);
            }
        };

        let copy = self.with_one_off_commands("upload_morph_buffers", |cmd| {
            // SAFETY: the ash seam. Both device buffers outlive the submit-wait; the staging
            // buffer is the source.
            unsafe {
                let raw = self.raw();
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    deltas_buf.0,
                    &[vk::BufferCopy::default().size(delta_bytes)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    ranges_buf.0,
                    &[vk::BufferCopy::default()
                        .src_offset(delta_bytes)
                        .dst_offset(0)
                        .size(range_bytes)],
                );
            }
        });
        drop(staging);
        if let Err(err) = copy {
            free_one(allocator, deltas_buf);
            free_one(allocator, ranges_buf);
            return Err(err);
        }

        Ok(MorphBuffers {
            deltas: deltas_buf,
            ranges: ranges_buf,
            cpu_ranges: ranges,
            target_count,
            delta_count,
        })
    }

    /// Builds the watertight-conditioning data ([`MeshConditioning`], a pure function of the mesh, as
    /// the cook's clusterer is) and uploads its four arrays as device-local `STORAGE_BUFFER`s: the
    /// unique edges, the per-triangle edge indices, the per-welded-vertex basis, and the base→welded
    /// map. One staging buffer with four contiguous slices, one `cmd_copy_buffer` per slice.
    /// Non-fatal on failure (the mesh then carries no conditioning).
    fn upload_conditioning_buffers(&self, mesh: &Mesh) -> Result<Option<ConditioningBuffers>> {
        if mesh.vertices.is_empty() {
            return Ok(None);
        }
        let cond = MeshConditioning::build(mesh);

        let edges_bytes = std::mem::size_of_val(cond.edges.as_slice());
        let tri_bytes = std::mem::size_of_val(cond.tri_edges.as_slice());
        let welded_bytes = std::mem::size_of_val(cond.welded.as_slice());
        let weld_id_bytes = std::mem::size_of_val(cond.weld_id.as_slice());
        // Grow-to-stride so an empty array (a mesh with no triangles) still backs a valid buffer.
        let edges_size = edges_bytes.max(16) as vk::DeviceSize;
        let tri_size = tri_bytes.max(16) as vk::DeviceSize;
        let welded_size =
            welded_bytes.max(size_of::<saffron_geometry::WeldedVertex>()) as vk::DeviceSize;
        let weld_id_size = weld_id_bytes.max(4) as vk::DeviceSize;

        let mut staging = StagingBuffer::new(
            self.allocator(),
            edges_size + tri_size + welded_size + weld_id_size,
        )?;
        {
            let bytes = staging.mapped_slice();
            let (e, t, w) = (edges_size as usize, tri_size as usize, welded_size as usize);
            if edges_bytes > 0 {
                bytes[..edges_bytes].copy_from_slice(bytemuck::cast_slice(&cond.edges));
            }
            if tri_bytes > 0 {
                bytes[e..e + tri_bytes].copy_from_slice(bytemuck::cast_slice(&cond.tri_edges));
            }
            if welded_bytes > 0 {
                bytes[e + t..e + t + welded_bytes]
                    .copy_from_slice(bytemuck::cast_slice(&cond.welded));
            }
            if weld_id_bytes > 0 {
                bytes[e + t + w..e + t + w + weld_id_bytes]
                    .copy_from_slice(bytemuck::cast_slice(&cond.weld_id));
            }
        }
        staging.flush();

        let allocator = self.allocator();
        let usage = vk::BufferUsageFlags::STORAGE_BUFFER;
        let edges_buf = make_device_buffer(allocator, edges_size, usage)?;
        let tri_buf = match make_device_buffer(allocator, tri_size, usage) {
            Ok(buf) => buf,
            Err(err) => {
                free_one(allocator, edges_buf);
                return Err(err);
            }
        };
        let welded_buf = match make_device_buffer(allocator, welded_size, usage) {
            Ok(buf) => buf,
            Err(err) => {
                free_one(allocator, edges_buf);
                free_one(allocator, tri_buf);
                return Err(err);
            }
        };
        let weld_id_buf = match make_device_buffer(allocator, weld_id_size, usage) {
            Ok(buf) => buf,
            Err(err) => {
                free_one(allocator, edges_buf);
                free_one(allocator, tri_buf);
                free_one(allocator, welded_buf);
                return Err(err);
            }
        };

        let copy = self.with_one_off_commands("upload_conditioning_buffers", |cmd| {
            // SAFETY: the ash seam. All four device buffers outlive the submit-wait; the staging
            // buffer is the source for each contiguous slice.
            unsafe {
                let raw = self.raw();
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    edges_buf.0,
                    &[vk::BufferCopy::default().size(edges_size)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    tri_buf.0,
                    &[vk::BufferCopy::default()
                        .src_offset(edges_size)
                        .size(tri_size)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    welded_buf.0,
                    &[vk::BufferCopy::default()
                        .src_offset(edges_size + tri_size)
                        .size(welded_size)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    weld_id_buf.0,
                    &[vk::BufferCopy::default()
                        .src_offset(edges_size + tri_size + welded_size)
                        .size(weld_id_size)],
                );
            }
        });
        drop(staging);
        if let Err(err) = copy {
            free_one(allocator, edges_buf);
            free_one(allocator, tri_buf);
            free_one(allocator, welded_buf);
            free_one(allocator, weld_id_buf);
            return Err(err);
        }

        Ok(Some(ConditioningBuffers {
            edges: edges_buf,
            tri_edges: tri_buf,
            welded: welded_buf,
            weld_id: weld_id_buf,
            edge_count: cond.edges.len() as u32,
            welded_count: cond.welded.len() as u32,
        }))
    }

    /// Uploads tightly packed RGBA8 pixels as a sampled, mipmapped texture in the
    /// bindless array, claiming a slot in `descriptors` and writing the view into it.
    ///
    /// `srgb` selects `R8G8B8A8_SRGB` (color) vs `R8G8B8A8_UNORM` (data). A full mip
    /// chain is generated by blitting down from mip 0.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ZeroSizedImage`] for a zero extent or [`Error::Vk`] for a
    /// failing Vulkan/VMA call; allocated resources are freed before return on error.
    pub fn upload_texture(
        &self,
        descriptors: &Descriptors,
        rgba: &[u8],
        width: u32,
        height: u32,
        srgb: bool,
    ) -> Result<Arc<GpuTexture>> {
        if width == 0 || height == 0 {
            return Err(Error::ZeroSizedImage);
        }
        let bytes = (width as vk::DeviceSize) * (height as vk::DeviceSize) * 4;

        let mut staging = StagingBuffer::new(self.allocator(), bytes)?;
        staging.mapped_slice()[..bytes as usize].copy_from_slice(&rgba[..bytes as usize]);
        staging.flush();

        let mip_levels = mip_count(width, height);
        let format = if srgb {
            vk::Format::R8G8B8A8_SRGB
        } else {
            vk::Format::R8G8B8A8_UNORM
        };
        let uploaded = self.create_sampled_image(width, height, mip_levels, format)?;
        let image = uploaded.image;

        // Record the upload + mip generation; on failure free the image.
        let recorded = self.with_one_off_commands("upload_texture", |cmd| {
            // SAFETY: the ash seam. The image/staging buffer outlive the submit-wait.
            unsafe {
                record_texture_upload(
                    self.raw(),
                    cmd,
                    image,
                    staging.handle(),
                    width,
                    height,
                    mip_levels,
                )
            };
        });
        drop(staging);
        if let Err(err) = recorded {
            self.destroy_image(uploaded.image, uploaded.allocation);
            return Err(err);
        }

        self.finish_texture(descriptors, uploaded, None)
    }

    /// Uploads an explicit, prefiltered RGBA8 mip chain into one bindless texture.
    ///
    /// This is the canonical coverage path: every level is CPU-derived and copied
    /// exactly, so driver blit filtering cannot change thin-sheet classification.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ZeroSizedImage`] for an empty/malformed chain or [`Error::Vk`]
    /// for a failing Vulkan/VMA operation.
    pub fn upload_texture_mips(
        &self,
        descriptors: &Descriptors,
        mips: &[TextureMipLevel<'_>],
        srgb: bool,
    ) -> Result<Arc<GpuTexture>> {
        let Some(base) = mips.first() else {
            return Err(Error::ZeroSizedImage);
        };
        if !valid_prefiltered_mip_chain(mips) {
            return Err(Error::ZeroSizedImage);
        }
        let total = mips.iter().map(|mip| mip.rgba.len()).sum::<usize>();
        let mut staging = StagingBuffer::new(self.allocator(), total as vk::DeviceSize)?;
        let mut offsets = Vec::with_capacity(mips.len());
        let mut offset = 0_usize;
        for mip in mips {
            offsets.push(offset as vk::DeviceSize);
            staging.mapped_slice()[offset..offset + mip.rgba.len()].copy_from_slice(mip.rgba);
            offset += mip.rgba.len();
        }
        staging.flush();

        let format = if srgb {
            vk::Format::R8G8B8A8_SRGB
        } else {
            vk::Format::R8G8B8A8_UNORM
        };
        let uploaded =
            self.create_sampled_image(base.width, base.height, mips.len() as u32, format)?;
        let image = uploaded.image;
        let recorded = self.with_one_off_commands("upload_texture_mips", |cmd| {
            // SAFETY: the image/staging buffer and mip slices outlive the submit-wait.
            unsafe {
                transition_image(
                    self.raw(),
                    cmd,
                    image,
                    mips.len() as u32,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                for (level, (mip, &mip_offset)) in mips.iter().zip(&offsets).enumerate() {
                    copy_buffer_to_image_mip(
                        self.raw(),
                        cmd,
                        staging.handle(),
                        image,
                        mip_offset,
                        level as u32,
                        vk::Extent2D {
                            width: mip.width,
                            height: mip.height,
                        },
                    );
                }
                transition_image(
                    self.raw(),
                    cmd,
                    image,
                    mips.len() as u32,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
            }
        });
        drop(staging);
        if let Err(error) = recorded {
            self.destroy_image(uploaded.image, uploaded.allocation);
            return Err(error);
        }
        self.finish_texture(descriptors, uploaded, None)
    }

    /// Uploads the 1×1 white RGBA8 texture and seeds it into *every* bindless slot,
    /// returning the [`GpuTexture`] the renderer holds for its lifetime.
    ///
    /// A material with no albedo/ORM texture indexes [`crate::DEFAULT_WHITE_SLOT`], so
    /// that slot must hold a valid view or sampling it faults on lavapipe and is
    /// undefined behaviour on real hardware. The white pixel makes the missing-texture
    /// factors pass through unchanged (white × factor = factor). The upload claims slot
    /// 0 (the first claim at init) via the normal path, then [`Descriptors::seed_all_textures`]
    /// fills every remaining slot so no descriptor in the partially-bound array is ever
    /// unbound.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for a failing Vulkan/VMA call during the texture upload.
    pub fn upload_default_white(&self, descriptors: &Descriptors) -> Result<Arc<GpuTexture>> {
        let white = self.upload_texture(descriptors, &[255u8, 255, 255, 255], 1, 1, false)?;
        descriptors.seed_all_textures(white.view());
        Ok(white)
    }

    /// Uploads the "empty space" default SDF (a one-brick all-`+max` SDST v2 field) and
    /// seeds it into *every* SDF bindless slot (bindings 1 + 2), returning the [`GpuSdf`] the
    /// renderer holds for its lifetime.
    ///
    /// The bindless SDF arrays are partially bound — only the slots of meshes that baked a
    /// field carry real bricks. The cone-trace and lighting übershader still declare the
    /// whole arrays, and lavapipe faults on an unbound slot even one the shader never samples
    /// (UB on real hardware). The default field's single brick is classified empty (every
    /// voxel saturates to `+max`), reading back as "far from any surface" so an instance that
    /// ever resolved to it contributes no occlusion. The upload claims slot 0 (the first
    /// claim at init), then [`Descriptors::seed_all_sdf_textures`] fills every remaining slot.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`]/[`Error::ZeroSizedImage`] for a failing Vulkan/VMA call during
    /// the SDF upload.
    pub fn upload_default_sdf(&self, descriptors: &Descriptors) -> Result<Arc<GpuSdf>> {
        let grid = GridDesc {
            dims: [8, 8, 8],
            bounds_min: Vec3::splat(-0.5),
            bounds_max: Vec3::splat(0.5),
            max_dist: 1.0,
        };
        let dense = vec![i16::MAX; 8 * 8 * 8];
        let sdf = Sdf::from_dense_field(&grid, &dense);
        let field = self.upload_sdf(descriptors, &sdf)?;
        descriptors.seed_all_sdf_textures(
            field.atlas_view(),
            field.indirection_view(),
            field.coverage_view(),
        );
        Ok(field)
    }

    /// Uploads tightly packed linear-float RGBA (`width*height*4` floats) as an
    /// `R16G16B16A16_SFLOAT` sampled texture in the bindless array, narrowing f32→f16
    /// on the CPU before staging (HDR panoramas / env sources). Single mip.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ZeroSizedImage`] for a zero extent or [`Error::Vk`] for a
    /// failing Vulkan/VMA call; allocated resources are freed before return on error.
    pub fn upload_texture_float(
        &self,
        descriptors: &Descriptors,
        rgba: &[f32],
        width: u32,
        height: u32,
    ) -> Result<Arc<GpuTexture>> {
        if width == 0 || height == 0 {
            return Err(Error::ZeroSizedImage);
        }
        let texels = (width as usize) * (height as usize) * 4;
        let half: Vec<u16> = rgba[..texels].iter().copied().map(float_to_half).collect();
        let bytes = (texels * std::mem::size_of::<u16>()) as vk::DeviceSize;

        let mut staging = StagingBuffer::new(self.allocator(), bytes)?;
        staging.mapped_slice()[..bytes as usize].copy_from_slice(bytemuck::cast_slice(&half));
        staging.flush();

        let format = vk::Format::R16G16B16A16_SFLOAT;
        let uploaded = self.create_sampled_image(width, height, 1, format)?;
        let image = uploaded.image;

        let recorded = self.with_one_off_commands("upload_texture_float", |cmd| {
            // SAFETY: the ash seam. The image/staging buffer outlive the submit-wait.
            unsafe {
                let raw = self.raw();
                transition_image(
                    raw,
                    cmd,
                    image,
                    1,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                copy_buffer_to_image(raw, cmd, staging.handle(), image, width, height);
                transition_image(
                    raw,
                    cmd,
                    image,
                    1,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
            }
        });
        drop(staging);
        if let Err(err) = recorded {
            self.destroy_image(uploaded.image, uploaded.allocation);
            return Err(err);
        }

        self.finish_texture(descriptors, uploaded, None)
    }

    /// Uploads six tightly packed linear-float RGBA faces into a sampled HDR cube.
    ///
    /// Faces use Vulkan cube order `+X, -X, +Y, -Y, +Z, -Z`; each carries `size²` texels.
    pub fn upload_cube_float(&self, rgba: &[f32], size: u32) -> Result<Image> {
        if size == 0 {
            return Err(Error::ZeroSizedImage);
        }
        let texels = size as usize * size as usize * 6 * 4;
        if rgba.len() < texels {
            return Err(Error::InvalidUploadData(format!(
                "cube upload expected {texels} floats, received {}",
                rgba.len()
            )));
        }
        let half: Vec<u16> = rgba[..texels].iter().copied().map(float_to_half).collect();
        let bytes = (half.len() * std::mem::size_of::<u16>()) as vk::DeviceSize;
        let mut staging = StagingBuffer::new(self.allocator(), bytes)?;
        staging
            .mapped_slice()
            .copy_from_slice(bytemuck::cast_slice(&half));
        staging.flush();

        let desc = ImageDesc {
            extent: vk::Extent2D {
                width: size,
                height: size,
            },
            format: vk::Format::R16G16B16A16_SFLOAT,
            usage: vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
            aspect: vk::ImageAspectFlags::COLOR,
            view_type: vk::ImageViewType::CUBE,
            mip_levels: 1,
            array_layers: 6,
            samples: vk::SampleCountFlags::TYPE_1,
        };
        let mut image = Image::new(&self.resources, &desc)?;
        let handle = image.handle();
        let recorded = self.with_one_off_commands("upload_cube_float", |cmd| {
            // SAFETY: the cube and staging buffer outlive the waited one-off submit.
            unsafe {
                transition_image_layers(
                    self.raw(),
                    cmd,
                    handle,
                    6,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 6,
                    })
                    .image_extent(vk::Extent3D {
                        width: size,
                        height: size,
                        depth: 1,
                    });
                self.raw().cmd_copy_buffer_to_image(
                    cmd,
                    staging.handle(),
                    handle,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
                transition_image_layers(
                    self.raw(),
                    cmd,
                    handle,
                    6,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
            }
        });
        drop(staging);
        recorded?;
        image.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        Ok(image)
    }

    /// Uploads an RGBA8 image as a sampled, mipmapped **displacement height** texture *and* builds its
    /// per-height min/max pyramid, writing both into the same bindless slot (the texture at binding 0,
    /// the pyramid at binding 4). Mirrors [`Uploader::upload_texture`] with `srgb = false` (height is
    /// linear data), then attaches the pyramid so the tessellation factor kernel refines per-region.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ZeroSizedImage`] for a zero extent or [`Error::Vk`] for a failing Vulkan/VMA
    /// call; allocated resources are freed before return on error.
    pub fn upload_height_texture(
        &self,
        descriptors: &Descriptors,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Arc<GpuTexture>> {
        if width == 0 || height == 0 {
            return Err(Error::ZeroSizedImage);
        }
        // The min/max pyramid over the height channel (R), built + uploaded first; freed on the texture
        // failure path below (finish_texture takes ownership on success).
        let pyramid = self.build_and_upload_pyramid(rgba, width, height)?;

        let bytes = (width as vk::DeviceSize) * (height as vk::DeviceSize) * 4;
        let mut staging = match StagingBuffer::new(self.allocator(), bytes) {
            Ok(staging) => staging,
            Err(err) => {
                self.free_pyramid(pyramid);
                return Err(err);
            }
        };
        staging.mapped_slice()[..bytes as usize].copy_from_slice(&rgba[..bytes as usize]);
        staging.flush();

        let mip_levels = mip_count(width, height);
        let uploaded = match self.create_sampled_image(
            width,
            height,
            mip_levels,
            vk::Format::R8G8B8A8_UNORM,
        ) {
            Ok(uploaded) => uploaded,
            Err(err) => {
                drop(staging);
                self.free_pyramid(pyramid);
                return Err(err);
            }
        };
        let image = uploaded.image;
        let recorded = self.with_one_off_commands("upload_height_texture", |cmd| {
            // SAFETY: the ash seam. The image/staging buffer outlive the submit-wait.
            unsafe {
                record_texture_upload(
                    self.raw(),
                    cmd,
                    image,
                    staging.handle(),
                    width,
                    height,
                    mip_levels,
                )
            };
        });
        drop(staging);
        if let Err(err) = recorded {
            self.destroy_image(uploaded.image, uploaded.allocation);
            self.free_pyramid(pyramid);
            return Err(err);
        }

        self.finish_texture(descriptors, uploaded, Some(pyramid))
    }

    /// Builds the min/max pyramid over the height channel (R, normalized `[0, 1]`) on the CPU and
    /// uploads it into an `R32G32_SFLOAT` image (min in R / max in G, one mip per pyramid level). Each
    /// level is written directly (no blit — a blit would linearly filter, breaking the conservative
    /// bound); the CPU levels are the exact per-texel `(min, max)`.
    fn build_and_upload_pyramid(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<MinMaxPyramid> {
        let texel_count = (width as usize) * (height as usize);
        let heights: Vec<f32> = (0..texel_count)
            .map(|i| f32::from(rgba[i * 4]) / 255.0)
            .collect();
        let levels = build_min_max_pyramid(&heights, width, height);
        // A degenerate input yields no levels; fall back to a single 1×1 `(min, max)` over the image so
        // the slot always holds a valid pyramid (the factor kernel clamps the LOD).
        let mip_levels = levels.len().max(1) as u32;

        // Stage every level's `[min, max]` texels contiguously; each level starts 8-byte aligned (the
        // texel block size), satisfying the buffer-image copy offset alignment.
        let mut packed: Vec<[f32; 2]> = Vec::new();
        let mut level_offsets: Vec<vk::DeviceSize> = Vec::with_capacity(levels.len());
        for level in &levels {
            level_offsets.push((packed.len() * std::mem::size_of::<[f32; 2]>()) as vk::DeviceSize);
            packed.extend_from_slice(&level.texels);
        }
        if packed.is_empty() {
            packed.push([0.0, 0.0]);
            level_offsets.push(0);
        }
        let bytes = (packed.len() * std::mem::size_of::<[f32; 2]>()) as vk::DeviceSize;
        let mut staging = StagingBuffer::new(self.allocator(), bytes)?;
        staging.mapped_slice()[..bytes as usize].copy_from_slice(bytemuck::cast_slice(&packed));
        staging.flush();

        let uploaded =
            self.create_sampled_image(width, height, mip_levels, vk::Format::R32G32_SFLOAT)?;
        let image = uploaded.image;
        let dims: Vec<(u32, u32)> = if levels.is_empty() {
            vec![(1, 1)]
        } else {
            levels.iter().map(|l| (l.width, l.height)).collect()
        };
        let recorded = self.with_one_off_commands("build_and_upload_pyramid", |cmd| {
            // SAFETY: the ash seam. The image/staging buffer outlive the submit-wait.
            unsafe {
                let raw = self.raw();
                transition_image(
                    raw,
                    cmd,
                    image,
                    mip_levels,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                for (mip, &(w, h)) in dims.iter().enumerate() {
                    copy_buffer_to_image_mip(
                        raw,
                        cmd,
                        staging.handle(),
                        image,
                        level_offsets[mip],
                        mip as u32,
                        vk::Extent2D {
                            width: w,
                            height: h,
                        },
                    );
                }
                transition_image(
                    raw,
                    cmd,
                    image,
                    mip_levels,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
            }
        });
        drop(staging);
        if let Err(err) = recorded {
            self.destroy_image(uploaded.image, uploaded.allocation);
            return Err(err);
        }

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R32G32_SFLOAT)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: mip_levels,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the pyramid image just uploaded.
        let view = match unsafe { self.raw().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                self.destroy_image(uploaded.image, uploaded.allocation);
                return Err(Error::Vk {
                    context: "create_image_view (min/max pyramid)",
                    result,
                });
            }
        };
        Ok(MinMaxPyramid {
            image,
            view,
            allocation: uploaded.allocation,
        })
    }

    /// Frees a not-yet-owned [`MinMaxPyramid`] on an upload error path (before a `GpuTexture` takes it).
    fn free_pyramid(&self, pyramid: MinMaxPyramid) {
        // SAFETY: the ash/VMA seam. The view/image were created here and not yet owned; freed once.
        unsafe { self.raw().destroy_image_view(pyramid.view, None) };
        self.destroy_image(pyramid.image, pyramid.allocation);
    }

    /// Uploads the default 1×1 `(0, 0)` min/max pyramid and seeds it into *every*
    /// `heightMinMaxTextures` slot (binding 4), returning the holder the renderer keeps for its
    /// lifetime. A non-displacement texture's slot keeps this default (zero local range → no extra
    /// tessellation refinement); a displacement height map overwrites its slot with its real pyramid.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`]/[`Error::ZeroSizedImage`] for a failing Vulkan/VMA call.
    pub fn upload_default_height_minmax(
        &self,
        descriptors: &Descriptors,
    ) -> Result<crate::resources::DefaultHeightMinMax> {
        let bytes = std::mem::size_of::<[f32; 2]>() as vk::DeviceSize;
        let mut staging = StagingBuffer::new(self.allocator(), bytes)?;
        staging.mapped_slice()[..bytes as usize]
            .copy_from_slice(bytemuck::cast_slice(&[0.0f32, 0.0]));
        staging.flush();

        let uploaded = self.create_sampled_image(1, 1, 1, vk::Format::R32G32_SFLOAT)?;
        let image = uploaded.image;
        let recorded = self.with_one_off_commands("upload_default_height_minmax", |cmd| {
            // SAFETY: the ash seam. The image/staging buffer outlive the submit-wait.
            unsafe {
                let raw = self.raw();
                transition_image(
                    raw,
                    cmd,
                    image,
                    1,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                copy_buffer_to_image(raw, cmd, staging.handle(), image, 1, 1);
                transition_image(
                    raw,
                    cmd,
                    image,
                    1,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
            }
        });
        drop(staging);
        if let Err(err) = recorded {
            self.destroy_image(uploaded.image, uploaded.allocation);
            return Err(err);
        }

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R32G32_SFLOAT)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the default image just uploaded.
        let view = match unsafe { self.raw().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                self.destroy_image(uploaded.image, uploaded.allocation);
                return Err(Error::Vk {
                    context: "create_image_view (default min/max)",
                    result,
                });
            }
        };
        descriptors.seed_all_height_minmax(view);
        Ok(crate::resources::DefaultHeightMinMax::from_parts(
            &self.resources,
            image,
            view,
            uploaded.allocation,
        ))
    }

    /// Creates a device-local sampled image (`TRANSFER_DST | TRANSFER_SRC | SAMPLED`,
    /// dedicated memory), the shared image shape of both texture upload paths.
    fn create_sampled_image(
        &self,
        width: u32,
        height: u32,
        mip_levels: u32,
        format: vk::Format,
    ) -> Result<UploadedImage> {
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(mip_levels)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the image is owned and
        // freed by the returned `GpuTexture` (or `destroy_image` on a later failure).
        let (image, allocation) = checked(
            unsafe { self.allocator().create_image(&info, &alloc_info) },
            "vmaCreateImage (texture)",
        )?;
        Ok(UploadedImage {
            image,
            allocation,
            width,
            height,
            format,
            mip_levels,
        })
    }

    /// Creates the sampled view, claims a bindless slot, writes the texture into the
    /// global set, and wraps the image as a [`GpuTexture`] owning that slot. When `min_max` is
    /// present (a displacement height map) the pyramid is written into binding 4 at the same slot, so
    /// the factor kernel's `heightIndex` addresses both, and the [`GpuTexture`] owns it.
    fn finish_texture(
        &self,
        descriptors: &Descriptors,
        uploaded: UploadedImage,
        min_max: Option<MinMaxPyramid>,
    ) -> Result<Arc<GpuTexture>> {
        let UploadedImage {
            image,
            allocation,
            width,
            height,
            format,
            mip_levels,
        } = uploaded;
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: mip_levels,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the image just uploaded.
        let view = match unsafe { self.raw().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                self.destroy_image(image, allocation);
                return Err(Error::Vk {
                    context: "create_image_view (texture)",
                    result,
                });
            }
        };

        // Claim a bindless slot (reusing a reclaimed one) and write the texture in. A full
        // array skips the upload (freeing the image + any pyramid) rather than writing out of range.
        let Some(index) = descriptors.claim_slot() else {
            tracing::warn!(
                "albedo bindless array full ({}), texture skipped",
                descriptors.texture_capacity()
            );
            // SAFETY: the ash seam. The view was created above and not yet owned by a
            // `GpuTexture`; free it once on this array-full path before the image.
            unsafe { self.raw().destroy_image_view(view, None) };
            self.destroy_image(image, allocation);
            if let Some(pyramid) = min_max {
                self.free_pyramid(pyramid);
            }
            return Err(Error::BindlessFull("albedo texture"));
        };
        descriptors.write_texture(view, index);
        if let Some(pyramid) = &min_max {
            descriptors.write_height_minmax(pyramid.view, index);
        }

        let texture = GpuTexture::from_parts(
            &self.resources,
            GpuTextureParts {
                image,
                view,
                allocation,
                bindless_index: index,
                extent: vk::Extent2D { width, height },
                format,
                mip_count: mip_levels,
                min_max,
            },
            descriptors.free_list(),
        );
        Ok(Arc::new(texture))
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
            self.resources
                .device()
                .destroy_command_pool(self.command_pool, None);
        }
    }
}

/// The push-constant block the three SDF bake shaders share (64 bytes, std430). The
/// `misc` lane carries the per-pass selector (jump step / parity / init-vs-prop / final
/// parity); see each shader's `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BakePush {
    /// `xyz` fine voxel dims, `w` triangle count.
    dims: [u32; 4],
    /// `xyz` grid lower corner (local), `w` the `R16_SNORM` encode clamp (`max_dist`).
    bounds_min: [f32; 4],
    /// `xyz` per-axis cell size, `w` the winding sign (orients the distance sign).
    cell: [f32; 4],
    /// Per-pass selector (`x` step / final parity, `y` parity, `z` mode).
    misc: [i32; 4],
}

const _: () = assert!(size_of::<BakePush>() == 64, "BakePush must be 64 bytes");

/// The transient seed / work 3D images one SDF bake dispatches over: the seed-key scatter
/// target and the two jump-flood ping-pong seed buffers. All live only for the bake;
/// dropping frees them.
struct BakeImages {
    seed_key: Image3D,
    seed_a: Image3D,
    seed_b: Image3D,
}

/// The two GPU jump-flood bake compute pipelines (voxelize → JFA) + the shared
/// descriptor-set layout, pipeline layout, and a small descriptor pool. Owned by an
/// [`Uploader`] (built in [`Uploader::new`]) so the bake runs on the upload path —
/// including the thumbnail worker's own uploader — not the renderer's frame pipeline cache.
/// The sign pass is on the host (see [`sign_field`]).
struct BakePipelines {
    resources: Arc<DeviceResources>,
    set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pool: vk::DescriptorPool,
    voxelize: vk::Pipeline,
    jfa: vk::Pipeline,
}

impl BakePipelines {
    /// Builds the bake descriptor layout (2 storage buffers + 3 storage images), the
    /// pipeline layout (the 64-byte [`BakePush`]), a small `FREE_DESCRIPTOR_SET` pool, and
    /// the two compute pipelines from the runtime shader dir.
    fn new(resources: &Arc<DeviceResources>) -> Result<Self> {
        let raw = resources.device();
        let buffer = |b: u32| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        };
        let image = |b: u32| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        };
        let bindings = [buffer(0), buffer(1), image(2), image(3), image(4)];
        let layout_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        // SAFETY: the ash seam. The bindings outlive the call; the layout is freed in `Drop`.
        let set_layout = checked(
            unsafe { raw.create_descriptor_set_layout(&layout_info, None) },
            "sdf bake set layout",
        )?;

        let set_layouts = [set_layout];
        let push = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .offset(0)
            .size(size_of::<BakePush>() as u32)];
        let pl_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push);
        // SAFETY: the ash seam. The layout owns the set layout reference for the call.
        let pipeline_layout = match checked(
            unsafe { raw.create_pipeline_layout(&pl_info, None) },
            "sdf bake pipeline layout",
        ) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. The set layout was created above; free it once.
                unsafe { raw.destroy_descriptor_set_layout(set_layout, None) };
                return Err(err);
            }
        };

        let pool_sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_BUFFER,
                descriptor_count: 16,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_IMAGE,
                descriptor_count: 32,
            },
        ];
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
            .max_sets(8)
            .pool_sizes(&pool_sizes);
        // SAFETY: the ash seam. The pool is freed in `Drop`.
        let pool = match checked(
            unsafe { raw.create_descriptor_pool(&pool_info, None) },
            "sdf bake pool",
        ) {
            Ok(pool) => pool,
            Err(err) => {
                // SAFETY: the ash seam. The layouts created above are freed once.
                unsafe {
                    raw.destroy_pipeline_layout(pipeline_layout, None);
                    raw.destroy_descriptor_set_layout(set_layout, None);
                }
                return Err(err);
            }
        };

        let build = (|| -> Result<(vk::Pipeline, vk::Pipeline)> {
            let dir = crate::pipelines::resolve_shader_dir();
            let voxelize = compute_pipeline(raw, &dir, "sdf_voxelize.spv", pipeline_layout)?;
            let jfa = match compute_pipeline(raw, &dir, "sdf_jfa.spv", pipeline_layout) {
                Ok(p) => p,
                Err(err) => {
                    // SAFETY: the ash seam. Free the voxelize pipeline before the error.
                    unsafe { raw.destroy_pipeline(voxelize, None) };
                    return Err(err);
                }
            };
            Ok((voxelize, jfa))
        })();
        let (voxelize, jfa) = match build {
            Ok(pipelines) => pipelines,
            Err(err) => {
                // SAFETY: the ash seam. Free the pool + layouts on a pipeline-build failure.
                unsafe {
                    raw.destroy_descriptor_pool(pool, None);
                    raw.destroy_pipeline_layout(pipeline_layout, None);
                    raw.destroy_descriptor_set_layout(set_layout, None);
                }
                return Err(err);
            }
        };

        Ok(Self {
            resources: Arc::clone(resources),
            set_layout,
            pipeline_layout,
            pool,
            voxelize,
            jfa,
        })
    }

    /// Allocates one bake descriptor set from the pool (freed by the caller after the bake).
    fn allocate_set(&self, raw: &ash::Device) -> Result<vk::DescriptorSet> {
        let layouts = [self.set_layout];
        let info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.pool)
            .set_layouts(&layouts);
        // SAFETY: the ash seam. The layout outlives the call; the set is freed after the bake.
        let sets = checked(
            unsafe { raw.allocate_descriptor_sets(&info) },
            "allocate sdf bake set",
        )?;
        Ok(sets[0])
    }

    /// Writes the geometry buffers + the three transient images into the bake set.
    fn write_set(
        &self,
        raw: &ash::Device,
        set: vk::DescriptorSet,
        positions: vk::Buffer,
        indices: vk::Buffer,
        images: &BakeImages,
    ) {
        let buf = |b: vk::Buffer| {
            [vk::DescriptorBufferInfo {
                buffer: b,
                offset: 0,
                range: vk::WHOLE_SIZE,
            }]
        };
        let img = |v: vk::ImageView| {
            [vk::DescriptorImageInfo {
                sampler: vk::Sampler::null(),
                image_view: v,
                image_layout: vk::ImageLayout::GENERAL,
            }]
        };
        let pos = buf(positions);
        let idx = buf(indices);
        let key = img(images.seed_key.view());
        let sa = img(images.seed_a.view());
        let sb = img(images.seed_b.view());
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&pos),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&idx),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&key),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&sa),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(4)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&sb),
        ];
        // SAFETY: the ash seam. The set + all referenced resources outlive the call.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };
    }
}

impl Drop for BakePipelines {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The bundle keeps the device alive; each handle is freed once.
        unsafe {
            let raw = self.resources.device();
            raw.destroy_pipeline(self.voxelize, None);
            raw.destroy_pipeline(self.jfa, None);
            raw.destroy_pipeline_layout(self.pipeline_layout, None);
            raw.destroy_descriptor_pool(self.pool, None);
            raw.destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}

/// Loads `<dir>/<file>` SPIR-V and builds a compute pipeline (entry `computeMain`) against
/// `layout`. The shader module is freed after pipeline creation.
fn compute_pipeline(
    raw: &ash::Device,
    dir: &Path,
    file: &str,
    layout: vk::PipelineLayout,
) -> Result<vk::Pipeline> {
    let path = dir.join(file);
    let bytes = std::fs::read(&path)
        .map_err(|err| Error::ShaderLoad(format!("cannot read '{}': {err}", path.display())))?;
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return Err(Error::ShaderLoad(format!(
            "invalid SPIR-V size for '{}' ({} bytes)",
            path.display(),
            bytes.len()
        )));
    }
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let module_info = vk::ShaderModuleCreateInfo::default().code(&words);
    // SAFETY: the ash seam. The code slice outlives the call; the module is freed below.
    let module = checked(
        unsafe { raw.create_shader_module(&module_info, None) },
        "create_shader_module (sdf bake)",
    )?;
    let stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(module)
        .name(c"computeMain");
    let info = [vk::ComputePipelineCreateInfo::default()
        .stage(stage)
        .layout(layout)];
    // SAFETY: the ash seam. The create-info outlives the call.
    let created = unsafe { raw.create_compute_pipelines(vk::PipelineCache::null(), &info, None) };
    // SAFETY: the ash seam. The module is consumed by creation; free it now.
    unsafe { raw.destroy_shader_module(module, None) };
    match created {
        Ok(pipelines) => Ok(pipelines[0]),
        Err((_, result)) => Err(Error::Vk {
            context: "create_compute_pipelines (sdf bake)",
            result,
        }),
    }
}

/// A global transfer→compute memory barrier: the geometry copies + the seed-key clear are
/// made visible to the bake's compute reads.
fn transfer_to_compute_barrier(raw: &ash::Device, cmd: vk::CommandBuffer) {
    let barrier = vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .dst_access_mask(
            vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
        );
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
    // SAFETY: the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// A global compute→compute memory barrier between successive bake dispatches (one pass's
/// storage-image writes visible to the next pass's reads).
fn compute_to_compute_barrier(raw: &ash::Device, cmd: vk::CommandBuffer) {
    let barrier = vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .dst_access_mask(
            vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
        );
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
    // SAFETY: the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// A global compute→transfer memory barrier: the final jump-flood seed write is made
/// visible to the readback copy.
fn compute_to_transfer_barrier(raw: &ash::Device, cmd: vk::CommandBuffer) {
    let barrier = vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_READ);
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
    // SAFETY: the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// Decodes the octahedral-packed unit normal the jump-flood seed carries in `w` (the bits
/// of an f32). Mirrors `packNormal`/`octDecode` in `sdf_jfa.slang` so the host sign pass
/// reads the same nearest-triangle normal the GPU stored.
fn unpack_oct_normal(packed: f32) -> Vec3 {
    let p = packed.to_bits();
    let ex = (p & 0xFFFF) as f32 / 65535.0 * 2.0 - 1.0;
    let ey = (p >> 16) as f32 / 65535.0 * 2.0 - 1.0;
    let mut n = Vec3::new(ex, ey, 1.0 - ex.abs() - ey.abs());
    let t = (-n.z).clamp(0.0, 1.0);
    n.x += if n.x >= 0.0 { -t } else { t };
    n.y += if n.y >= 0.0 { -t } else { t };
    n.normalize_or_zero()
}

/// Keeps the triangles of `tris` (global vertex indices into `positions`) whose AABB overlaps
/// `[lo, hi]`, remapping their vertices into a compact position list + local index list so an
/// SDF region bake uploads only the geometry that reaches it. A triangle straddling the bounds
/// is kept whole (AABB overlap is conservative), so a surface passing through the region is
/// never missed; a triangle within the encode clamp of a chunk's core is captured by that
/// chunk's padded grid, which is what keeps a `min`-over-instances sample exact across seams.
fn compact_tris_in_bounds(
    positions: &[Vec3],
    tris: &[u32],
    lo: Vec3,
    hi: Vec3,
) -> (Vec<Vec3>, Vec<u32>) {
    use std::collections::HashMap;
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut out_pos: Vec<Vec3> = Vec::new();
    let mut out_idx: Vec<u32> = Vec::new();
    for t in tris.chunks_exact(3) {
        let (a, b, c) = (
            positions[t[0] as usize],
            positions[t[1] as usize],
            positions[t[2] as usize],
        );
        let tlo = a.min(b).min(c);
        let thi = a.max(b).max(c);
        if tlo.x > hi.x
            || thi.x < lo.x
            || tlo.y > hi.y
            || thi.y < lo.y
            || tlo.z > hi.z
            || thi.z < lo.z
        {
            continue;
        }
        for &g in t {
            let local = match remap.get(&g) {
                Some(&l) => l,
                None => {
                    let l = out_pos.len() as u32;
                    out_pos.push(positions[g as usize]);
                    remap.insert(g, l);
                    l
                }
            };
            out_idx.push(local);
        }
    }
    (out_pos, out_idx)
}

/// Resolves the per-voxel signed distance from the GPU jump-flood `seeds` (closest surface
/// point in `xyz`, octahedral-packed nearest-triangle normal in `w`, X-fastest over `grid`),
/// returning the `R16_SNORM` field the brick compactor consumes.
///
/// The magnitude is the unsigned distance `|center − closestPoint|`. The sign comes from an
/// outside-flood: grid-border voxels are outside, and "outside" floods (6-connected) through
/// every voxel the surface band does not block, cross-checked by the nearest-triangle normal
/// in the band the flood cannot label cleanly. A voxel the flood reaches is positive
/// (outside); an enclosed voxel it cannot reach is negative (inside); a surface-band voxel
/// takes the normal's side (oriented by the mesh `winding`). This mirrors the inside/outside
/// the `MeshBvh` oracle reports on closed geometry while staying robust at concave corners,
/// where a single face normal can flip an interior voxel to "outside" and leak occlusion.
fn sign_field(grid: &GridDesc, winding: f32, seeds: &[[f32; 4]]) -> Vec<i16> {
    let [nx, ny, nz] = grid.dims;
    let (nx, ny, nz) = (nx as usize, ny as usize, nz as usize);
    let count = nx * ny * nz;
    let max_dist = grid.max_dist.max(1e-6);
    // A voxel whose center is within one cell diagonal of the surface joins the band the
    // flood must not cross (so "outside" cannot leak to "inside" through the surface).
    // Generous on purpose: an over-thick band only widens the region signed by the normal,
    // which is correct there anyway.
    let band = grid.cell().length();
    const SENTINEL: f32 = 5e29;
    let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;

    // Unsigned distance + surface-band ("solid") classification per voxel.
    let mut dist = vec![max_dist; count];
    let mut solid = vec![false; count];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let s = seeds[idx(x, y, z)];
                if s[0] >= SENTINEL {
                    continue; // unseeded → far outside
                }
                let center = grid.voxel_center(x as u32, y as u32, z as u32);
                let d = (center - Vec3::new(s[0], s[1], s[2])).length();
                dist[idx(x, y, z)] = d;
                solid[idx(x, y, z)] = d <= band;
            }
        }
    }

    // Outside-flood: a 6-connected BFS from every non-solid grid-border voxel.
    let mut outside = vec![false; count];
    let mut stack: Vec<(usize, usize, usize)> = Vec::new();
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let border =
                    x == 0 || y == 0 || z == 0 || x == nx - 1 || y == ny - 1 || z == nz - 1;
                let i = idx(x, y, z);
                if border && !solid[i] && !outside[i] {
                    outside[i] = true;
                    stack.push((x, y, z));
                }
            }
        }
    }
    while let Some((x, y, z)) = stack.pop() {
        let mut visit = |nx_: usize, ny_: usize, nz_: usize, outside: &mut [bool]| {
            let j = idx(nx_, ny_, nz_);
            if !solid[j] && !outside[j] {
                outside[j] = true;
                stack.push((nx_, ny_, nz_));
            }
        };
        if x > 0 {
            visit(x - 1, y, z, &mut outside);
        }
        if x + 1 < nx {
            visit(x + 1, y, z, &mut outside);
        }
        if y > 0 {
            visit(x, y - 1, z, &mut outside);
        }
        if y + 1 < ny {
            visit(x, y + 1, z, &mut outside);
        }
        if z > 0 {
            visit(x, y, z - 1, &mut outside);
        }
        if z + 1 < nz {
            visit(x, y, z + 1, &mut outside);
        }
    }

    // Sign each voxel: flood-outside → positive; enclosed → negative; band → the normal.
    let mut field = vec![0i16; count];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let i = idx(x, y, z);
                let inside = if outside[i] {
                    false
                } else if solid[i] {
                    let s = seeds[i];
                    if s[0] >= SENTINEL {
                        false
                    } else {
                        let center = grid.voxel_center(x as u32, y as u32, z as u32);
                        let delta = center - Vec3::new(s[0], s[1], s[2]);
                        delta.dot(unpack_oct_normal(s[3])) * winding < 0.0
                    }
                } else {
                    true
                };
                let signed = if inside { -dist[i] } else { dist[i] };
                field[i] = ((signed / max_dist).clamp(-1.0, 1.0) * 32767.0).round() as i16;
            }
        }
    }
    field
}

/// An FNV-1a content hash of a mesh's geometry + bake resolution scale — the sidecar cache
/// key. A touched-but-unchanged mesh hashes identically and reuses its baked field.
fn mesh_content_hash(positions: &[Vec3], indices: &[u32], resolution_scale: f32) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut fold = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    for p in positions {
        fold(&p.x.to_le_bytes());
        fold(&p.y.to_le_bytes());
        fold(&p.z.to_le_bytes());
    }
    fold(bytemuck::cast_slice(indices));
    fold(&resolution_scale.to_le_bytes());
    hash
}

/// A freshly created device-local sampled image awaiting its view + bindless slot —
/// the handoff from [`Uploader::create_sampled_image`] to
/// [`Uploader::finish_texture`]. Owns the image until `finish_texture` wraps it in a
/// [`GpuTexture`] (or an upload-recording failure frees it directly).
struct UploadedImage {
    image: vk::Image,
    allocation: vk_mem::Allocation,
    width: u32,
    height: u32,
    format: vk::Format,
    mip_levels: u32,
}

/// A host-visible, persistently mapped staging buffer that flushes and frees itself.
struct StagingBuffer<'a> {
    allocator: &'a vk_mem::Allocator,
    buffer: vk::Buffer,
    allocation: vk_mem::Allocation,
    mapped: *mut u8,
    size: vk::DeviceSize,
}

impl<'a> StagingBuffer<'a> {
    /// Allocates a `TRANSFER_SRC`, host-sequential-write, mapped buffer of `size`.
    fn new(allocator: &'a vk_mem::Allocator, size: vk::DeviceSize) -> Result<Self> {
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the buffer is freed in
        // `Drop`.
        let (buffer, allocation) = checked(
            unsafe { allocator.create_buffer(&info, &alloc_info) },
            "vmaCreateBuffer (staging)",
        )?;
        let mapped = allocator
            .get_allocation_info(&allocation)
            .mapped_data
            .cast::<u8>();
        Ok(Self {
            allocator,
            buffer,
            allocation,
            mapped,
            size,
        })
    }

    fn handle(&self) -> vk::Buffer {
        self.buffer
    }

    /// The mapped staging memory as a writable byte slice (the upload source).
    fn mapped_slice(&mut self) -> &mut [u8] {
        // SAFETY: the allocation is HOST_VISIBLE + MAPPED for `size` bytes; the
        // `&mut self` borrow makes the slice exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.mapped, self.size as usize) }
    }

    /// Flushes the mapped writes so the GPU copy sees them.
    fn flush(&self) {
        // The map may be coherent; the flush is a no-op then. Either way it flushes
        // the whole allocation.
        let _ = self
            .allocator
            .flush_allocation(&self.allocation, 0, self.size);
    }
}

impl Drop for StagingBuffer<'_> {
    fn drop(&mut self) {
        // SAFETY: the VMA seam. The staging buffer is freed exactly once after the
        // copy completed (the one-off submit was waited before this drop).
        unsafe {
            self.allocator
                .destroy_buffer(self.buffer, &mut self.allocation);
        }
    }
}

/// Allocates a device-local buffer (`size`, `usage | TRANSFER_DST`, auto memory).
fn make_device_buffer(
    allocator: &vk_mem::Allocator,
    size: vk::DeviceSize,
    usage: vk::BufferUsageFlags,
) -> Result<(vk::Buffer, vk_mem::Allocation)> {
    let info = vk::BufferCreateInfo::default()
        .size(size)
        .usage(usage | vk::BufferUsageFlags::TRANSFER_DST);
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    // SAFETY: the VMA seam. The create-infos are valid; ownership of the returned
    // buffer passes to the caller (the `GpuMesh`, or freed on the error path).
    checked(
        unsafe { allocator.create_buffer(&info, &alloc_info) },
        "vmaCreateBuffer (device)",
    )
}

/// Frees one device buffer directly — the mesh-upload error-path cleanup before a
/// `GpuMesh` takes ownership of the set.
fn free_one(allocator: &vk_mem::Allocator, buffer: (vk::Buffer, vk_mem::Allocation)) {
    let (handle, mut allocation) = buffer;
    // SAFETY: the VMA seam. The buffer was created on this allocator and not yet
    // owned by a `GpuMesh`; freed exactly once on the error path.
    unsafe { allocator.destroy_buffer(handle, &mut allocation) };
}

/// Full mip-chain length for a `width × height` image (down to 1×1).
fn mip_count(width: u32, height: u32) -> u32 {
    let mut d = width.max(height);
    let mut levels = 1;
    while d > 1 {
        d >>= 1;
        levels += 1;
    }
    levels
}

/// Records the RGBA8 upload + full mip-chain generation for `image` into `cmd`: all
/// mips → `TRANSFER_DST`, copy mip 0, blit down the chain, then every mip → shader
/// read.
///
/// # Safety
///
/// `cmd` must be in the recording state; `image` (with `mip_levels` mips) and `src`
/// must outlive the submit that consumes `cmd`.
unsafe fn record_texture_upload(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    src: vk::Buffer,
    width: u32,
    height: u32,
    mip_levels: u32,
) {
    // All mips start TransferDst: mip 0 receives the copy, the rest receive blits.
    let to_dst = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: mip_levels,
            base_array_layer: 0,
            layer_count: 1,
        });
    let to_dst = [to_dst];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&to_dst);
    // SAFETY: the caller's recording contract; the image outlives the submit.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };

    // SAFETY: as above; mip 0 is in TRANSFER_DST per the barrier.
    unsafe { copy_buffer_to_image(raw, cmd, src, image, width, height) };

    // SAFETY: as above; generates mips 1..n and transitions every level to read.
    unsafe { record_mip_chain(raw, cmd, image, width, height, mip_levels) };
}

/// Copies the whole of `src` into mip 0 of `image` (in `TRANSFER_DST`).
///
/// # Safety
///
/// `cmd` recording; `src`/`image` outlive the submit.
unsafe fn copy_buffer_to_image(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    src: vk::Buffer,
    image: vk::Image,
    width: u32,
    height: u32,
) {
    let region = vk::BufferImageCopy::default()
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        });
    // SAFETY: the caller's recording contract.
    unsafe {
        raw.cmd_copy_buffer_to_image(
            cmd,
            src,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        );
    }
}

/// Copies `mip_level`'s `width`×`height` texels from `src` at `buffer_offset` into `image` (in
/// `TRANSFER_DST`) — the per-level path the min/max pyramid uses (each level is exact CPU data, so it
/// is copied directly rather than blitted).
///
/// # Safety
///
/// `cmd` recording; `src`/`image` outlive the submit; `buffer_offset` is aligned to the texel block.
unsafe fn copy_buffer_to_image_mip(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    src: vk::Buffer,
    image: vk::Image,
    buffer_offset: vk::DeviceSize,
    mip_level: u32,
    extent: vk::Extent2D,
) {
    let region = vk::BufferImageCopy::default()
        .buffer_offset(buffer_offset)
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_extent(vk::Extent3D {
            width: extent.width,
            height: extent.height,
            depth: 1,
        });
    // SAFETY: the caller's recording contract.
    unsafe {
        raw.cmd_copy_buffer_to_image(
            cmd,
            src,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        );
    }
}

/// Generates mips 1..`mip_levels` by blitting down from mip 0, then transitions every
/// level to `SHADER_READ_ONLY_OPTIMAL`. On entry every level is `TRANSFER_DST`.
///
/// # Safety
///
/// `cmd` recording; `image` (with `mip_levels` mips) outlives the submit.
unsafe fn record_mip_chain(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    width: u32,
    height: u32,
    mip_levels: u32,
) {
    let mut mw = width as i32;
    let mut mh = height as i32;
    for i in 1..mip_levels {
        // SAFETY: the caller's recording contract.
        unsafe {
            mip_barrier(
                raw,
                cmd,
                image,
                i - 1,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_WRITE,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_READ,
            );
        }
        let nw = if mw > 1 { mw / 2 } else { 1 };
        let nh = if mh > 1 { mh / 2 } else { 1 };
        let blit = vk::ImageBlit::default()
            .src_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: i - 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .src_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D { x: mw, y: mh, z: 1 },
            ])
            .dst_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: i,
                base_array_layer: 0,
                layer_count: 1,
            })
            .dst_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D { x: nw, y: nh, z: 1 },
            ]);
        // SAFETY: the caller's recording contract; the blit reads mip i-1 (SRC) and
        // writes mip i (DST).
        unsafe {
            raw.cmd_blit_image(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                vk::Filter::LINEAR,
            );
        }
        mw = nw;
        mh = nh;
    }
    for i in 0..mip_levels {
        let last = i == mip_levels - 1;
        // The last level only received a copy/blit-dst (TRANSFER_DST); every earlier
        // level was a blit source (TRANSFER_SRC), so its source stage/access differ.
        let (from_layout, src_stage, src_access) = if last {
            (
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_WRITE,
            )
        } else {
            (
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_READ,
            )
        };
        // SAFETY: the caller's recording contract.
        unsafe {
            mip_barrier(
                raw,
                cmd,
                image,
                i,
                from_layout,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                src_stage,
                src_access,
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            );
        }
    }
}

/// One sync2 image-memory barrier on a single mip level.
///
/// # Safety
///
/// `cmd` recording; `image` outlives the submit.
#[allow(clippy::too_many_arguments)]
unsafe fn mip_barrier(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    mip: u32,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let barrier = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(from)
        .new_layout(to)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: mip,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
    // SAFETY: the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// One whole-image sync2 layout transition (single mip range), used by the float
/// (single-mip) texture path.
///
/// # Safety
///
/// `cmd` recording; `image` outlives the submit.
#[allow(clippy::too_many_arguments)]
unsafe fn transition_image(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    mip_levels: u32,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let barrier = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(from)
        .new_layout(to)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: mip_levels,
            base_array_layer: 0,
            layer_count: 1,
        });
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
    // SAFETY: the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// One whole-image sync2 transition for every array layer of a single-mip image.
#[allow(clippy::too_many_arguments)]
unsafe fn transition_image_layers(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    layers: u32,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let barrier = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(from)
        .new_layout(to)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: layers,
        });
    let barriers = [barrier];
    let dependency = vk::DependencyInfo::default().image_memory_barriers(&barriers);
    // SAFETY: the caller's recording contract; the image outlives the submit.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dependency) };
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
    use crate::device::SurfaceSource;
    use crate::resources::BindlessFreeList;
    use crate::validation_issue_count;
    use saffron_geometry::glam::{Vec2, Vec3};
    use saffron_geometry::{Submesh, Vertex};
    use std::sync::Mutex;

    /// Builds a headless device or skips the test (no Vulkan ICD in this toolbox).
    fn device_or_skip() -> Option<Device> {
        match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => Some(device),
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                None
            }
        }
    }

    /// A single-triangle mesh, the minimal valid upload input.
    fn triangle() -> Mesh {
        let v = |x: f32, y: f32| Vertex {
            position: Vec3::new(x, y, 0.0),
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv0: Vec2::ZERO,
            ..Vertex::default()
        };
        Mesh {
            vertices: vec![v(-1.0, -1.0), v(1.0, -1.0), v(0.0, 1.0)],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        }
    }

    /// A plain mesh's cooked hierarchy (one prototype, one identity use) builds no
    /// assembly table; a multi-prototype hierarchy builds the id-ordered prototype
    /// records with prefix-summed vertex bases and prototype-grouped f32 use
    /// transforms.
    #[test]
    fn assembly_table_builds_for_multi_prototype_hierarchies_only() {
        let mesh = triangle();
        let hierarchy = hierarchy_for_upload(&mesh, &[]).expect("cook hierarchy");
        assert!(
            assembly_from_hierarchy(&hierarchy)
                .expect("trivial shape")
                .is_none(),
            "a plain mesh keeps its parts range empty"
        );

        const IDENTITY: [i32; 16] = [
            65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536,
        ];
        let mut translated = IDENTITY;
        translated[7] = 2 * 65_536; // row 1, column 3: +2 m along Y.
        let mut family = hierarchy.clone();
        let base = family.prototypes[0].clone();
        family.prototypes = vec![
            saffron_geometry::GeometryPrototype {
                id: 0,
                vertex_count: 3,
                ..base.clone()
            },
            saffron_geometry::GeometryPrototype {
                id: 1,
                vertex_count: 5,
                ..base
            },
        ];
        family.micro_instances = vec![
            saffron_geometry::MicroInstance {
                part: 1,
                prototype: 0,
                transform_bits: IDENTITY,
            },
            saffron_geometry::MicroInstance {
                part: 2,
                prototype: 1,
                transform_bits: translated,
            },
            saffron_geometry::MicroInstance {
                part: 3,
                prototype: 1,
                transform_bits: IDENTITY,
            },
        ];
        let assembly = assembly_from_hierarchy(&family)
            .expect("family shape")
            .expect("assembly table");
        assert_eq!(assembly.prototypes.len(), 2);
        assert_eq!(
            assembly.prototypes[0],
            crate::GpuAssemblyPrototypeRecord {
                first_use: 0,
                use_count: 1,
                vertex_base: 0,
                reserved: 0,
            }
        );
        assert_eq!(
            assembly.prototypes[1],
            crate::GpuAssemblyPrototypeRecord {
                first_use: 1,
                use_count: 2,
                vertex_base: 3,
                reserved: 0,
            }
        );
        assert_eq!(assembly.uses.len(), 3);
        assert_eq!(assembly.uses[1].prototype, 1);
        assert_eq!(
            assembly.uses[1].transform[7], 2.0,
            "row 1 translation in metres"
        );
        assert_eq!(assembly.uses[2].transform[0], 1.0, "identity scale");
        // No authored combinations → one implicit all-active mask word.
        assert_eq!(assembly.combinations, vec![(0, 0)]);
        assert_eq!(assembly.masks, vec![0b111]);
        assert_eq!(
            assembly.byte_len(),
            size_of::<crate::GpuAssemblyHeaderRecord>()
                + 2 * size_of::<crate::GpuAssemblyPrototypeRecord>()
                + 3 * size_of::<crate::GpuAssemblyUseRecord>()
                + size_of::<u32>()
        );
        assert_eq!(assembly.packed_bytes().len(), assembly.byte_len());
    }

    /// Uploading a mesh with a skin stream produces a `GpuMesh` with a non-null skin
    /// buffer; uploading without one leaves it null — the phase's named skin gate. The
    /// upload runs the real staging→device-local copy on the queue, validation-clean.
    /// Skips when no Vulkan device is present.
    #[test]
    fn upload_mesh_skin_buffer_presence_tracks_the_stream() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        let mesh = triangle();
        let plain_hierarchy = hierarchy_for_upload(&mesh, &[]).expect("cook hierarchy");

        let plain = uploader
            .upload_mesh(&descriptors, &mesh, &plain_hierarchy, &[], None, None)
            .expect("unskinned upload");
        assert_eq!(plain.index_count, 3);
        assert_eq!(plain.vertex_count, 3);
        assert!(
            plain.skin_buffer().is_none(),
            "no skin stream → null skin buffer"
        );
        assert_eq!(&*plain.cpu_indices, mesh.indices.as_slice());
        assert_eq!(plain.cpu_vertices.len(), 3);

        let skin = vec![VertexSkin::default(); mesh.vertices.len()];
        let skinned_hierarchy = hierarchy_for_upload(&mesh, &skin).expect("cook hierarchy");
        let skinned = uploader
            .upload_mesh(&descriptors, &mesh, &skinned_hierarchy, &skin, None, None)
            .expect("skinned upload");
        assert!(
            skinned.skin_buffer().is_some(),
            "a parallel skin stream → non-null skin buffer"
        );
        assert_eq!(skinned.cpu_skin.len(), 3);

        // A mismatched skin stream is rejected before any allocation.
        let bad = uploader.upload_mesh(
            &descriptors,
            &mesh,
            &plain_hierarchy,
            &[VertexSkin::default()],
            None,
            None,
        );
        assert!(matches!(bad, Err(Error::SkinMismatch { .. })));

        drop(plain);
        drop(skinned);
        drop(uploader);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the mesh uploads must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// Uploading an RGBA8 texture (with a full mip chain) and an HDR float texture
    /// each claim a bindless slot, write the view into the global set, and are
    /// validation-clean — the phase's GPU upload smoke. The texture drop returns its
    /// slot to the shared free-list. Skips when no Vulkan device is present.
    #[test]
    fn upload_texture_paths_are_validation_clean() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");

        // A 4×4 sRGB image: mip 0 + a blitted-down chain (mip_count(4,4) == 3).
        let rgba = vec![200u8; 4 * 4 * 4];
        let tex = uploader
            .upload_texture(&descriptors, &rgba, 4, 4, true)
            .expect("rgba8 upload");
        assert_eq!(tex.extent.width, 4);
        assert_eq!(tex.format, vk::Format::R8G8B8A8_SRGB);
        // Slot 0 is the default white; the first uploaded texture takes slot 1.
        assert_eq!(tex.bindless_index(), 1);

        // A 2×2 HDR float image → R16G16B16A16_SFLOAT, single mip.
        let hdr = vec![2.0f32; 2 * 2 * 4];
        let hdr_tex = uploader
            .upload_texture_float(&descriptors, &hdr, 2, 2)
            .expect("float upload");
        assert_eq!(hdr_tex.format, vk::Format::R16G16B16A16_SFLOAT);
        assert_eq!(hdr_tex.bindless_index(), 2);

        // A zero-sized image is rejected before any allocation.
        assert!(matches!(
            uploader.upload_texture(&descriptors, &rgba, 0, 4, false),
            Err(Error::ZeroSizedImage)
        ));

        // Dropping a texture returns its slot to the shared free-list.
        let slot = hdr_tex.bindless_index();
        drop(hdr_tex);
        assert_eq!(free_list.lock().unwrap().as_slice(), &[slot]);

        drop(tex);
        drop(uploader);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the texture uploads must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// An axis-aligned box `[lo, hi]` as 12 outward-wound triangles (positions + indices),
    /// the small mesh the GPU bake test drives.
    fn box_geometry(lo: Vec3, hi: Vec3) -> (Vec<Vec3>, Vec<u32>) {
        let v = [
            Vec3::new(lo.x, lo.y, lo.z),
            Vec3::new(hi.x, lo.y, lo.z),
            Vec3::new(hi.x, hi.y, lo.z),
            Vec3::new(lo.x, hi.y, lo.z),
            Vec3::new(lo.x, lo.y, hi.z),
            Vec3::new(hi.x, lo.y, hi.z),
            Vec3::new(hi.x, hi.y, hi.z),
            Vec3::new(lo.x, hi.y, hi.z),
        ];
        let faces = [
            [0, 1, 2, 3],
            [5, 4, 7, 6],
            [4, 0, 3, 7],
            [1, 5, 6, 2],
            [4, 5, 1, 0],
            [3, 2, 6, 7],
        ];
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        for f in faces {
            let base = positions.len() as u32;
            for &i in &f {
                positions.push(v[i]);
            }
            indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        (positions, indices)
    }

    /// A watertight concave L-shaped prism (an L cross-section extruded in z), built outward-
    /// wound from a rectangle decomposition of the caps + the six perimeter walls. Its
    /// re-entrant corner is the case a single nearest-face normal mis-signs but the outside-
    /// flood gets right — the fixture the box (convex) cannot exercise.
    fn l_prism_geometry() -> (Vec<Vec3>, Vec<u32>) {
        let mut positions: Vec<Vec3> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        // Emit a planar quad a,b,c,d as two triangles, flipped so its normal faces `outward`.
        let mut quad = |a: Vec3, b: Vec3, c: Vec3, d: Vec3, outward: Vec3| {
            let n = (b - a).cross(c - a);
            let (a, b, c, d) = if n.dot(outward) >= 0.0 {
                (a, b, c, d)
            } else {
                (a, d, c, b)
            };
            let base = positions.len() as u32;
            positions.extend([a, b, c, d]);
            indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        };
        // L cross-section (CCW), centred on the origin and scaled up so the arms span a few
        // voxels; the re-entrant vertex sits at (1, 1) before centring.
        let s = 2.0f32;
        let h = 2.0f32;
        let outline: [(f32, f32); 6] = [
            (0.0, 0.0),
            (2.0, 0.0),
            (2.0, 1.0),
            (1.0, 1.0),
            (1.0, 2.0),
            (0.0, 2.0),
        ];
        let xy = |p: (f32, f32)| Vec3::new((p.0 - 1.0) * s, (p.1 - 1.0) * s, 0.0);
        let z0 = -h * 0.5;
        let z1 = h * 0.5;
        let at = |p: (f32, f32), z: f32| {
            let v = xy(p);
            Vec3::new(v.x, v.y, z)
        };
        // Caps from the two-rectangle decomposition of the L (A: lower full width, B: upper
        // left arm). Bottom faces -z, top faces +z.
        let rects: [[(f32, f32); 4]; 2] = [
            [(0.0, 0.0), (2.0, 0.0), (2.0, 1.0), (0.0, 1.0)],
            [(0.0, 1.0), (1.0, 1.0), (1.0, 2.0), (0.0, 2.0)],
        ];
        for r in rects {
            quad(
                at(r[0], z0),
                at(r[1], z0),
                at(r[2], z0),
                at(r[3], z0),
                Vec3::NEG_Z,
            );
            quad(
                at(r[0], z1),
                at(r[1], z1),
                at(r[2], z1),
                at(r[3], z1),
                Vec3::Z,
            );
        }
        // Side walls: one quad per perimeter edge, outward = the edge rotated −90° in xy.
        for i in 0..outline.len() {
            let a = outline[i];
            let b = outline[(i + 1) % outline.len()];
            let edge = (b.0 - a.0, b.1 - a.1);
            let outward = Vec3::new(edge.1, -edge.0, 0.0).normalize_or_zero();
            quad(at(a, z0), at(b, z0), at(b, z1), at(a, z1), outward);
        }
        (positions, indices)
    }

    /// Asserts the baked field's sign matches the `MeshBvh` oracle's inside/outside at every
    /// voxel away from the surface band (where the discretized sign legitimately flutters),
    /// returning the number of voxels compared. The whole-grid sweep catches an edge/corner
    /// or concave-corner sign flip that a few interior samples would miss.
    fn assert_sign_matches_oracle(sdf: &Sdf, positions: &[Vec3], indices: &[u32]) -> usize {
        let oracle = saffron_geometry::MeshBvh::build(positions, indices).expect("oracle");
        let [nx, ny, nz] = sdf.header.dims;
        let bmin = Vec3::from(sdf.header.bounds_min);
        let bmax = Vec3::from(sdf.header.bounds_max);
        let cell = (bmax - bmin) / Vec3::new(nx as f32, ny as f32, nz as f32);
        let band = cell.length();
        let mut compared = 0usize;
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let center =
                        bmin + Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5) * cell;
                    let analytic = oracle.nearest_signed_distance(center);
                    if analytic.abs() < band {
                        continue;
                    }
                    let got = sdf.sample_voxel(x, y, z);
                    assert_eq!(
                        got < 0.0,
                        analytic < 0.0,
                        "voxel ({x},{y},{z}) sign mismatch: gpu {got}, oracle {analytic}"
                    );
                    compared += 1;
                }
            }
        }
        compared
    }

    /// The concave-corner sign gate: an L-shaped prism's re-entrant corner is exactly where a
    /// single nearest-face normal can read an interior voxel as outside (a sky-leak). The
    /// outside-flood sign must agree with the `MeshBvh` oracle's inside/outside across the
    /// whole grid, including the notch. Skips when no Vulkan device is present.
    #[test]
    fn gpu_bake_sdf_signs_concave_corner_like_the_oracle() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        let (positions, indices) = l_prism_geometry();
        let fields = uploader
            .bake_sdf(&positions, &indices, &[], 1.0)
            .expect("gpu bake");
        // A small single-primitive mesh bakes exactly one field (below the chunk threshold).
        assert_eq!(fields.len(), 1, "a small mesh bakes one field");
        let sdf = &fields[0];
        assert!(sdf.header.occupied_bricks > 0, "the prism occupies bricks");
        let compared = assert_sign_matches_oracle(sdf, &positions, &indices);
        assert!(
            compared > 0,
            "no voxels were comparable away from the surface"
        );
        drop(uploader);
        device.wait_idle().expect("idle before teardown");
        drop(device);
    }

    /// The central Phase-2 gate: a small mesh bakes through the GPU jump-flood
    /// (voxelize/JFA) validation-clean, the SDST v2 `GpuSdf` claims + reclaims one
    /// slot in both bindless arrays, and the GPU-baked field agrees in **sign** with the CPU
    /// `MeshBvh` oracle at points away from the surface band. Skips when no Vulkan device is
    /// present.
    #[test]
    fn gpu_bake_sdf_is_validation_clean_signs_correctly_and_reclaims_its_slot() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");

        let (positions, indices) = box_geometry(Vec3::splat(-1.0), Vec3::splat(1.0));
        let fields = uploader
            .bake_sdf(&positions, &indices, &[], 1.0)
            .expect("gpu bake");
        assert_eq!(fields.len(), 1, "a small mesh bakes one field");
        let sdf = &fields[0];
        assert!(sdf.header.occupied_bricks > 0, "the box occupies bricks");

        // Sign gate: every voxel away from the surface band must match the CPU oracle's
        // inside/outside — not a handful of interior points, so an edge/corner sign flip is
        // caught (the box's exterior edge + corner voxels are sampled too).
        let compared = assert_sign_matches_oracle(sdf, &positions, &indices);
        assert!(
            compared > 0,
            "no voxels were comparable away from the surface"
        );

        let field = uploader.upload_sdf(&descriptors, sdf).expect("sdf upload");
        // The first SDF claims slot 0 of the SDF arrays (its own allocator, distinct from
        // the albedo array's reserved white slot 0).
        assert_eq!(field.bindless_index(), 0);
        assert_eq!(field.voxel_dims, sdf.header.dims);
        assert_eq!(field.indirection_dims, sdf.header.indirection_dims);
        assert_eq!(field.max_dist, sdf.header.max_dist);
        assert_eq!(field.mip_count, sdf.header.mip_count);
        assert_eq!(field.mip_count, saffron_geometry::SDF_MIP_COUNT);

        // A zero-dims field is rejected before any allocation.
        let mut empty = sdf.clone();
        empty.header.dims = [0, 0, 0];
        assert!(matches!(
            uploader.upload_sdf(&descriptors, &empty),
            Err(Error::ZeroSizedImage)
        ));

        // Dropping the field returns its slot to the shared SDF free-list (both arrays share
        // the one slot).
        let slot = field.bindless_index();
        drop(field);
        assert_eq!(free_list.lock().unwrap().as_slice(), &[] as &[u32]); // albedo free-list untouched
        assert_eq!(
            descriptors.sdf_free_list().lock().unwrap().as_slice(),
            &[slot]
        );

        drop(uploader);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the SDF bake + upload must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// The sidecar cache: a second `bake_or_load_sdf` of the same mesh reads the written
    /// `<hash>.sdfset` and returns fields byte-identical to the bake. Skips without a device.
    #[test]
    fn sdf_sidecar_cache_round_trips() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        let dir = std::env::temp_dir().join(format!("saffron-sdf-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bake = SdfBake {
            resolution_scale: 1.0,
            cache_dir: Some(dir.clone()),
        };
        let (positions, indices) = box_geometry(Vec3::splat(-0.5), Vec3::splat(0.5));

        let first = uploader
            .bake_or_load_sdf(&positions, &indices, &[], &bake)
            .expect("first bake writes the sidecar");
        // The sidecar exists now; the second call must read it back identically.
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("cache dir")
            .filter_map(std::result::Result::ok)
            .collect();
        assert_eq!(entries.len(), 1, "one sidecar written");
        let second = uploader
            .bake_or_load_sdf(&positions, &indices, &[], &bake)
            .expect("second load from sidecar");
        assert_eq!(
            first, second,
            "the cached fields round-trip byte-identically"
        );

        drop(uploader);
        device.wait_idle().expect("idle before teardown");
        drop(device);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The mip-chain length: 1×1 → 1, square powers of two → log2+1, and a non-square
    /// image uses the larger dimension.
    #[test]
    fn mip_count_matches_cpp() {
        assert_eq!(mip_count(1, 1), 1);
        assert_eq!(mip_count(2, 2), 2);
        assert_eq!(mip_count(256, 256), 9);
        assert_eq!(mip_count(1024, 512), 11);
        assert_eq!(mip_count(512, 1024), 11);
        assert_eq!(mip_count(7, 1), 3); // 7 -> 3 -> 1
    }

    #[test]
    fn explicit_mip_chain_requires_every_level_exactly_once() {
        let base = vec![0_u8; 7 * 3 * 4];
        let middle = vec![0_u8; 3 * 4];
        let tail = vec![0_u8; 4];
        let valid = [
            TextureMipLevel {
                rgba: &base,
                width: 7,
                height: 3,
            },
            TextureMipLevel {
                rgba: &middle,
                width: 3,
                height: 1,
            },
            TextureMipLevel {
                rgba: &tail,
                width: 1,
                height: 1,
            },
        ];
        assert!(valid_prefiltered_mip_chain(&valid));
        assert!(!valid_prefiltered_mip_chain(&valid[..2]));

        let extra = [valid[0], valid[1], valid[2], valid[2]];
        assert!(!valid_prefiltered_mip_chain(&extra));
        let malformed = [
            valid[0],
            TextureMipLevel {
                rgba: &tail,
                width: 2,
                height: 1,
            },
            valid[2],
        ];
        assert!(!valid_prefiltered_mip_chain(&malformed));
        assert!(!valid_prefiltered_mip_chain(&[]));
    }

    /// `float_to_half` reproduces the known IEEE half encodings: the exact
    /// representables, the f16-max overflow to +inf, and the sign bit. This is
    /// the load-bearing half of `upload_texture_float` (a wrong narrowing corrupts
    /// every HDR env source) and runs on any host.
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
