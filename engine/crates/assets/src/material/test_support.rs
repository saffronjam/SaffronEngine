use saffron_core::{HeightMode, Uuid};
use saffron_geometry::glam::{Vec2, Vec3, Vec4};
use saffron_vegetation::MaterialSurface;

use crate::AssetServer;

use super::{MaterialAsset, empty_object};

/// A material with every field set away from its default, for round-trip coverage.
pub(super) fn populated_material() -> MaterialAsset {
    MaterialAsset {
        surface: MaterialSurface::Standard,
        shader: "mesh".to_owned(),
        blend: "masked".to_owned(),
        unlit: true,
        double_sided: true,
        base_color: Vec4::new(0.1, 0.2, 0.3, 0.4),
        metallic: 0.6,
        roughness: 0.25,
        emissive: Vec3::new(1.5, 2.5, 3.5),
        emissive_strength: 4.0,
        normal_strength: 0.75,
        alpha_cutoff: 0.33,
        height_scale: 0.125,
        height_mode: HeightMode::Displacement,
        uv_tiling: Vec2::new(2.0, 3.0),
        uv_offset: Vec2::new(0.25, 0.5),
        albedo_texture: Uuid(1001),
        orm_texture: Uuid(1002),
        normal_texture: Uuid(1003),
        emissive_texture: Uuid(1004),
        height_texture: Uuid(1005),
        vector_displacement_texture: Uuid(1006),
        normal_convention: "dx".to_owned(),
        features: 0,
        graph: empty_object(),
        parent: Uuid(0),
        overrides: empty_object(),
    }
}

/// A scratch [`AssetServer`] rooted under a per-test temp dir.
pub(super) fn scratch_server(tag: &str) -> (AssetServer, std::path::PathBuf) {
    let tmp = std::env::temp_dir().join(format!(
        "saffron-material-test-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    let root = tmp.join("project").join("assets");
    (AssetServer::new(&root), tmp)
}
