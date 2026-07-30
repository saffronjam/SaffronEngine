use saffron_core::{HeightMode, Uuid};
use saffron_geometry::glam::{Vec2, Vec3, Vec4};
use saffron_json::{Value, json_bool_or, json_f32_or, json_string_or};
use saffron_spatial::{DecisionScalar, UnitInterval};
use saffron_vegetation::{
    AlphaClassification, CoverageMipMetadata, CoverageSource, MaterialSurface,
    OpacityMicromapDerivation, ThinSheetFoliageParameters, ThinSheetNormalBehavior,
    VoxelMaterialMoments,
};

use crate::error::{Error, Result};

use super::{MaterialAsset, empty_object};

/// A uuid emitted as a decimal JSON *string*.
fn uuid_string(id: Uuid) -> Value {
    Value::String(id.value().to_string())
}

/// Reads a [`Uuid`] from a JSON value, accepting a decimal string *or* an unsigned number
/// (the frozen lenient read). Anything else is `0`.
pub(super) fn uuid_from_value(value: &Value) -> Uuid {
    match value {
        Value::String(s) => Uuid(s.parse::<u64>().unwrap_or(0)),
        Value::Number(n) => Uuid(n.as_u64().unwrap_or(0)),
        _ => Uuid(0),
    }
}

/// Reads a fixed-length `f32` array field, returning `None` unless it is an array of
/// exactly `N` numbers.
fn read_fixed_array<const N: usize>(object: &Value, key: &str) -> Option<[f32; N]> {
    let array = object.as_object()?.get(key)?.as_array()?;
    if array.len() != N {
        return None;
    }
    let mut out = [0.0f32; N];
    for (slot, element) in out.iter_mut().zip(array) {
        *slot = element.as_f64()? as f32;
    }
    Some(out)
}

fn hex_hash(hash: &[u8; 32]) -> String {
    let mut value = String::with_capacity(64);
    for byte in hash {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}").unwrap();
    }
    value
}

fn parse_hash(value: &Value) -> Result<[u8; 32]> {
    let text = value
        .as_str()
        .ok_or_else(|| Error::Io(".smat coverage hash is not a string".to_owned()))?;
    if text.len() != 64
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::Io(
            ".smat coverage hash is not canonical lowercase SHA-256".to_owned(),
        ));
    }
    let mut hash = [0_u8; 32];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|error| Error::Io(error.to_string()))?;
    }
    Ok(hash)
}

fn thin_sheet_to_json(parameters: &ThinSheetFoliageParameters) -> Value {
    let (coverage_source, coverage_texture) = match parameters.coverage_source {
        CoverageSource::AlbedoAlpha => ("albedo-alpha", Uuid(0)),
        CoverageSource::Texture(texture) => ("texture", texture),
        CoverageSource::ModeledGeometry => ("modeled-geometry", Uuid(0)),
    };
    let alpha_classification = match parameters.coverage.classification {
        AlphaClassification::Opaque => "opaque",
        AlphaClassification::Masked => "masked",
        AlphaClassification::Transmissive => "transmissive",
    };
    serde_json::json!({
        "frontAlbedoResponse": parameters.front_albedo_response.bits(),
        "backAlbedoResponse": parameters.back_albedo_response.bits(),
        "thicknessBits": parameters.thickness.bits(),
        "absorptionColorBits": parameters.absorption_color.map(DecisionScalar::bits),
        "transmissionColorBits": parameters.transmission_color.map(DecisionScalar::bits),
        "roughness": parameters.roughness.bits(),
        "normalBehavior": parameters.normal_behavior.as_wire(),
        "coverageSource": {
            "kind": coverage_source,
            "texture": uuid_string(coverage_texture),
        },
        "coverage": {
            "referenceCutoff": parameters.coverage.reference_cutoff.bits(),
            "sourceExtent": parameters.coverage.source_extent,
            "spatialHashSalt": parameters.coverage.spatial_hash_salt.to_string(),
            "classification": alpha_classification,
            "mipHashes": parameters.coverage.mip_hashes.iter().map(hex_hash).collect::<Vec<_>>(),
        },
        "voxelMoments": {
            "occupancy": parameters.voxel_moments.occupancy.bits(),
            "albedoMeanBits": parameters.voxel_moments.albedo_mean.map(DecisionScalar::bits),
            "roughnessMean": parameters.voxel_moments.roughness_mean.bits(),
            "transmissionMeanBits": parameters.voxel_moments.transmission_mean.map(DecisionScalar::bits),
            "thicknessMeanBits": parameters.voxel_moments.thickness_mean.bits(),
            "normalSecondMomentsBits": parameters.voxel_moments.normal_second_moments.map(DecisionScalar::bits),
        },
        "opacityMicromap": {
            "enabled": parameters.opacity_micromap.enabled,
            "maxSubdivision": parameters.opacity_micromap.max_subdivision,
            "transparentThreshold": parameters.opacity_micromap.transparent_threshold.bits(),
            "opaqueThreshold": parameters.opacity_micromap.opaque_threshold.bits(),
        },
        "energyLimit": parameters.energy_limit.bits(),
    })
}

fn thin_sheet_from_json(value: &Value) -> Result<ThinSheetFoliageParameters> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::Io(".smat thinSheetFoliage is not an object".to_owned()))?;
    let u16_field = |object: &serde_json::Map<String, Value>, key: &str| -> Result<u16> {
        object
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| Error::Io(format!(".smat thinSheetFoliage.{key} is invalid")))
    };
    let u8_field = |object: &serde_json::Map<String, Value>, key: &str| -> Result<u8> {
        object
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(|| Error::Io(format!(".smat thinSheetFoliage.{key} is invalid")))
    };
    let i32_field = |object: &serde_json::Map<String, Value>, key: &str| -> Result<i32> {
        object
            .get(key)
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| Error::Io(format!(".smat thinSheetFoliage.{key} is invalid")))
    };
    let fixed_array = |object: &serde_json::Map<String, Value>, key: &str, count: usize| {
        let values = object
            .get(key)
            .and_then(Value::as_array)
            .filter(|values| values.len() == count)
            .ok_or_else(|| Error::Io(format!(".smat thinSheetFoliage.{key} is invalid")))?;
        values
            .iter()
            .map(|value| {
                value
                    .as_i64()
                    .and_then(|value| i32::try_from(value).ok())
                    .map(DecisionScalar::from_bits)
                    .ok_or_else(|| Error::Io(format!(".smat thinSheetFoliage.{key} is invalid")))
            })
            .collect::<Result<Vec<_>>>()
    };

    let coverage_source = object
        .get("coverageSource")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Io(".smat thinSheetFoliage.coverageSource is invalid".to_owned()))?;
    let coverage_source = match coverage_source.get("kind").and_then(Value::as_str) {
        Some("albedo-alpha") => CoverageSource::AlbedoAlpha,
        Some("modeled-geometry") => CoverageSource::ModeledGeometry,
        Some("texture") => CoverageSource::Texture(
            coverage_source
                .get("texture")
                .map(uuid_from_value)
                .ok_or_else(|| {
                    Error::Io(".smat thinSheetFoliage coverage texture is missing".to_owned())
                })?,
        ),
        _ => {
            return Err(Error::Io(
                ".smat thinSheetFoliage coverage source is invalid".to_owned(),
            ));
        }
    };
    let coverage = object
        .get("coverage")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Io(".smat thinSheetFoliage.coverage is invalid".to_owned()))?;
    let source_extent = coverage
        .get("sourceExtent")
        .and_then(Value::as_array)
        .filter(|values| values.len() == 2)
        .and_then(|values| {
            Some([
                u32::try_from(values[0].as_u64()?).ok()?,
                u32::try_from(values[1].as_u64()?).ok()?,
            ])
        })
        .ok_or_else(|| Error::Io(".smat coverage sourceExtent is invalid".to_owned()))?;
    let spatial_hash_salt = coverage
        .get("spatialHashSalt")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| Error::Io(".smat coverage spatialHashSalt is invalid".to_owned()))?;
    let classification = match coverage.get("classification").and_then(Value::as_str) {
        Some("opaque") => AlphaClassification::Opaque,
        Some("masked") => AlphaClassification::Masked,
        Some("transmissive") => AlphaClassification::Transmissive,
        _ => {
            return Err(Error::Io(
                ".smat coverage classification is invalid".to_owned(),
            ));
        }
    };
    let mip_hashes = coverage
        .get("mipHashes")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Io(".smat coverage mipHashes is invalid".to_owned()))?
        .iter()
        .map(parse_hash)
        .collect::<Result<Vec<_>>>()?;
    let voxel = object
        .get("voxelMoments")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Io(".smat thinSheetFoliage.voxelMoments is invalid".to_owned()))?;
    let albedo_mean: [DecisionScalar; 3] = fixed_array(voxel, "albedoMeanBits", 3)?
        .try_into()
        .map_err(|_| Error::Io(".smat voxel albedo mean is invalid".to_owned()))?;
    let transmission_mean: [DecisionScalar; 3] = fixed_array(voxel, "transmissionMeanBits", 3)?
        .try_into()
        .map_err(|_| Error::Io(".smat voxel transmission mean is invalid".to_owned()))?;
    let normal_second_moments: [DecisionScalar; 6] =
        fixed_array(voxel, "normalSecondMomentsBits", 6)?
            .try_into()
            .map_err(|_| Error::Io(".smat voxel normal moments are invalid".to_owned()))?;
    let omm = object
        .get("opacityMicromap")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Io(".smat thinSheetFoliage.opacityMicromap is invalid".to_owned()))?;
    let absorption_color: [DecisionScalar; 3] = fixed_array(object, "absorptionColorBits", 3)?
        .try_into()
        .map_err(|_| Error::Io(".smat absorption color is invalid".to_owned()))?;
    let transmission_color: [DecisionScalar; 3] = fixed_array(object, "transmissionColorBits", 3)?
        .try_into()
        .map_err(|_| Error::Io(".smat transmission color is invalid".to_owned()))?;
    let parameters = ThinSheetFoliageParameters {
        front_albedo_response: UnitInterval::from_bits(u16_field(object, "frontAlbedoResponse")?),
        back_albedo_response: UnitInterval::from_bits(u16_field(object, "backAlbedoResponse")?),
        thickness: DecisionScalar::from_bits(i32_field(object, "thicknessBits")?),
        absorption_color,
        transmission_color,
        roughness: UnitInterval::from_bits(u16_field(object, "roughness")?),
        normal_behavior: object
            .get("normalBehavior")
            .and_then(Value::as_str)
            .and_then(ThinSheetNormalBehavior::from_wire)
            .ok_or_else(|| Error::Io(".smat normalBehavior is invalid".to_owned()))?,
        coverage_source,
        coverage: CoverageMipMetadata {
            reference_cutoff: UnitInterval::from_bits(u16_field(coverage, "referenceCutoff")?),
            source_extent,
            spatial_hash_salt,
            classification,
            mip_hashes,
        },
        voxel_moments: VoxelMaterialMoments {
            occupancy: UnitInterval::from_bits(u16_field(voxel, "occupancy")?),
            albedo_mean,
            roughness_mean: UnitInterval::from_bits(u16_field(voxel, "roughnessMean")?),
            transmission_mean,
            thickness_mean: DecisionScalar::from_bits(i32_field(voxel, "thicknessMeanBits")?),
            normal_second_moments,
        },
        opacity_micromap: OpacityMicromapDerivation {
            enabled: omm
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| Error::Io(".smat OMM enabled is invalid".to_owned()))?,
            max_subdivision: u8_field(omm, "maxSubdivision")?,
            transparent_threshold: UnitInterval::from_bits(u16_field(omm, "transparentThreshold")?),
            opaque_threshold: UnitInterval::from_bits(u16_field(omm, "opaqueThreshold")?),
        },
        energy_limit: UnitInterval::from_bits(u16_field(object, "energyLimit")?),
    };
    parameters.validate()?;
    Ok(parameters)
}

/// Serializes a [`MaterialAsset`] to the frozen `.smat` JSON document.
///
/// Uuid fields are emitted as decimal strings, never numbers; `version` is pinned to `2`;
/// `surfaceModel` and `thinSheetFoliage` form one strict tagged union. `graph` /
/// `overrides` ride through verbatim (an empty object when unset). Object key order is
/// incidental: the `.smat` write path serializes via `dump_json_sorted`
/// (alphabetically sorted keys), so the byte shape is stable regardless of insertion order.
#[must_use]
pub fn material_asset_to_json(material: &MaterialAsset) -> Value {
    let graph = if material.graph.is_null() {
        empty_object()
    } else {
        material.graph.clone()
    };
    let overrides = if material.overrides.is_null() {
        empty_object()
    } else {
        material.overrides.clone()
    };
    let thin_sheet_foliage = match &material.surface {
        MaterialSurface::Standard => Value::Null,
        MaterialSurface::ThinSheetFoliage(parameters) => thin_sheet_to_json(parameters),
    };
    serde_json::json!({
        "version": 2,
        "surfaceModel": material.surface.model().as_wire(),
        "thinSheetFoliage": thin_sheet_foliage,
        "shader": material.shader,
        "blend": material.blend,
        "unlit": material.unlit,
        "doubleSided": material.double_sided,
        "heightMode": material.height_mode.as_wire(),
        "normalConvention": material.normal_convention,
        "factors": {
            "baseColor": [
                material.base_color.x,
                material.base_color.y,
                material.base_color.z,
                material.base_color.w,
            ],
            "metallic": material.metallic,
            "roughness": material.roughness,
            "emissive": [material.emissive.x, material.emissive.y, material.emissive.z],
            "emissiveStrength": material.emissive_strength,
            "normalStrength": material.normal_strength,
            "alphaCutoff": material.alpha_cutoff,
            "heightScale": material.height_scale,
            "uvTiling": [material.uv_tiling.x, material.uv_tiling.y],
            "uvOffset": [material.uv_offset.x, material.uv_offset.y],
        },
        "textures": {
            "albedo": uuid_string(material.albedo_texture),
            "ormOrMr": uuid_string(material.orm_texture),
            "normal": uuid_string(material.normal_texture),
            "emissive": uuid_string(material.emissive_texture),
            "height": uuid_string(material.height_texture),
            "vectorDisplacement": uuid_string(material.vector_displacement_texture),
        },
        "graph": graph,
        "parent": uuid_string(material.parent),
        "overrides": overrides,
    })
}

/// Encodes the canonical `.smat` text with sorted keys and one trailing newline.
#[must_use]
pub fn material_asset_to_text(material: &MaterialAsset, indent: i32) -> String {
    let mut text = saffron_json::dump_json_sorted(&material_asset_to_json(material), indent);
    text.push('\n');
    text
}

/// Rebuilds a [`MaterialAsset`] from a `.smat` JSON document.
///
/// The version and surface union are strict. Conventional material properties retain
/// their defaults when absent; uuid fields accept a decimal string *or* a number.
/// `features` is not read (it is a resolved bitset, never serialized).
pub fn material_asset_from_json(doc: &Value) -> Result<MaterialAsset> {
    let version = doc.get("version").and_then(Value::as_i64).unwrap_or(-1);
    if version != 2 {
        return Err(Error::BadAssetVersion {
            format: ".smat",
            found: version,
            expected: 2,
        });
    }
    let surface = match doc.get("surfaceModel").and_then(Value::as_str) {
        Some("standard") if doc.get("thinSheetFoliage").is_none_or(Value::is_null) => {
            MaterialSurface::Standard
        }
        Some("thin-sheet-foliage") => MaterialSurface::ThinSheetFoliage(thin_sheet_from_json(
            doc.get("thinSheetFoliage").ok_or_else(|| {
                Error::Io(".smat thinSheetFoliage parameters are missing".to_owned())
            })?,
        )?),
        _ => {
            return Err(Error::Io(
                ".smat surfaceModel and thinSheetFoliage do not form one valid surface".to_owned(),
            ));
        }
    };
    let mut material = MaterialAsset {
        surface,
        shader: json_string_or(doc, "shader", "mesh".to_owned()),
        blend: json_string_or(doc, "blend", "opaque".to_owned()),
        unlit: json_bool_or(doc, "unlit", false),
        double_sided: json_bool_or(doc, "doubleSided", false),
        height_mode: HeightMode::from_wire(&json_string_or(doc, "heightMode", "bump".to_owned())),
        normal_convention: json_string_or(doc, "normalConvention", "gl".to_owned()),
        ..MaterialAsset::default()
    };

    if let Some(factors) = doc.get("factors").filter(|v| v.is_object()) {
        if let Some([r, g, b, a]) = read_fixed_array::<4>(factors, "baseColor") {
            material.base_color = Vec4::new(r, g, b, a);
        }
        material.metallic = json_f32_or(factors, "metallic", 0.0);
        material.roughness = json_f32_or(factors, "roughness", 1.0);
        if let Some([r, g, b]) = read_fixed_array::<3>(factors, "emissive") {
            material.emissive = Vec3::new(r, g, b);
        }
        material.emissive_strength = json_f32_or(factors, "emissiveStrength", 1.0);
        material.normal_strength = json_f32_or(factors, "normalStrength", 1.0);
        material.alpha_cutoff = json_f32_or(factors, "alphaCutoff", 0.5);
        material.height_scale = json_f32_or(factors, "heightScale", 0.05);
        if let Some([x, y]) = read_fixed_array::<2>(factors, "uvTiling") {
            material.uv_tiling = Vec2::new(x, y);
        }
        if let Some([x, y]) = read_fixed_array::<2>(factors, "uvOffset") {
            material.uv_offset = Vec2::new(x, y);
        }
    }

    if let Some(textures) = doc.get("textures").filter(|v| v.is_object()) {
        if let Some(v) = textures.get("albedo") {
            material.albedo_texture = uuid_from_value(v);
        }
        if let Some(v) = textures.get("ormOrMr") {
            material.orm_texture = uuid_from_value(v);
        }
        if let Some(v) = textures.get("normal") {
            material.normal_texture = uuid_from_value(v);
        }
        if let Some(v) = textures.get("emissive") {
            material.emissive_texture = uuid_from_value(v);
        }
        if let Some(v) = textures.get("height") {
            material.height_texture = uuid_from_value(v);
        }
        if let Some(v) = textures.get("vectorDisplacement") {
            material.vector_displacement_texture = uuid_from_value(v);
        }
    }

    if let Some(graph) = doc.get("graph").filter(|v| is_non_empty_object(v)) {
        material.graph = graph.clone();
    }
    if let Some(parent) = doc.get("parent") {
        material.parent = uuid_from_value(parent);
    }
    if let Some(overrides) = doc.get("overrides").filter(|v| is_non_empty_object(v)) {
        material.overrides = overrides.clone();
    }

    material.surface.validate()?;
    Ok(material)
}

/// Whether `value` is a JSON object with at least one member.
fn is_non_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(|map| !map.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_support::populated_material;

    #[test]
    fn round_trip_reproduces_every_field() {
        let original = populated_material();
        let restored = material_asset_from_json(&material_asset_to_json(&original)).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn thin_sheet_foliage_round_trip_reproduces_the_complete_union() {
        let parameters = ThinSheetFoliageParameters {
            front_albedo_response: UnitInterval::from_bits(20_000),
            back_albedo_response: UnitInterval::from_bits(19_000),
            thickness: DecisionScalar::from_bits(1_310),
            absorption_color: [
                DecisionScalar::from_bits(4_000),
                DecisionScalar::from_bits(5_000),
                DecisionScalar::from_bits(6_000),
            ],
            transmission_color: [
                DecisionScalar::from_bits(25_000),
                DecisionScalar::from_bits(24_000),
                DecisionScalar::from_bits(23_000),
            ],
            roughness: UnitInterval::from_bits(41_000),
            normal_behavior: ThinSheetNormalBehavior::Symmetric,
            coverage_source: CoverageSource::Texture(Uuid(77)),
            coverage: CoverageMipMetadata {
                reference_cutoff: UnitInterval::from_bits(31_000),
                source_extent: [4, 2],
                spatial_hash_salt: 99,
                classification: AlphaClassification::Transmissive,
                mip_hashes: vec![[1; 32], [2; 32], [3; 32]],
            },
            voxel_moments: VoxelMaterialMoments {
                occupancy: UnitInterval::from_bits(30_000),
                albedo_mean: [DecisionScalar::from_bits(10_000); 3],
                roughness_mean: UnitInterval::from_bits(40_000),
                transmission_mean: [DecisionScalar::from_bits(12_000); 3],
                thickness_mean: DecisionScalar::from_bits(700),
                normal_second_moments: [DecisionScalar::from_bits(8_000); 6],
            },
            opacity_micromap: OpacityMicromapDerivation {
                enabled: true,
                max_subdivision: 5,
                transparent_threshold: UnitInterval::from_bits(5_000),
                opaque_threshold: UnitInterval::from_bits(60_000),
            },
            energy_limit: UnitInterval::from_bits(60_000),
        };
        let original = MaterialAsset {
            surface: MaterialSurface::ThinSheetFoliage(parameters),
            ..populated_material()
        };

        let restored = material_asset_from_json(&material_asset_to_json(&original)).unwrap();

        assert_eq!(restored, original);
    }

    #[test]
    fn thin_sheet_foliage_rejects_unknown_normal_behavior() {
        let material = MaterialAsset {
            surface: MaterialSurface::ThinSheetFoliage(ThinSheetFoliageParameters::default()),
            ..MaterialAsset::default()
        };
        let mut doc = material_asset_to_json(&material);
        doc["thinSheetFoliage"]["normalBehavior"] = Value::String("unknown".to_owned());

        assert!(material_asset_from_json(&doc).is_err());
    }

    #[test]
    fn surface_union_rejects_parameters_for_standard_materials() {
        let mut doc = material_asset_to_json(&MaterialAsset::default());
        doc["thinSheetFoliage"] = thin_sheet_to_json(&ThinSheetFoliageParameters::default());

        assert!(material_asset_from_json(&doc).is_err());
    }

    #[test]
    fn round_trip_preserves_opaque_graph_and_overrides() {
        let mut original = populated_material();
        original.parent = Uuid(42);
        original.graph = serde_json::json!({
            "nodes": [{ "id": "n1", "type": "constant", "props": { "value": [1, 0, 0, 1] } }],
            "edges": [],
        });
        original.overrides = serde_json::json!({ "metallic": 0.9, "albedoTexture": "7" });
        let restored = material_asset_from_json(&material_asset_to_json(&original)).unwrap();
        assert_eq!(restored.graph, original.graph);
        assert_eq!(restored.overrides, original.overrides);
        assert_eq!(restored.parent, Uuid(42));
    }

    #[test]
    fn uuid_fields_serialize_as_strings_never_numbers() {
        let material = populated_material();
        let doc = material_asset_to_json(&material);
        let textures = doc.get("textures").unwrap();
        for key in ["albedo", "ormOrMr", "normal", "emissive", "height"] {
            assert!(
                textures.get(key).unwrap().is_string(),
                "texture {key} must serialize as a string"
            );
        }
        assert!(doc.get("parent").unwrap().is_string());
        // The serialized bytes must carry quotes around every id.
        let serialized = saffron_json::dump_json(&doc, -1);
        assert!(serialized.contains(r#""albedo":"1001""#));
        assert!(serialized.contains(r#""parent":"0""#));
    }

    #[test]
    fn byte_equal_to_captured_smat_fixture() {
        // A `.smat` document: alphabetically-sorted keys (via `dump_json_sorted`), uuid
        // fields as decimal strings, `version: 2`. The default material with two texture
        // ids assigned.
        let material = MaterialAsset {
            albedo_texture: Uuid(5),
            normal_texture: Uuid(6),
            ..MaterialAsset::default()
        };
        let serialized = material_asset_to_text(&material, -1);
        // `heightScale` (f32 `0.05`) carries its f64-promoted long decimal (an
        // exactly-representable value like `0.5` stays short).
        let expected = concat!(
            r#"{"blend":"opaque","doubleSided":false,"#,
            r#""factors":{"alphaCutoff":0.5,"baseColor":[1.0,1.0,1.0,1.0],"#,
            r#""emissive":[0.0,0.0,0.0],"emissiveStrength":1.0,"#,
            r#""heightScale":0.05000000074505806,"#,
            r#""metallic":0.0,"normalStrength":1.0,"roughness":1.0,"#,
            r#""uvOffset":[0.0,0.0],"uvTiling":[1.0,1.0]},"#,
            r#""graph":{},"heightMode":"bump","normalConvention":"gl","overrides":{},"parent":"0","#,
            r#""shader":"mesh","surfaceModel":"standard","#,
            r#""textures":{"albedo":"5","emissive":"0","height":"0","normal":"6","ormOrMr":"0","vectorDisplacement":"0"},"#,
            r#""thinSheetFoliage":null,"#,
            "\"unlit\":false,\"version\":2}\n",
        );
        assert_eq!(serialized, expected);
    }

    #[test]
    fn from_json_accepts_uuid_string_or_number() {
        let doc = serde_json::json!({
            "version": 2,
            "surfaceModel": "standard",
            "thinSheetFoliage": null,
            "textures": { "albedo": "1234", "ormOrMr": 5678 },
            "parent": 99,
        });
        let material = material_asset_from_json(&doc).unwrap();
        assert_eq!(material.albedo_texture, Uuid(1234));
        assert_eq!(material.orm_texture, Uuid(5678));
        assert_eq!(material.parent, Uuid(99));
    }

    #[test]
    fn from_json_rejects_missing_version_and_surface_model() {
        assert!(material_asset_from_json(&serde_json::json!({})).is_err());
    }
}
