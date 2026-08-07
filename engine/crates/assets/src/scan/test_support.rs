use std::path::PathBuf;

use saffron_core::Uuid;
use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{ImportedMaterial, ImportedModel, Mesh, Submesh, Vertex};
use saffron_rendering::{BindlessFreeList, Descriptors, Device, SurfaceSource, Uploader};

use crate::AssetServer;
use crate::import::ImportOptions;

/// A unique scratch dir under the system temp, removed and recreated per test.
pub(super) fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("saffron-assets-scan-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A single-triangle mesh.
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

/// A graph with one material (no textures) so the bake writes a small container.
pub(super) fn flat_graph() -> ImportedModel {
    ImportedModel {
        origin: Default::default(),
        nodes: vec![saffron_geometry::ImportedNode {
            name: "mesh".to_owned(),
            mesh: Some(triangle_mesh()),
            ..saffron_geometry::ImportedNode::default()
        }],
        materials: vec![ImportedMaterial {
            name: "flat".to_owned(),
            ..ImportedMaterial::default()
        }],
        animations: Vec::new(),
        skin: None,
        morph: None,
    }
}

/// Bakes a `.smodel` under the asset root (no catalog rows added — the scan rediscovers
/// it) and returns its project-relative path + model id.
pub(super) fn bake_fixture(assets: &AssetServer, source: &str) -> (Uuid, String) {
    let bake = assets
        .bake_model(&flat_graph(), ImportOptions::default(), source, Uuid(0))
        .expect("bake");
    (bake.model_id, bake.path)
}

/// A 2x2 RGBA8 PNG (the encoded bytes the texture register decodes).
pub(super) fn png_2x2() -> Vec<u8> {
    let buffer = image::RgbaImage::from_pixel(2, 2, image::Rgba([180, 120, 60, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    buffer
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

pub(super) struct GpuFixture {
    pub(super) uploader: Uploader,
    pub(super) descriptors: Descriptors,
    device: Device,
}

pub(super) fn gpu_or_skip() -> Option<GpuFixture> {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping (no Vulkan device): {err}");
            return None;
        }
    };
    let free_list: BindlessFreeList = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
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
