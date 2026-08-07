use saffron_geometry::glam::{Vec2, Vec3, Vec4};
use saffron_json::Value;

use super::MaterialAsset;
use super::codec::uuid_from_value;

/// Applies a sparse override map `{ field: value }` onto a base material — the instance
/// path.
///
/// Writes only the named, well-typed fields and leaves the rest untouched. A non-object
/// `overrides` is a no-op. The field set is the material's exposed parameters (see
/// [`pbr_exposed_parameters`](crate::pbr_exposed_parameters)): the scalar factors, the two colour
/// vectors, the two UV vectors, the three flags/enum, and the five texture ids.
pub fn apply_overrides(material: &mut MaterialAsset, overrides: &Value) {
    let Some(map) = overrides.as_object() else {
        return;
    };
    for (field, value) in map {
        match field.as_str() {
            "baseColor" => {
                if let Some([r, g, b, a]) = read_array4(value) {
                    material.base_color = Vec4::new(r, g, b, a);
                }
            }
            "emissive" => {
                if let Some([r, g, b]) = read_array3(value) {
                    material.emissive = Vec3::new(r, g, b);
                }
            }
            "uvTiling" => {
                if let Some([x, y]) = read_array2(value) {
                    material.uv_tiling = Vec2::new(x, y);
                }
            }
            "uvOffset" => {
                if let Some([x, y]) = read_array2(value) {
                    material.uv_offset = Vec2::new(x, y);
                }
            }
            "metallic" => {
                if let Some(v) = value.as_f64() {
                    material.metallic = v as f32;
                }
            }
            "roughness" => {
                if let Some(v) = value.as_f64() {
                    material.roughness = v as f32;
                }
            }
            "emissiveStrength" => {
                if let Some(v) = value.as_f64() {
                    material.emissive_strength = v as f32;
                }
            }
            "normalStrength" => {
                if let Some(v) = value.as_f64() {
                    material.normal_strength = v as f32;
                }
            }
            "alphaCutoff" => {
                if let Some(v) = value.as_f64() {
                    material.alpha_cutoff = v as f32;
                }
            }
            "heightScale" => {
                if let Some(v) = value.as_f64() {
                    material.height_scale = v as f32;
                }
            }
            "unlit" => {
                if let Some(v) = value.as_bool() {
                    material.unlit = v;
                }
            }
            "doubleSided" => {
                if let Some(v) = value.as_bool() {
                    material.double_sided = v;
                }
            }
            "blend" => {
                if let Some(v) = value.as_str() {
                    material.blend = v.to_owned();
                }
            }
            "albedoTexture" => material.albedo_texture = uuid_from_value(value),
            "ormTexture" => material.orm_texture = uuid_from_value(value),
            "normalTexture" => material.normal_texture = uuid_from_value(value),
            "emissiveTexture" => material.emissive_texture = uuid_from_value(value),
            "heightTexture" => material.height_texture = uuid_from_value(value),
            "vectorDisplacementTexture" => {
                material.vector_displacement_texture = uuid_from_value(value)
            }
            _ => {}
        }
    }
}

/// Reads a 4-element `f32` array from a JSON value (with at least 4 numbers), for the
/// override path's `baseColor`.
fn read_array4(value: &Value) -> Option<[f32; 4]> {
    let array = value.as_array()?;
    if array.len() < 4 {
        return None;
    }
    Some([
        array[0].as_f64()? as f32,
        array[1].as_f64()? as f32,
        array[2].as_f64()? as f32,
        array[3].as_f64()? as f32,
    ])
}

/// Reads a 3-element `f32` array from a JSON value (with at least 3 numbers), for the
/// override path's `emissive`.
fn read_array3(value: &Value) -> Option<[f32; 3]> {
    let array = value.as_array()?;
    if array.len() < 3 {
        return None;
    }
    Some([
        array[0].as_f64()? as f32,
        array[1].as_f64()? as f32,
        array[2].as_f64()? as f32,
    ])
}

/// Reads a 2-element `f32` array from a JSON value (with at least 2 numbers), for the
/// override path's `uvTiling` / `uvOffset`.
fn read_array2(value: &Value) -> Option<[f32; 2]> {
    let array = value.as_array()?;
    if array.len() < 2 {
        return None;
    }
    Some([array[0].as_f64()? as f32, array[1].as_f64()? as f32])
}

#[cfg(test)]
mod tests {
    use saffron_core::Uuid;

    use super::*;

    #[test]
    fn apply_overrides_writes_only_named_fields() {
        let mut material = MaterialAsset::default();
        let overrides = serde_json::json!({
            "metallic": 0.8,
            "baseColor": [0.1, 0.2, 0.3, 0.4],
            "albedoTexture": "77",
            "bogus": 5,
            "roughness": "not a number",
        });
        apply_overrides(&mut material, &overrides);
        assert_eq!(material.metallic, 0.8);
        assert_eq!(material.base_color, Vec4::new(0.1, 0.2, 0.3, 0.4));
        assert_eq!(material.albedo_texture, Uuid(77));
        assert_eq!(material.roughness, 1.0);
        assert_eq!(material.emissive, Vec3::ZERO);
        assert_eq!(material.normal_texture, Uuid(0));
    }

    #[test]
    fn apply_overrides_ignores_non_object() {
        let mut material = MaterialAsset::default();
        let before = material.clone();
        apply_overrides(&mut material, &Value::Null);
        apply_overrides(&mut material, &serde_json::json!([1, 2, 3]));
        assert_eq!(material, before);
    }
}
