//! Material resolution for a cook: the pinned `.smat`/imported documents a family's slots
//! reference, and the coverage images its contours and atlas are derived from.

use std::collections::{BTreeMap, BTreeSet};

use saffron_core::Uuid;
use saffron_geometry::{AlphaMode, ChunkKind, ImportedMaterial, decode_image_from_memory};
use saffron_scene::AssetType;
use saffron_vegetation::{
    AlphaClassification, ContentHash, CoverageSource, MaterialSurface, PlantSourceMaterialSnapshot,
    PlantSourceSelector, PlantSourceSnapshot, vegetation_content_hash,
};

use crate::cook_reader::CookAssetAccess;
use crate::material::{
    MaterialAsset, apply_overrides, default_material_asset, material_asset_from_json,
    material_asset_to_json,
};
use crate::{DEFAULT_MATERIAL_ID, Error, Result};

use super::decode::SectionReader;
use super::sections::{
    append_bytes, append_domain, append_json, append_string, append_u32, append_u64,
};
use super::sources::{ResolvedPlantSource, source_snapshot_hash};

pub(super) type ResolvedMaterialDocuments = BTreeMap<u64, Vec<u8>>;
pub(super) type ResolvedCoverageImages = BTreeMap<u64, ResolvedCoverageImage>;
type ResolvedCatalogMaterials = (
    Vec<PlantSourceMaterialSnapshot>,
    ResolvedMaterialDocuments,
    ResolvedCoverageImages,
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ResolvedCoverageImage {
    pub(super) alpha: Vec<u8>,
    /// The decoded RGBA texels the alpha plane was derived from. The atlas composites colour, so
    /// packing needs the edge texel's hue as well as its coverage: a gutter carrying transparent
    /// black filters into a slot's edge as a dark fringe.
    pub(super) rgba: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) cutoff: u8,
    /// Catalog texture the coverage was decoded from, with the hash of the bytes read. The material
    /// document names its coverage texture by id alone, so without this hash in the cook key an
    /// edited cutout returns the previous artifact. `None` for an imported material, whose texture
    /// bytes travel inside the model source and are covered by that source's content hash.
    pub(super) texture: Option<(Uuid, ContentHash)>,
}

pub(super) fn resolve_catalog_material_source(
    assets: &mut dyn CookAssetAccess,
    entry: &saffron_scene::AssetEntry,
) -> Result<ResolvedPlantSource> {
    let (materials, material_documents, coverage_images) = resolve_catalog_materials(
        assets,
        std::iter::once((entry.id, format!("materials/{}", entry.name))),
    )?;
    let mut snapshot = PlantSourceSnapshot {
        source: 0,
        content_hash: [0; 32],
        meshes: Vec::new(),
        materials,
        joints: Vec::new(),
        semantic_elements: Vec::new(),
    };
    snapshot.content_hash = source_snapshot_hash(&snapshot);
    Ok(ResolvedPlantSource {
        origin: saffron_geometry::ImportedOrigin::default(),
        snapshot,
        material_documents,
        coverage_images,
    })
}

pub(super) fn resolve_catalog_materials(
    assets: &mut dyn CookAssetAccess,
    materials: impl IntoIterator<Item = (Uuid, String)>,
) -> Result<ResolvedCatalogMaterials> {
    let mut snapshots = Vec::new();
    let mut documents = BTreeMap::new();
    let mut coverage_images = BTreeMap::new();
    for (id, path) in materials {
        let (material, document) = resolve_catalog_material_strict(assets, id)?;
        let (classification, coverage_source) = material_coverage(&material);
        if let Some(image) = resolve_material_coverage_image(assets, &material)? {
            coverage_images.insert(id.value(), image);
        }
        let content_hash = vegetation_content_hash(&document);
        snapshots.push(PlantSourceMaterialSnapshot {
            selector: PlantSourceSelector::Element {
                id: u128::from(id.value()),
                path,
            },
            material: id,
            content_hash,
            surface: material.surface.clone(),
            alpha_classification: classification,
            coverage_source,
        });
        documents.insert(id.value(), document);
    }
    Ok((snapshots, documents, coverage_images))
}

fn resolve_catalog_material_strict(
    assets: &mut dyn CookAssetAccess,
    id: Uuid,
) -> Result<(MaterialAsset, Vec<u8>)> {
    let mut chain = Vec::new();
    let mut active = BTreeSet::new();
    let mut current = id;
    while current.value() != 0 {
        if !active.insert(current.value()) {
            return Err(Error::Io(
                "material parent hierarchy contains a cycle".to_owned(),
            ));
        }
        if chain.len() >= 1_024 {
            return Err(Error::Io(
                "material parent hierarchy exceeds 1024 entries".to_owned(),
            ));
        }
        let material = load_cook_material_asset_raw(assets, current)?;
        current = material.parent;
        chain.push(material);
    }
    let mut resolved = chain
        .pop()
        .ok_or_else(|| Error::Io("material identity is missing".to_owned()))?;
    while let Some(child) = chain.pop() {
        apply_overrides(&mut resolved, &child.overrides);
        resolved.parent = child.parent;
        resolved.overrides = child.overrides;
    }
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/plant-material-source/v1");
    append_json(&mut bytes, &material_asset_to_json(&resolved));
    let mut textures = material_texture_ids(&resolved);
    textures.sort_unstable_by_key(|texture| texture.value());
    textures.dedup();
    for texture in textures {
        append_u64(&mut bytes, texture.value());
        append_bytes(&mut bytes, &read_catalog_asset_bytes(assets, texture)?);
    }
    Ok((resolved, bytes))
}

fn load_cook_material_asset_raw(
    assets: &mut dyn CookAssetAccess,
    id: Uuid,
) -> Result<MaterialAsset> {
    if id == DEFAULT_MATERIAL_ID {
        return Ok(default_material_asset());
    }
    let entry = assets
        .catalog()
        .find(id)
        .cloned()
        .ok_or(Error::NotInCatalog(id.value()))?;
    if entry.asset_type != AssetType::Material {
        return Err(Error::WrongAssetType {
            id: id.value(),
            wanted: "material",
        });
    }
    let bytes = if entry.container.value() == 0 {
        assets.read_file(&assets.root().join(&entry.path))?
    } else {
        let model = assets.load_model(entry.container).ok_or_else(|| {
            Error::Io(format!(
                "material {}: container {} is not loadable",
                id.value(),
                entry.container.value()
            ))
        })?;
        let source = assets.chunk_source(&model, ChunkKind::Material, id);
        if source.is_empty() {
            return Err(Error::ContainerMissingSubAsset {
                container: entry.container.value(),
                sub: id.value(),
            });
        }
        assets.read_source(&source)?
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| Error::Io(format!("material {} is not UTF-8: {error}", id.value())))?;
    material_asset_from_json(&saffron_json::parse_json(text)?)
}

fn read_catalog_asset_bytes(assets: &mut dyn CookAssetAccess, id: Uuid) -> Result<Vec<u8>> {
    let entry = assets
        .catalog()
        .find(id)
        .cloned()
        .ok_or(Error::NotInCatalog(id.value()))?;
    if entry.asset_type != AssetType::Texture {
        return Err(Error::WrongAssetType {
            id: id.value(),
            wanted: "texture",
        });
    }
    if entry.container.value() == 0 {
        return assets.read_file(&assets.root().join(entry.path));
    }
    let model = assets
        .load_model(entry.container)
        .ok_or_else(|| Error::Io(format!("model {} is not loadable", entry.container.value())))?;
    let source = assets.chunk_source(&model, ChunkKind::Texture, id);
    if source.is_empty() {
        return Err(Error::ContainerMissingSubAsset {
            container: entry.container.value(),
            sub: id.value(),
        });
    }
    assets.read_source(&source)
}

fn resolve_material_coverage_image(
    assets: &mut dyn CookAssetAccess,
    material: &MaterialAsset,
) -> Result<Option<ResolvedCoverageImage>> {
    let (texture, cutoff, multiply_base_alpha) = match &material.surface {
        MaterialSurface::ThinSheetFoliage(parameters) => {
            let texture = match parameters.coverage_source {
                CoverageSource::AlbedoAlpha => material.albedo_texture,
                CoverageSource::Texture(texture) => texture,
                CoverageSource::ModeledGeometry => return Ok(None),
            };
            (
                texture,
                unit_interval_to_u8(parameters.coverage.reference_cutoff),
                matches!(parameters.coverage_source, CoverageSource::AlbedoAlpha),
            )
        }
        MaterialSurface::Standard if material.blend == "masked" => (
            material.albedo_texture,
            normalized_f32_to_u8(material.alpha_cutoff),
            true,
        ),
        MaterialSurface::Standard => return Ok(None),
    };
    if texture.value() == 0 {
        return Ok(None);
    }
    let bytes = read_catalog_asset_bytes(assets, texture)?;
    let content_hash = ContentHash::of(&bytes);
    let decoded = decode_image_from_memory(&bytes)?;
    let base_alpha = if multiply_base_alpha {
        material.base_color.w
    } else {
        1.0
    };
    Ok(Some(resolved_coverage_image(
        decoded.rgba,
        decoded.width,
        decoded.height,
        cutoff,
        base_alpha,
        Some((texture, content_hash)),
    )))
}

pub(super) fn imported_material_coverage_image(
    material: &ImportedMaterial,
) -> Result<Option<ResolvedCoverageImage>> {
    if material.alpha_mode != AlphaMode::Mask {
        return Ok(None);
    }
    let Some(texture) = &material.albedo else {
        return Ok(None);
    };
    let decoded = decode_image_from_memory(&texture.bytes)?;
    Ok(Some(resolved_coverage_image(
        decoded.rgba,
        decoded.width,
        decoded.height,
        normalized_f32_to_u8(material.alpha_cutoff),
        material.base_color.w,
        None,
    )))
}

fn resolved_coverage_image(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    cutoff: u8,
    base_alpha: f32,
    texture: Option<(Uuid, ContentHash)>,
) -> ResolvedCoverageImage {
    let alpha = rgba
        .chunks_exact(4)
        .map(|pixel| normalized_f32_to_u8(f32::from(pixel[3]) / 255.0 * base_alpha))
        .collect();
    ResolvedCoverageImage {
        alpha,
        rgba,
        width,
        height,
        cutoff,
        texture,
    }
}

fn unit_interval_to_u8(value: saffron_spatial::UnitInterval) -> u8 {
    u8::try_from((u32::from(value.bits()) * 255 + 32_767) / 65_535)
        .expect("unit interval maps to u8")
}

fn normalized_f32_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

pub(super) fn imported_material_document(material: &ImportedMaterial) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/imported-plant-material/v1");
    append_string(&mut bytes, &material.name);
    for value in material.base_color.to_array() {
        append_u32(&mut bytes, value.to_bits());
    }
    append_u32(&mut bytes, material.metallic.to_bits());
    append_u32(&mut bytes, material.roughness.to_bits());
    for value in material.emissive.to_array() {
        append_u32(&mut bytes, value.to_bits());
    }
    append_u32(&mut bytes, material.emissive_strength.to_bits());
    append_u32(&mut bytes, material.alpha_cutoff.to_bits());
    bytes.push(match material.alpha_mode {
        AlphaMode::Opaque => 0,
        AlphaMode::Mask => 1,
        AlphaMode::Blend => 2,
    });
    bytes.push(u8::from(material.double_sided));
    for texture in [
        &material.albedo,
        &material.metallic_roughness,
        &material.normal,
        &material.occlusion,
        &material.emissive_tex,
    ] {
        match texture {
            Some(texture) => {
                bytes.push(1);
                append_string(&mut bytes, &texture.ext);
                append_bytes(&mut bytes, &texture.bytes);
            }
            None => bytes.push(0),
        }
    }
    bytes
}

/// Decodes one pinned material document into its resolved [`MaterialAsset`]. The two document forms
/// dispatch on their domain: a catalog material pins its parent-resolved `.smat` JSON, an imported
/// source pins the binary parameter record. The embedded texture payloads are cook inputs and are
/// skipped; textures resolve through the catalog by id.
pub(crate) fn decode_plant_material_document(bytes: &[u8]) -> Result<MaterialAsset> {
    let mut probe = SectionReader::new(bytes);
    if probe.read_bytes()? == b"saffron-anima/plant-material-source/v1" {
        let document = saffron_json::parse_json(&String::from_utf8_lossy(probe.read_bytes()?))
            .map_err(|err| Error::Io(format!("pinned plant material document: {err}")))?;
        return crate::material::material_asset_from_json(&document);
    }
    decode_imported_material_document(bytes)
}

/// Decodes one pinned imported-material document into its resolved parameter-level
/// [`MaterialAsset`] — the decode mirror of [`imported_material_document`]. The embedded
/// texture payloads are cook inputs (coverage/atlas derivation) and are skipped; the
/// runtime material carries the factors, blend axis, and sidedness.
fn decode_imported_material_document(bytes: &[u8]) -> Result<MaterialAsset> {
    let mut reader = SectionReader::new(bytes);
    reader.expect_domain(b"saffron-anima/imported-plant-material/v1")?;
    let _name = reader.read_string()?;
    let read_f32 =
        |reader: &mut SectionReader<'_>| -> Result<f32> { Ok(f32::from_bits(reader.read_u32()?)) };
    let base_color = [
        read_f32(&mut reader)?,
        read_f32(&mut reader)?,
        read_f32(&mut reader)?,
        read_f32(&mut reader)?,
    ];
    let metallic = read_f32(&mut reader)?;
    let roughness = read_f32(&mut reader)?;
    let emissive = [
        read_f32(&mut reader)?,
        read_f32(&mut reader)?,
        read_f32(&mut reader)?,
    ];
    let emissive_strength = read_f32(&mut reader)?;
    let alpha_cutoff = read_f32(&mut reader)?;
    let blend = match reader.read_u8()? {
        0 => "opaque",
        1 => "masked",
        2 => "translucent",
        _ => {
            return Err(Error::Io(
                "imported plant material alpha mode is unknown".to_owned(),
            ));
        }
    };
    let double_sided = reader.read_u8()? != 0;
    for _ in 0..5 {
        if reader.read_u8()? != 0 {
            let _ext = reader.read_string()?;
            let _payload = reader.read_bytes()?;
        }
    }
    Ok(MaterialAsset {
        blend: blend.to_owned(),
        double_sided,
        base_color: saffron_geometry::glam::Vec4::from_array(base_color),
        metallic,
        roughness,
        emissive: saffron_geometry::glam::Vec3::from_array(emissive),
        emissive_strength,
        alpha_cutoff,
        ..MaterialAsset::default()
    })
}

fn material_coverage(material: &MaterialAsset) -> (AlphaClassification, CoverageSource) {
    match &material.surface {
        MaterialSurface::ThinSheetFoliage(parameters) => (
            parameters.coverage.classification,
            parameters.coverage_source,
        ),
        MaterialSurface::Standard => match material.blend.as_str() {
            "masked" => (AlphaClassification::Masked, CoverageSource::AlbedoAlpha),
            "translucent" => (
                AlphaClassification::Transmissive,
                CoverageSource::AlbedoAlpha,
            ),
            _ => (AlphaClassification::Opaque, CoverageSource::ModeledGeometry),
        },
    }
}

pub(super) fn imported_material_coverage(
    material: &ImportedMaterial,
) -> (AlphaClassification, CoverageSource) {
    match material.alpha_mode {
        AlphaMode::Opaque => (AlphaClassification::Opaque, CoverageSource::ModeledGeometry),
        AlphaMode::Mask => (AlphaClassification::Masked, CoverageSource::AlbedoAlpha),
        AlphaMode::Blend => (
            AlphaClassification::Transmissive,
            CoverageSource::AlbedoAlpha,
        ),
    }
}

fn material_texture_ids(material: &MaterialAsset) -> Vec<Uuid> {
    let mut textures = vec![
        material.albedo_texture,
        material.orm_texture,
        material.normal_texture,
        material.emissive_texture,
        material.height_texture,
        material.vector_displacement_texture,
    ];
    if let MaterialSurface::ThinSheetFoliage(parameters) = &material.surface
        && let CoverageSource::Texture(texture) = parameters.coverage_source
    {
        textures.push(texture);
    }
    textures.retain(|texture| texture.value() != 0);
    textures
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        Scratch, coverage_png, imported_family, save_family, triangle,
    };
    use super::super::validate_plant_family_sources;
    use crate::{MaterialAsset, save_material_asset};
    use saffron_core::Uuid;
    use saffron_geometry::save_mesh_to_buffer;
    use saffron_scene::{AssetEntry, AssetType};
    use saffron_vegetation::{
        CookDependency, CookDependencyAddress, MaterialSurface, PlantCompileLimits,
    };

    #[test]
    fn repainting_a_coverage_texture_moves_the_cook_key() {
        // A material document names its coverage texture by id and carries none of its pixels, so
        // repainting the texture leaves the document byte-identical. The pixels therefore have to be
        // in the cook key themselves: the publication guards are staleness checks, and a cache hit
        // never reaches them.
        let scratch = Scratch::new("coverage-repaint");
        let mut assets = scratch.assets();
        let texture = Uuid(7_402);
        let relative = "textures/leaf.png";
        std::fs::create_dir_all(assets.root.join("textures")).expect("create textures");
        std::fs::write(assets.root.join(relative), coverage_png(255)).expect("write texture");
        assets.catalog.put(AssetEntry {
            id: texture,
            name: "leaf".to_owned(),
            asset_type: AssetType::Texture,
            path: relative.to_owned(),
            ..AssetEntry::default()
        });
        let material_asset = MaterialAsset {
            surface: MaterialSurface::Standard,
            blend: "masked".to_owned(),
            albedo_texture: texture,
            ..MaterialAsset::default()
        };
        let material = save_material_asset(&mut assets, &material_asset, "Leaf", "plants")
            .expect("save material");
        let mesh = Uuid(7_403);
        std::fs::create_dir_all(assets.root.join("meshes")).expect("create meshes");
        std::fs::write(
            assets.root.join("meshes/leaf.smesh"),
            save_mesh_to_buffer(&triangle(), &[], None).expect("encode mesh"),
        )
        .expect("write mesh");
        assets.catalog.put(AssetEntry {
            id: mesh,
            name: "leaf".to_owned(),
            asset_type: AssetType::Mesh,
            path: "meshes/leaf.smesh".to_owned(),
            ..AssetEntry::default()
        });
        let family = save_family(&mut assets, imported_family(material, mesh));

        let before =
            validate_plant_family_sources(&mut assets, &family, PlantCompileLimits::default())
                .expect("validate before")
                .dependencies;
        // The texture must actually be an input, or the comparison below is between two cook keys
        // that never mentioned it and would agree for the wrong reason.
        let dependency_of = |dependencies: &[CookDependency]| {
            dependencies
                .iter()
                .find(|dependency| {
                    dependency.address == CookDependencyAddress::SourceAsset { asset: texture }
                })
                .map(|dependency| dependency.content_hash)
        };
        let hash_before =
            dependency_of(&before).expect("the coverage texture is a cook dependency");

        std::fs::write(assets.root.join(relative), coverage_png(64)).expect("repaint texture");
        assets.clear_asset_caches();
        let after =
            validate_plant_family_sources(&mut assets, &family, PlantCompileLimits::default())
                .expect("validate after")
                .dependencies;
        let hash_after = dependency_of(&after).expect("the coverage texture is still a dependency");

        assert_ne!(
            hash_before, hash_after,
            "repainting the coverage texture must move its dependency hash"
        );
        assert_ne!(before, after, "the cook key must move with it");
    }
}
