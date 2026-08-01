//! Device-backed fixtures shared by the mirror's submodule tests.

use super::*;
use glam::Vec3;
use saffron_rendering::{
    BindlessFreeList, Descriptors, Device, GpuSceneUploadLimits, SurfaceSource,
};
use saffron_scene::{AssetEntry, AssetType};

pub(crate) const WORLD: GpuSceneWorldId = GpuSceneWorldId(0);

pub(crate) struct GpuFixture {
    pub(crate) uploader: Uploader,
    pub(crate) descriptors: Descriptors,
    pub(crate) device: Device,
}

pub(crate) fn gpu_or_skip() -> Option<GpuFixture> {
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
    pub(crate) fn teardown(
        self,
        mirror: GpuSceneMirror,
        gpu_scene: PersistentGpuScene,
        gpu_data: GlobalGpuData,
        mut assets: AssetServer,
    ) {
        let GpuFixture {
            device,
            descriptors,
            uploader,
        } = self;
        device.wait_idle().expect("idle before teardown");
        drop(mirror);
        drop(gpu_scene);
        drop(gpu_data);
        assets.clear_asset_caches();
        drop(assets);
        drop(uploader);
        drop(descriptors);
        drop(device);
    }
}

pub(crate) fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "saffron-gpu-scene-mirror-{tag}-{}",
        Uuid::new().value()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes a standalone `.smesh` of one forward-facing triangle and registers a Mesh
/// catalog row for `id`.
pub(crate) fn write_triangle_mesh(assets: &mut AssetServer, id: Uuid, name: &str) {
    use saffron_geometry::glam::Vec2;
    use saffron_geometry::{Mesh, Submesh, Vertex, save_mesh_to_buffer};
    let mesh = Mesh {
        vertices: vec![
            Vertex {
                position: Vec3::new(-1.0, -1.0, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::ZERO,
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(1.0, -1.0, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::new(1.0, 0.0),
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(0.0, 1.0, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::new(0.5, 1.0),
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
    };
    let rel = format!("models/{name}.smesh");
    let full = format!("{}/{rel}", assets.root.display());
    std::fs::create_dir_all(format!("{}/models", assets.root.display())).unwrap();
    std::fs::write(&full, save_mesh_to_buffer(&mesh, &[], None).unwrap()).unwrap();
    assets.catalog.put(AssetEntry {
        id,
        name: name.to_owned(),
        asset_type: AssetType::Mesh,
        path: rel,
        chunk: -1,
        ..AssetEntry::default()
    });
}

/// Writes a `.smat` whose height slot displaces, plus the 4x4 height PNG it references, and
/// returns the material id a [`saffron_scene::MaterialSlot`] binds. Real displacement needs a
/// decodable height texture — `displace_info_from` returns `None` without one — so the texture
/// is written to disk rather than stubbed.
pub(crate) fn write_displacing_material(assets: &mut AssetServer, name: &str) -> Uuid {
    let texture_id = Uuid::new();
    let rel = format!("textures/{name}-height.png");
    std::fs::create_dir_all(format!("{}/textures", assets.root.display())).unwrap();
    let mut pixels = image::RgbaImage::new(4, 4);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        let height = ((x * 4 + y) * 16) as u8;
        *pixel = image::Rgba([height, height, height, 255]);
    }
    pixels
        .save(assets.root.join(&rel))
        .expect("write height png");
    assets.catalog.put(AssetEntry {
        id: texture_id,
        name: format!("{name}-height"),
        asset_type: AssetType::Texture,
        path: rel,
        chunk: -1,
        ..AssetEntry::default()
    });

    let material = crate::MaterialAsset {
        height_texture: texture_id,
        height_mode: saffron_core::HeightMode::Displacement,
        height_scale: 0.3,
        ..crate::MaterialAsset::default()
    };
    crate::save_material_asset(assets, &material, name, "").expect("write smat")
}

/// Field order is the drop order: every GPU-resource holder precedes `fixture`, so an
/// assertion unwind tears down buffers and textures before the device.
pub(crate) struct MirrorHarness {
    pub(crate) mirror: GpuSceneMirror,
    pub(crate) gpu_scene: PersistentGpuScene,
    pub(crate) gpu_data: GlobalGpuData,
    pub(crate) pending: GpuScenePendingUploads,
    pub(crate) residency: PageResidency,
    pub(crate) default_white: Arc<GpuTexture>,
    pub(crate) assets: AssetServer,
    pub(crate) fixture: GpuFixture,
}

pub(crate) fn harness(tag: &str) -> Option<MirrorHarness> {
    let fixture = gpu_or_skip()?;
    let assets = AssetServer::new(scratch(tag));
    let gpu_data = GlobalGpuData::new(&fixture.device).expect("GlobalGpuData::new");
    let mut gpu_scene =
        PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene mirror");
    gpu_scene.create_world(WORLD).expect("create world");
    let default_white = fixture
        .uploader
        .upload_default_white(&fixture.descriptors)
        .expect("default white");
    Some(MirrorHarness {
        fixture,
        assets,
        gpu_data,
        gpu_scene,
        pending: GpuScenePendingUploads::default(),
        residency: PageResidency::default(),
        default_white,
        mirror: GpuSceneMirror::new(),
    })
}

impl MirrorHarness {
    pub(crate) fn sync(&mut self, scene: &mut Scene) {
        let gpu = RendererUploader::new(&self.fixture.uploader, &self.fixture.descriptors, false);
        let mut target = GpuSceneMirrorTarget {
            gpu_data: &mut self.gpu_data,
            gpu_scene: &mut self.gpu_scene,
            default_white: &self.default_white,
            pending: &mut self.pending,
            residency: &mut self.residency,
        };
        self.mirror
            .sync_world(WORLD, scene, &mut self.assets, &gpu, &mut target)
            .expect("mirror sync");
    }

    pub(crate) fn drive(&mut self, scene: &Scene) {
        let mut target = GpuSceneMirrorTarget {
            gpu_data: &mut self.gpu_data,
            gpu_scene: &mut self.gpu_scene,
            default_white: &self.default_white,
            pending: &mut self.pending,
            residency: &mut self.residency,
        };
        self.mirror
            .drive_page_streaming(WORLD, scene, None, &mut target)
            .expect("drive page streaming");
        self.residency
            .publish_ready(&mut self.gpu_data, &mut self.pending)
            .expect("publish ready pages");
    }

    pub(crate) fn finish(self) {
        let MirrorHarness {
            fixture,
            assets,
            gpu_data,
            gpu_scene,
            pending,
            residency,
            default_white,
            mirror,
        } = self;
        drop(pending);
        drop(residency);
        drop(default_white);
        fixture.teardown(mirror, gpu_scene, gpu_data, assets);
    }
}

/// A two-prototype assembly family: each single-triangle prototype placed once at
/// identity, plus the flattened family mesh (prototype streams concatenated in id
/// order).
pub(crate) fn assembly_family_fixture() -> (
    saffron_geometry::Mesh,
    saffron_geometry::PortableVirtualHierarchy,
) {
    use saffron_geometry::glam::Vec2;
    use saffron_geometry::{
        Mesh, PortableAggregationMode, PortableHierarchyInput, PortableSourceMesh,
        PortableSourceSubmesh, PortableSourceVertex, Submesh, Vertex, VirtualHierarchyMaterial,
        cook_portable_virtual_hierarchy,
    };
    let quantized = |x: f32, y: f32| PortableSourceVertex {
        position_bits: [(x * 65_536.0) as i32, (y * 65_536.0) as i32, 0],
        normal_snorm: [0, 0, 32_767],
        uv_bits: [0, 0],
        tangent_snorm: [32_767, 0, 0, 32_767],
    };
    let source_mesh = |source: u128| PortableSourceMesh {
        source,
        selector_hash: [source as u8; 32],
        vertices: vec![
            quantized(-1.0, -1.0),
            quantized(1.0, -1.0),
            quantized(0.0, 1.0),
        ],
        indices: vec![0, 1, 2],
        submeshes: vec![PortableSourceSubmesh {
            first_index: 0,
            index_count: 3,
            material: VirtualHierarchyMaterial::opaque(0),
        }],
        skin: Vec::new(),
        aggregation: PortableAggregationMode::Contiguous,
    };
    const IDENTITY: [i32; 16] = [
        65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536,
    ];
    let bounds = saffron_geometry::PortableBounds {
        min_bits: [-2 * 65_536; 3],
        max_bits: [2 * 65_536; 3],
    };
    let input = PortableHierarchyInput {
        combinations: Vec::new(),
        meshes: vec![source_mesh(1), source_mesh(2)],
        micro_instances: vec![
            saffron_geometry::MicroInstance {
                part: 1,
                prototype: 0,
                transform_bits: IDENTITY,
            },
            saffron_geometry::MicroInstance {
                part: 2,
                prototype: 1,
                transform_bits: IDENTITY,
            },
        ],
        deformation: Vec::new(),
        bounds,
        root_material: VirtualHierarchyMaterial::opaque(0),
        deformation_padding: 0,
    };
    let hierarchy = cook_portable_virtual_hierarchy(&input).expect("cook family hierarchy");

    let flat_vertex = |x: f32, y: f32| Vertex {
        position: Vec3::new(x, y, 0.0),
        normal: Vec3::Z,
        uv0: Vec2::ZERO,
        tangent: [1.0, 0.0, 0.0, 1.0],
    };
    let flat = Mesh {
        vertices: vec![
            flat_vertex(-1.0, -1.0),
            flat_vertex(1.0, -1.0),
            flat_vertex(0.0, 1.0),
            flat_vertex(-1.0, -1.0),
            flat_vertex(1.0, -1.0),
            flat_vertex(0.0, 1.0),
        ],
        indices: vec![0, 1, 2, 3, 4, 5],
        submeshes: vec![
            Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            },
            Submesh {
                first_index: 3,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            },
        ],
    };
    (flat, hierarchy)
}

/// A distinctive authored response, so a prototype record that carries it cannot be confused
/// with the all-zero "not a plant family" value.
pub(crate) fn mechanics_fixture() -> saffron_vegetation::MechanicalResponse {
    saffron_vegetation::MechanicalResponse {
        stiffness: saffron_spatial::DecisionScalar::from_bits(3 << 16),
        damping: saffron_spatial::UnitInterval::from_bits(9_000),
        drag: saffron_spatial::DecisionScalar::from_bits(2 << 16),
        flutter: saffron_spatial::DecisionScalar::from_bits(5 << 16),
        bend_limit: saffron_spatial::UnitInterval::from_bits(21_000),
        damage_threshold: saffron_spatial::DecisionScalar::from_bits(0),
        break_threshold: saffron_spatial::DecisionScalar::from_bits(0),
    }
}
