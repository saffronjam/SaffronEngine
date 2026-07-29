//! The GPU-upload seam the resolve/load paths reach through.
//!
//! Rendering owns the upload calls (README §1); this crate reaches them through the
//! [`GpuUploader`] trait so the loaders are one code path over either the live renderer
//! or a test stub — the upload is genuinely performed by rendering's ash seam, not
//! stubbed in the engine.
//!
//! The trait carries exactly the three upload entry points the loaders need plus the
//! `skinning_enabled` gate `render_scene` reads (the skinned draw path is byte-identical
//! to a build without it when off). Errors surface as [`saffron_rendering::Error`]; the
//! loaders turn a failure into a logged warn plus a negative-cache `None`, never an `Err`.

use std::sync::Arc;

use saffron_geometry::{Mesh, MorphData, PortableVirtualHierarchy, VertexSkin};
use saffron_rendering::{Descriptors, GpuMesh, GpuTexture, SdfSource, TextureMipLevel, Uploader};

/// The GPU-facing operations the resolve/load paths drive.
///
/// Implemented by the live renderer ([`RendererUploader`]) and by test stubs; the
/// loaders depend only on this trait, so the get-or-negative-cache logic is exercised
/// without a Vulkan device while the production path still performs the real upload.
pub trait GpuUploader {
    /// Uploads a mesh and its canonical portable hierarchy (plus an optional parallel
    /// [`VertexSkin`] stream) into device-local buffers, returning the shared [`GpuMesh`].
    ///
    /// When `sdf_bake` is present the per-mesh signed distance field is GPU jump-flood baked
    /// (or read from the sidecar cache) from the mesh geometry, uploaded into the bindless
    /// SDF arrays, and tied to the returned mesh's lifetime. A `None` request bakes no field
    /// (the gizmo/preview meshes).
    ///
    /// # Errors
    ///
    /// Propagates the renderer's upload failure (empty mesh, skin mismatch, or a
    /// failing Vulkan/VMA call).
    fn upload_mesh(
        &self,
        mesh: &Mesh,
        hierarchy: &PortableVirtualHierarchy,
        skin: &[VertexSkin],
        morph: Option<&MorphData>,
        sdf: SdfSource<'_>,
    ) -> saffron_rendering::Result<Arc<GpuMesh>>;

    /// Uploads tightly packed RGBA8 (already decoded by the caller) as an sRGB or unorm
    /// sampled texture.
    ///
    /// # Errors
    ///
    /// Propagates the renderer's upload failure (zero extent or a failing Vulkan/VMA
    /// call).
    fn upload_texture(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        srgb: bool,
    ) -> saffron_rendering::Result<Arc<GpuTexture>>;

    /// Uploads a complete prefiltered RGBA8 mip chain.
    ///
    /// The default uploads level zero through [`Self::upload_texture`], which keeps
    /// non-GPU test doubles small. Live renderers override it and preserve every level.
    fn upload_texture_mips(
        &self,
        mips: &[TextureMipLevel<'_>],
        srgb: bool,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        let Some(base) = mips.first() else {
            return Err(saffron_rendering::Error::ZeroSizedImage);
        };
        self.upload_texture(base.rgba, base.width, base.height, srgb)
    }

    /// Uploads tightly packed linear-float RGBA as an `R16G16B16A16_SFLOAT` sampled
    /// texture (HDR panoramas / env sources).
    ///
    /// # Errors
    ///
    /// Propagates the renderer's upload failure (zero extent or a failing Vulkan/VMA
    /// call).
    fn upload_texture_float(
        &self,
        rgba: &[f32],
        width: u32,
        height: u32,
    ) -> saffron_rendering::Result<Arc<GpuTexture>>;

    /// Uploads tightly packed RGBA8 as an unorm sampled texture (like
    /// [`Self::upload_texture`] with `srgb = false` — a height map is linear data) *and* builds its
    /// per-height min/max pyramid into the parallel bindless array, so the adaptive-tessellation factor
    /// kernel refines per-region. Called for a displacement material's height slot.
    ///
    /// The default delegates to [`Self::upload_texture`] and builds no pyramid — the fallback for a test
    /// stub without a real GPU. The live uploader overrides it to build the pyramid.
    ///
    /// # Errors
    ///
    /// Propagates the renderer's upload failure (zero extent or a failing Vulkan/VMA call).
    fn upload_height_texture(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.upload_texture(rgba, width, height, false)
    }

    /// Uploads a creative look-up table — `size³` red-fastest `[r, g, b]` triples — as an
    /// `R16G16B16A16_SFLOAT` `TYPE_3D` sampled image, returning the [`GpuLut`] the tonemap pass binds.
    ///
    /// The default errors ([`saffron_rendering::Error::ZeroSizedImage`]) — a non-GPU test stub never
    /// imports a LUT. The live uploaders override it with the real 3D upload.
    ///
    /// # Errors
    ///
    /// Propagates the renderer's upload failure (zero size or a failing Vulkan/VMA call).
    fn upload_lut_3d(
        &self,
        _rgb: &[[f32; 3]],
        _size: u32,
    ) -> saffron_rendering::Result<Arc<saffron_rendering::GpuLut>> {
        Err(saffron_rendering::Error::ZeroSizedImage)
    }

    /// Whether the compute-skinning path is built and on. The skinned draw list is
    /// gathered only when this is true, so a build with skinning off is byte-identical
    /// to one without the skinned path.
    fn skinning_enabled(&self) -> bool;

    /// Temporal phase used by canonical stochastic coverage for the active frame.
    fn coverage_temporal_phase(&self) -> u32 {
        0
    }
}

/// The live-renderer [`GpuUploader`]: an [`Uploader`] (its own one-off command pool +
/// the shared graphics queue) plus the renderer's [`Descriptors`] for the bindless
/// texture binds, and the skinning-enabled flag.
///
/// Borrows the descriptors for its lifetime — the loaders hold it transiently for a
/// resolve/render pass, never across a project switch.
pub struct RendererUploader<'a> {
    uploader: &'a Uploader,
    descriptors: &'a Descriptors,
    skinning_enabled: bool,
}

impl<'a> RendererUploader<'a> {
    /// Wraps the renderer's uploader + descriptors for the resolve/load paths.
    pub fn new(
        uploader: &'a Uploader,
        descriptors: &'a Descriptors,
        skinning_enabled: bool,
    ) -> Self {
        Self {
            uploader,
            descriptors,
            skinning_enabled,
        }
    }
}

impl GpuUploader for RendererUploader<'_> {
    fn upload_mesh(
        &self,
        mesh: &Mesh,
        hierarchy: &PortableVirtualHierarchy,
        skin: &[VertexSkin],
        morph: Option<&MorphData>,
        sdf: SdfSource<'_>,
    ) -> saffron_rendering::Result<Arc<GpuMesh>> {
        self.uploader
            .upload_mesh(self.descriptors, mesh, hierarchy, skin, morph, sdf)
    }

    fn upload_texture(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        srgb: bool,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.uploader
            .upload_texture(self.descriptors, rgba, width, height, srgb)
    }

    fn upload_texture_float(
        &self,
        rgba: &[f32],
        width: u32,
        height: u32,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.uploader
            .upload_texture_float(self.descriptors, rgba, width, height)
    }

    fn upload_texture_mips(
        &self,
        mips: &[TextureMipLevel<'_>],
        srgb: bool,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.uploader
            .upload_texture_mips(self.descriptors, mips, srgb)
    }

    fn upload_height_texture(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.uploader
            .upload_height_texture(self.descriptors, rgba, width, height)
    }

    fn upload_lut_3d(
        &self,
        rgb: &[[f32; 3]],
        size: u32,
    ) -> saffron_rendering::Result<Arc<saffron_rendering::GpuLut>> {
        self.uploader.upload_lut_3d(rgb, size)
    }

    fn skinning_enabled(&self) -> bool {
        self.skinning_enabled
    }
}
