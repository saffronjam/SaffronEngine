//! Recording and device-backed fixtures shared by the driver tests.

use std::cell::RefCell;
use std::path::PathBuf;

use saffron_rendering::{
    BindlessFreeList, Descriptors, Device, FogRenderSettings, GpuTexture, SurfaceSource, Uploader,
};
use saffron_scene::{AssetEntry, AssetType};

use super::*;

/// One submitted deformation work item: `(entity, skinned, model, joint_offset, joint_count)`.
pub(super) type WorkFact = (u64, bool, Mat4, u32, u32);

/// One recorded setter call, in the exact order [`render_scene`] issues them, so a test can
/// assert the byte-identical setter sequence the skinning gate must preserve.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Call {
    SpotShadow {
        index: u32,
        casting: bool,
    },
    PointShadow {
        index: u32,
        casting: bool,
        far: f32,
    },
    DirectionalShadow {
        casting: bool,
    },
    RtScene {
        static_count: usize,
    },
    DdgiScene,
    ReflectionProbes(usize),
    FogVolumes(usize),
    SceneLighting {
        light_count: usize,
    },
    EnvBake(EnvSource),
    ClusterCamera,
    SsaoCamera,
    ShowGrid(bool),
    Deformations {
        work_count: usize,
        joint_count: usize,
    },
    Sky {
        mode: u32,
    },
    Clouds {
        enabled: bool,
    },
    Fog {
        enabled: bool,
    },
}

/// A recording [`SceneRenderer`]: records every setter call and optionally backs the
/// upload path with a live [`Uploader`] so the draw-list
/// and pick tests resolve real `Arc<GpuMesh>`. Without a GPU the upload methods are never
/// reached by the early-out and light tests (no mesh resolves), so those run off-hardware.
pub(super) struct RecordingRenderer<'a> {
    width: u32,
    height: u32,
    skinning: bool,
    gpu: Option<(&'a Uploader, &'a Descriptors)>,
    calls: RefCell<Vec<Call>>,
    /// The static RT inputs captured by the last `set_rt_scene`, for the split assert.
    pub(super) rt_inputs: RefCell<Vec<saffron_rendering::RtInstanceInput>>,
    /// Per submitted work item.
    pub(super) work_facts: RefCell<Vec<WorkFact>>,
}

impl<'a> RecordingRenderer<'a> {
    pub(super) fn new(width: u32, height: u32, skinning: bool) -> Self {
        Self {
            width,
            height,
            skinning,
            gpu: None,
            calls: RefCell::new(Vec::new()),
            rt_inputs: RefCell::new(Vec::new()),
            work_facts: RefCell::new(Vec::new()),
        }
    }

    pub(super) fn with_gpu(mut self, uploader: &'a Uploader, descriptors: &'a Descriptors) -> Self {
        self.gpu = Some((uploader, descriptors));
        self
    }

    pub(super) fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }
}

impl GpuUploader for RecordingRenderer<'_> {
    fn upload_mesh(
        &self,
        mesh: &saffron_geometry::Mesh,
        hierarchy: &saffron_geometry::PortableVirtualHierarchy,
        skin: &[saffron_geometry::VertexSkin],
        morph: Option<&saffron_geometry::MorphData>,
        sdf: saffron_rendering::SdfSource<'_>,
    ) -> saffron_rendering::Result<Arc<GpuMesh>> {
        let (uploader, descriptors) = self.gpu.expect("upload_mesh needs a GPU fixture");
        uploader.upload_mesh(descriptors, mesh, hierarchy, skin, morph, sdf)
    }

    fn upload_texture(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        srgb: bool,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        let (uploader, descriptors) = self.gpu.expect("upload_texture needs a GPU fixture");
        uploader.upload_texture(descriptors, rgba, width, height, srgb)
    }

    fn upload_texture_float(
        &self,
        rgba: &[f32],
        width: u32,
        height: u32,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        let (uploader, descriptors) = self.gpu.expect("upload_texture_float needs a GPU fixture");
        uploader.upload_texture_float(descriptors, rgba, width, height)
    }

    fn skinning_enabled(&self) -> bool {
        self.skinning
    }
}

impl SceneRenderer for RecordingRenderer<'_> {
    fn viewport_width(&self) -> u32 {
        self.width
    }
    fn viewport_height(&self) -> u32 {
        self.height
    }
    fn jitter_offset(&self) -> Vec2 {
        Vec2::ZERO
    }
    fn set_spot_shadow(&mut self, _view_proj: Mat4, light_index: u32, casting: bool) {
        self.calls.borrow_mut().push(Call::SpotShadow {
            index: light_index,
            casting,
        });
    }
    fn set_point_shadow(&mut self, _pos: Vec3, far: f32, light_index: u32, casting: bool) {
        self.calls.borrow_mut().push(Call::PointShadow {
            index: light_index,
            casting,
            far,
        });
    }
    fn set_directional_shadow(&mut self, casting: bool) {
        self.calls
            .borrow_mut()
            .push(Call::DirectionalShadow { casting });
    }
    fn set_rt_scene(&mut self, instances: Arc<[saffron_rendering::RtInstanceInput]>) {
        self.calls.borrow_mut().push(Call::RtScene {
            static_count: instances.len(),
        });
        *self.rt_inputs.borrow_mut() = instances.to_vec();
    }
    fn set_ddgi_scene(
        &mut self,
        _cam_pos: Vec3,
        _sun_dir: Vec3,
        _sun_color: Vec3,
        _sun_intensity: f32,
    ) {
        self.calls.borrow_mut().push(Call::DdgiScene);
    }
    fn record_rt_culled(&mut self, _culled: u32) {}
    fn submit_reflection_probes(&mut self, probes: &[ReflectionProbeUpload]) {
        self.calls
            .borrow_mut()
            .push(Call::ReflectionProbes(probes.len()));
    }
    fn submit_fog_volumes(&mut self, volumes: &[FogVolumeUpload]) {
        self.calls
            .borrow_mut()
            .push(Call::FogVolumes(volumes.len()));
    }
    fn set_scene_lighting(&mut self, scene: &SceneLighting) -> saffron_rendering::Result<()> {
        self.calls.borrow_mut().push(Call::SceneLighting {
            light_count: scene.lights.len(),
        });
        Ok(())
    }
    fn request_env_bake(
        &mut self,
        source: EnvSource,
        _panorama: Option<Arc<GpuTexture>>,
        _params: SkygenParams,
    ) {
        self.calls.borrow_mut().push(Call::EnvBake(source));
    }
    fn set_cluster_camera(&mut self, _camera: ClusterCamera) {
        self.calls.borrow_mut().push(Call::ClusterCamera);
    }
    fn set_ssao_camera(&mut self, _view: Mat4, _proj: Mat4, _sun: Vec3) {
        self.calls.borrow_mut().push(Call::SsaoCamera);
    }
    fn set_show_grid(&mut self, enabled: bool) {
        self.calls.borrow_mut().push(Call::ShowGrid(enabled));
    }
    fn record_scene_gather(&mut self, _elapsed: Duration, _entities: u32) {}
    fn submit_deformations(
        &mut self,
        _view_proj: Mat4,
        work: &[saffron_rendering::DeformationWork],
        joints: &[Mat4],
    ) -> saffron_rendering::Result<()> {
        self.calls.borrow_mut().push(Call::Deformations {
            work_count: work.len(),
            joint_count: joints.len(),
        });
        *self.work_facts.borrow_mut() = work
            .iter()
            .map(|item| {
                (
                    item.entity,
                    item.skinned,
                    item.model,
                    item.joint_offset,
                    item.joint_count,
                )
            })
            .collect();
        Ok(())
    }
    fn submit_sky(&mut self, settings: &SkyRenderSettings) {
        self.calls.borrow_mut().push(Call::Sky {
            mode: settings.mode,
        });
    }
    fn submit_clouds(&mut self, settings: CloudRenderSettings) {
        self.calls.borrow_mut().push(Call::Clouds {
            enabled: settings.enabled,
        });
    }
    fn set_exposure(&mut self, _ev: f32) {}
    fn set_night_factor(&mut self, _factor: f32) {}
    fn submit_fog(&mut self, settings: &FogRenderSettings) {
        self.calls.borrow_mut().push(Call::Fog {
            enabled: settings.enabled,
        });
    }
}

/// A standard test camera (un-flipped projection; `render_scene` applies the Y-flip).
pub(super) fn test_camera() -> CameraView {
    CameraView {
        view: Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y),
        fov: 45.0,
        near_plane: 0.1,
        far_plane: 100.0,
    }
}

pub(super) fn scratch_server(tag: &str) -> (AssetServer, PathBuf) {
    let tmp =
        std::env::temp_dir().join(format!("saffron-render-scene-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let root = tmp.join("project").join("assets");
    (AssetServer::new(&root), tmp)
}

/// A live headless device + uploader + descriptors.
pub(super) struct GpuFixture {
    pub(super) uploader: Uploader,
    pub(super) descriptors: Descriptors,
    device: Device,
}

/// The GPU fixture, or `None` (no Vulkan ICD) so the device-backed tests skip rather than fail
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

/// Writes a standalone `.smesh` of a single forward-facing triangle (centered on the
/// origin, +Z normal) and registers a Mesh catalog row for `id`.
pub(super) fn write_triangle_mesh(assets: &mut AssetServer, id: saffron_core::Uuid, name: &str) {
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

/// Writes a skinned `.smesh` (a one-bone triangle, all weight on joint 0) + a Mesh row.
pub(super) fn write_skinned_triangle(assets: &mut AssetServer, id: saffron_core::Uuid, name: &str) {
    use saffron_geometry::glam::Vec2;
    use saffron_geometry::{Mesh, Submesh, Vertex, VertexSkin, save_mesh_to_buffer};
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
    // All weight on joint 0 so CPU skinning by an identity palette is the rest pose.
    let skin = vec![
        VertexSkin {
            joints: [0, 0, 0, 0],
            weights: [1.0, 0.0, 0.0, 0.0],
        };
        3
    ];
    let rel = format!("models/{name}.smesh");
    std::fs::create_dir_all(format!("{}/models", assets.root.display())).unwrap();
    std::fs::write(
        format!("{}/{rel}", assets.root.display()),
        save_mesh_to_buffer(&mesh, &skin, None).unwrap(),
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

/// Builds a one-bone skinned entity (bone at the origin, inverse-bind identity) and runs
/// the relink so `bone_handles` resolves. Returns the skinned entity.
pub(super) fn spawn_one_bone_skin(scene: &mut Scene, mesh_id: saffron_core::Uuid) -> Entity {
    let bone = scene.create_entity("Bone");
    let bone_uuid = scene
        .component::<saffron_scene::IdComponent>(bone)
        .unwrap()
        .id;
    let e = scene.create_entity("Rig");
    scene
        .add_component(
            e,
            SkinnedMesh {
                mesh: mesh_id,
                root_bone: bone_uuid,
                bones: vec![bone_uuid],
                inverse_bind: vec![Mat4::IDENTITY],
                bone_handles: Vec::new(),
            },
        )
        .unwrap();
    scene.relink_hierarchy();
    e
}
