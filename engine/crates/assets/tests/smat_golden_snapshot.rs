//! The `.smat` byte-exact golden snapshot: a byte compare against a fixture in
//! `fixtures/golden/gen/`, which carries the f64-promoted float formatting and sorted keys
//! `material_asset_to_json` + `dump_json_sorted` must reproduce. A `.smat` byte shift is otherwise
//! silent — the editor parses the file wrong without erroring. Reseed with `UPDATE_GOLDEN=1` only on
//! an intentional format change.

use saffron_assets::{MaterialAsset, material_asset_to_text};
use saffron_core::{HeightMode, Uuid};
use saffron_geometry::glam::{Vec2, Vec3, Vec4};
use saffron_test_support::assert_bytes_match_golden;
use saffron_vegetation::MaterialSurface;

/// The populated material the golden fixture covers, field-for-field. `graph`/`overrides` are
/// `Null` so `material_asset_to_json` emits `{}`.
fn populated_material() -> MaterialAsset {
    MaterialAsset {
        surface: MaterialSurface::Standard,
        shader: "mesh".to_owned(),
        blend: "masked".to_owned(),
        unlit: false,
        double_sided: true,
        normal_convention: "gl".to_owned(),
        base_color: Vec4::new(0.8, 0.4, 0.2, 1.0),
        metallic: 0.25,
        roughness: 0.7,
        emissive: Vec3::new(0.1, 0.0, 0.0),
        emissive_strength: 2.0,
        normal_strength: 1.0,
        alpha_cutoff: 0.5,
        height_scale: 0.05,
        height_mode: HeightMode::Bump,
        uv_tiling: Vec2::new(2.0, 2.0),
        uv_offset: Vec2::new(0.0, 0.0),
        albedo_texture: Uuid(4242),
        orm_texture: Uuid(0),
        normal_texture: Uuid(4243),
        emissive_texture: Uuid(0),
        height_texture: Uuid(0),
        vector_displacement_texture: Uuid(0),
        features: 0,
        graph: serde_json::Value::Null,
        parent: Uuid(1024),
        overrides: serde_json::Value::Null,
    }
}

#[test]
fn populated_smat_bytes_match_golden() {
    let text = material_asset_to_text(&populated_material(), 2);
    assert_bytes_match_golden("material.smat", text.as_bytes());
}
