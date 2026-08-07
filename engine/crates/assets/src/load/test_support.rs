use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use saffron_core::Uuid;
use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{Mesh, Submesh, Vertex, save_mesh_to_buffer};
use saffron_rendering::{
    BindlessFreeList, Descriptors, Device, GpuMesh, GpuTexture, SurfaceSource, Uploader,
};
use saffron_scene::{AssetEntry, AssetType, Colorspace};

use crate::gpu::GpuUploader;
use crate::{AssetServer, ContainerMetadata, RendererUploader};

/// A unique scratch dir under the system temp, removed and recreated per test.
pub(super) fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("saffron-assets-load-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A single-triangle mesh, the payload every `.smesh` fixture encodes.
pub(super) fn triangle_mesh() -> Mesh {
    Mesh {
        vertices: vec![
            Vertex {
                position: Vec3::ZERO,
                normal: Vec3::Z,
                uv0: Vec2::ZERO,
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::X,
                normal: Vec3::Z,
                uv0: Vec2::new(1.0, 0.0),
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::Y,
                normal: Vec3::Z,
                uv0: Vec2::new(0.0, 1.0),
                ..Vertex::default()
            },
        ],
        indices: vec![0, 1, 2],
        submeshes: vec![Submesh {
            first_index: 0,
            index_count: 3,
            vertex_offset: 0,
            material_slot: 0,
        }],
    }
}

/// A 2×2 RGBA8 PNG (the encoded bytes the texture loaders decode).
pub(super) fn png_2x2() -> Vec<u8> {
    let buffer = image::RgbaImage::from_pixel(2, 2, image::Rgba([180, 120, 60, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    buffer
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

pub(super) fn encode_meta(meta: &ContainerMetadata) -> Vec<u8> {
    crate::encode_container_metadata(meta)
}

/// A `GpuUploader` that wraps the live renderer and counts mesh/texture uploads, so a
/// test can prove a cache hit skips the whole load (no re-decode, no re-upload).
pub(super) struct CountingUploader<'a> {
    inner: RendererUploader<'a>,
    pub(super) mesh_uploads: AtomicUsize,
    pub(super) texture_uploads: AtomicUsize,
    fail_mesh_uploads: bool,
}

impl<'a> CountingUploader<'a> {
    fn new(uploader: &'a Uploader, descriptors: &'a Descriptors) -> Self {
        Self {
            inner: RendererUploader::new(uploader, descriptors, true),
            mesh_uploads: AtomicUsize::new(0),
            texture_uploads: AtomicUsize::new(0),
            fail_mesh_uploads: false,
        }
    }

    /// Every mesh upload counts an attempt and then fails, for the negative-cache path.
    pub(super) fn fail_mesh_uploads(mut self) -> Self {
        self.fail_mesh_uploads = true;
        self
    }
}

impl GpuUploader for CountingUploader<'_> {
    fn upload_mesh(
        &self,
        mesh: &Mesh,
        hierarchy: &saffron_geometry::PortableVirtualHierarchy,
        skin: &[saffron_geometry::VertexSkin],
        morph: Option<&saffron_geometry::MorphData>,
        sdf: saffron_rendering::SdfSource<'_>,
    ) -> saffron_rendering::Result<Arc<GpuMesh>> {
        self.mesh_uploads.fetch_add(1, Ordering::SeqCst);
        if self.fail_mesh_uploads {
            return Err(saffron_rendering::Error::InvalidUploadData(
                "injected upload failure".to_owned(),
            ));
        }
        self.inner.upload_mesh(mesh, hierarchy, skin, morph, sdf)
    }

    fn upload_texture(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        srgb: bool,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.texture_uploads.fetch_add(1, Ordering::SeqCst);
        self.inner.upload_texture(rgba, width, height, srgb)
    }

    fn upload_texture_float(
        &self,
        rgba: &[f32],
        width: u32,
        height: u32,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.texture_uploads.fetch_add(1, Ordering::SeqCst);
        self.inner.upload_texture_float(rgba, width, height)
    }

    fn skinning_enabled(&self) -> bool {
        self.inner.skinning_enabled()
    }
}

/// A live headless device + uploader + descriptors.
pub(super) struct GpuFixture {
    pub(super) uploader: Uploader,
    pub(super) descriptors: Descriptors,
    device: Device,
}

/// The GPU fixture, or `None` (no Vulkan ICD) so the GPU-backed tests skip rather than fail
/// off-hardware.
pub(super) fn gpu_or_skip() -> Option<GpuFixture> {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping (no Vulkan device): {err}");
            return None;
        }
    };
    let free_list: BindlessFreeList = Arc::new(std::sync::Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
    let queue = device.graphics_queue.clone();
    let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
    Some(GpuFixture {
        uploader,
        descriptors,
        device,
    })
}

impl GpuFixture {
    pub(super) fn counting(&self) -> CountingUploader<'_> {
        CountingUploader::new(&self.uploader, &self.descriptors)
    }

    /// Idles the GPU, then drops the cached GPU `Arc`s (their `Drop` frees the VMA allocations and
    /// returns the bindless slots), then the borrowing sub-state (the uploader's command pool and
    /// the descriptors' set layouts borrow the device), then the device last.
    pub(super) fn teardown(self, mut assets: AssetServer) {
        let GpuFixture {
            device,
            descriptors,
            uploader,
        } = self;
        device.wait_idle().expect("idle before teardown");
        assets.clear_asset_caches();
        drop(assets);
        drop(uploader);
        drop(descriptors);
        drop(device);
    }
}

/// Writes a standalone `.smesh` under `<root>/meshes/<name>.smesh` and registers a
/// `Mesh` catalog row for `id`.
pub(super) fn write_standalone_mesh(assets: &mut AssetServer, id: Uuid, name: &str) {
    let rel = format!("meshes/{name}.smesh");
    let full = format!("{}/{rel}", assets.root.display());
    std::fs::create_dir_all(format!("{}/meshes", assets.root.display())).unwrap();
    std::fs::write(
        &full,
        save_mesh_to_buffer(&triangle_mesh(), &[], None).unwrap(),
    )
    .unwrap();
    assets.catalog.put(AssetEntry {
        id,
        name: name.to_owned(),
        asset_type: AssetType::Mesh,
        path: rel,
        chunk: -1,
        ..AssetEntry::default()
    });
}

/// Writes a standalone texture (the PNG bytes) under `<root>/textures/<name>.png` and
/// registers a `Texture` catalog row for `id` with the given colorspace provenance.
pub(super) fn write_standalone_texture(
    assets: &mut AssetServer,
    id: Uuid,
    name: &str,
    colorspace: Colorspace,
    hdr: bool,
    linear: bool,
) {
    let rel = format!("textures/{name}.png");
    let full = format!("{}/{rel}", assets.root.display());
    std::fs::write(&full, png_2x2()).unwrap();
    assets.catalog.put(AssetEntry {
        id,
        name: name.to_owned(),
        asset_type: AssetType::Texture,
        path: rel,
        chunk: -1,
        colorspace,
        hdr,
        linear,
        ..AssetEntry::default()
    });
}
