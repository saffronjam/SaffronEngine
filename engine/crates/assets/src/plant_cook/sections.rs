//! The canonical big-endian writers for every `.splantc` section.

use std::collections::BTreeMap;

use saffron_geometry::VertexSkin;
use saffron_vegetation::{
    NormalizedPlantFamily, NormalizedPlantMesh, PlantCompileDiagnosticCode,
    PlantCompileDiagnosticSeverity, PlantCompileOutput, PlantFamilyAsset, PlantFamilySource,
    PlantImportSettings, PlantPartSemantic, PlantPivot, PlantReimportConflictReason,
    PlantSemanticDestination, PlantSourceLocator, PlantSourceRole, PlantSourceSelector,
    PlantTangentPolicy, PlantTextureContainer, SourceAxis, SourceHandedness, SourceUnits,
    SourceUvOrigin, SourceWinding, native_botanical_graph_content_hash, native_plant_source_id,
    write_plant_texture_container,
};

use crate::Result;

use super::materials::ResolvedMaterialDocuments;

pub(super) fn source_normalization_section(
    asset: &PlantFamilyAsset,
    compile: &PlantCompileOutput,
    family: &NormalizedPlantFamily,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/source-normalization/v1");
    append_u64(&mut bytes, asset.id.value());
    bytes.extend_from_slice(&compile.family_hash.unwrap_or_default());
    append_u64(&mut bytes, family.sources.len() as u64);
    let hashes = family.sources.iter().copied().collect::<BTreeMap<_, _>>();
    match &asset.source {
        PlantFamilySource::Imported(recipe) => {
            bytes.push(0);
            for source in &recipe.sources {
                bytes.extend_from_slice(&source.id.to_be_bytes());
                bytes.push(source_role_tag(source.role));
                append_selector(&mut bytes, &source.selector);
                bytes.extend_from_slice(
                    hashes
                        .get(&source.id)
                        .copied()
                        .unwrap_or([0; 32])
                        .as_slice(),
                );
                append_import_settings(&mut bytes, &source.settings);
            }
        }
        PlantFamilySource::Native { graph, .. } => {
            bytes.push(1);
            let source = native_plant_source_id(asset.id);
            bytes.extend_from_slice(&source.to_be_bytes());
            bytes.extend_from_slice(&graph.identity().bytes());
            bytes.extend_from_slice(hashes.get(&source).copied().unwrap_or([0; 32]).as_slice());
        }
    }
    bytes
}

pub(super) fn part_table_section(asset: &PlantFamilyAsset) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/part-table/v2");
    append_u64(&mut bytes, asset.tags.len() as u64);
    for tag in &asset.tags {
        append_u64(&mut bytes, tag.value());
    }
    append_u64(&mut bytes, asset.parts.len() as u64);
    for part in &asset.parts {
        bytes.extend_from_slice(&part.id.to_be_bytes());
        append_optional_u128(&mut bytes, part.parent);
        bytes.push(part_semantic_tag(part.semantic));
        append_u32(&mut bytes, part.material_slot);
        append_u64(&mut bytes, part.sources.len() as u64);
        for source in &part.sources {
            bytes.extend_from_slice(&source.to_be_bytes());
        }
    }
    append_dimensions(&mut bytes, asset.dimensions);
    for value in [
        asset.mechanics.stiffness,
        asset.mechanics.drag,
        asset.mechanics.flutter,
        asset.mechanics.damage_threshold,
        asset.mechanics.break_threshold,
    ] {
        bytes.extend_from_slice(&value.bits().to_be_bytes());
    }
    bytes.extend_from_slice(&asset.mechanics.damping.bits().to_be_bytes());
    bytes.extend_from_slice(&asset.mechanics.bend_limit.bits().to_be_bytes());
    append_u64(&mut bytes, asset.variations.len() as u64);
    for variation in &asset.variations {
        append_u32(&mut bytes, variation.id);
        append_string(&mut bytes, &variation.name);
        append_u128_list(&mut bytes, &variation.sources);
        append_u128_list(&mut bytes, &variation.active_parts);
    }
    append_u32(&mut bytes, asset.interaction_policy as u32);
    match &asset.habitat {
        Some(habitat) => {
            bytes.push(1);
            append_u64(&mut bytes, habitat.fields.len() as u64);
            for (channel, minimum, maximum) in &habitat.fields {
                append_field_channel(&mut bytes, *channel);
                bytes.extend_from_slice(&minimum.bits().to_be_bytes());
                bytes.extend_from_slice(&maximum.bits().to_be_bytes());
            }
            append_u64(&mut bytes, habitat.surface_tags.len() as u64);
            for tag in &habitat.surface_tags {
                append_u64(&mut bytes, *tag);
            }
            bytes.extend_from_slice(&habitat.shade_tolerance.bits().to_be_bytes());
        }
        None => bytes.push(0),
    }
    bytes
}

pub(super) fn mesh_section(
    family: &NormalizedPlantFamily,
    role: PlantSourceRole,
    include_materials: bool,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/mesh-facet/v1");
    let meshes = family
        .meshes
        .iter()
        .filter(|mesh| mesh.role == role)
        .collect::<Vec<_>>();
    append_u64(&mut bytes, meshes.len() as u64);
    for mesh in meshes {
        append_normalized_mesh(&mut bytes, mesh, include_materials);
    }
    bytes
}

pub(super) fn material_section(
    family: &NormalizedPlantFamily,
    documents: &ResolvedMaterialDocuments,
    atlas: Option<&crate::FamilyAtlas>,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/materials-coverage/v3");
    append_u64(&mut bytes, family.materials.len() as u64);
    for material in &family.materials {
        append_u64(&mut bytes, material.material.value());
        bytes.extend_from_slice(&material.content_hash);
        append_bytes(
            &mut bytes,
            documents
                .get(&material.material.value())
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
    }
    // Where each slot landed in the packed family atlas, whose extent the geometry's UVs already
    // address. Its texels are the texture-container section; a family with no coverage images, or
    // one whose slots do not fit the budget, writes the absent marker here and an empty container,
    // and keeps slot-local UVs — the halves never half-apply.
    let Some(atlas) = atlas else {
        append_u32(&mut bytes, 0);
        return bytes;
    };
    append_u32(&mut bytes, 1);
    append_u32(&mut bytes, atlas.layout.width);
    append_u32(&mut bytes, atlas.layout.height);
    append_u32(&mut bytes, atlas.layout.gutter);
    append_u64(&mut bytes, atlas.layout.placements.len() as u64);
    for placement in &atlas.layout.placements {
        append_u32(&mut bytes, placement.slot);
        append_u32(&mut bytes, placement.x);
        append_u32(&mut bytes, placement.y);
        append_u32(&mut bytes, placement.width);
        append_u32(&mut bytes, placement.height);
    }
    bytes
}

/// The packed atlas's complete mip chain as a KTX2 container, or empty bytes when the family packs
/// no atlas.
///
/// A standard container rather than a bespoke level list: the payload is self-describing — format,
/// extent, and level index — so what the loader uploads is what the bytes declare, and the section
/// can be lifted out and opened by any texture tool.
pub(super) fn texture_container_section(atlas: Option<&crate::FamilyAtlas>) -> Result<Vec<u8>> {
    let Some(atlas) = atlas else {
        return Ok(Vec::new());
    };
    Ok(write_plant_texture_container(&PlantTextureContainer {
        format: atlas.format,
        width: atlas.layout.width,
        height: atlas.layout.height,
        levels: atlas
            .levels
            .iter()
            .map(|level| level.rgba.as_slice())
            .collect(),
    })?)
}

pub(super) fn skeleton_section(
    asset: &PlantFamilyAsset,
    family: &NormalizedPlantFamily,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/skeleton-weights/v1");
    append_u64(&mut bytes, family.joints.len() as u64);
    for joint in &family.joints {
        bytes.extend_from_slice(&joint.source.to_be_bytes());
        append_selector(&mut bytes, &joint.selector);
        append_optional_selector(&mut bytes, joint.parent.as_ref());
        for value in joint.transform_bits {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
    }
    let skinned = family
        .meshes
        .iter()
        .filter(|mesh| !mesh.skin.is_empty())
        .collect::<Vec<_>>();
    append_u64(&mut bytes, skinned.len() as u64);
    for mesh in skinned {
        bytes.extend_from_slice(&mesh.source.to_be_bytes());
        append_selector(&mut bytes, &mesh.selector);
        append_u64(&mut bytes, mesh.skin.len() as u64);
        for skin in &mesh.skin {
            for joint in skin.joints {
                bytes.extend_from_slice(&joint.to_be_bytes());
            }
            for weight in skin.weights {
                bytes.extend_from_slice(&weight.to_be_bytes());
            }
        }
    }
    append_u64(&mut bytes, asset.spines.len() as u64);
    for spine in &asset.spines {
        bytes.extend_from_slice(&spine.id.to_be_bytes());
        bytes.extend_from_slice(&spine.part.to_be_bytes());
        append_optional_u128(&mut bytes, spine.parent);
        append_u64(&mut bytes, spine.rest_points.len() as u64);
        for (point, radius) in spine.rest_points.iter().zip(&spine.radii) {
            for value in point {
                bytes.extend_from_slice(&value.bits().to_be_bytes());
            }
            bytes.extend_from_slice(&radius.bits().to_be_bytes());
        }
    }
    bytes
}

pub(super) fn phenotype_section(asset: &PlantFamilyAsset) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/phenotypes/v2");
    append_u64(&mut bytes, asset.phenotypes.len() as u64);
    for phenotype in &asset.phenotypes {
        append_u32(&mut bytes, phenotype.id);
        bytes.push(match phenotype.role {
            saffron_vegetation::PhenotypeRole::Healthy => 0,
            saffron_vegetation::PhenotypeRole::Harvested => 1,
            saffron_vegetation::PhenotypeRole::Damaged => 2,
            saffron_vegetation::PhenotypeRole::Burned => 3,
            saffron_vegetation::PhenotypeRole::Dead => 4,
            saffron_vegetation::PhenotypeRole::Flowering => 5,
            saffron_vegetation::PhenotypeRole::Fruiting => 6,
            saffron_vegetation::PhenotypeRole::Senescent => 7,
            saffron_vegetation::PhenotypeRole::Wet => 8,
        });
        append_phenotype_response(&mut bytes, phenotype.response);
        append_u32(&mut bytes, phenotype.variation);
        append_u64(&mut bytes, phenotype.material_remap.len() as u64);
        for (from, to) in &phenotype.material_remap {
            append_u32(&mut bytes, *from);
            append_u32(&mut bytes, *to);
        }
        append_u128_list(&mut bytes, &phenotype.active_parts);
    }
    bytes
}

pub(super) fn collision_section(
    asset: &PlantFamilyAsset,
    family: &NormalizedPlantFamily,
) -> Vec<u8> {
    let mut bytes = mesh_section(family, PlantSourceRole::Collision, false);
    append_u64(&mut bytes, asset.collision_proxies.len() as u64);
    for proxy in &asset.collision_proxies {
        bytes.extend_from_slice(&proxy.id.to_be_bytes());
        bytes.push(match proxy.shape {
            saffron_vegetation::PlantCollisionShape::Box => 0,
            saffron_vegetation::PlantCollisionShape::Sphere => 1,
            saffron_vegetation::PlantCollisionShape::Capsule => 2,
            saffron_vegetation::PlantCollisionShape::ConvexHull => 3,
        });
        bytes.extend_from_slice(&proxy.part.to_be_bytes());
        for value in proxy.center.into_iter().chain(proxy.dimensions) {
            bytes.extend_from_slice(&value.bits().to_be_bytes());
        }
        bytes.push(u8::from(proxy.breakable));
    }
    bytes
}

pub(super) fn navigation_section(
    asset: &PlantFamilyAsset,
    family: &NormalizedPlantFamily,
) -> Vec<u8> {
    let mut bytes = mesh_section(family, PlantSourceRole::Navigation, false);
    append_u64(&mut bytes, asset.navigation_proxies.len() as u64);
    for proxy in &asset.navigation_proxies {
        bytes.extend_from_slice(&proxy.id.to_be_bytes());
        append_u64(&mut bytes, proxy.footprint.len() as u64);
        for point in &proxy.footprint {
            for value in point {
                bytes.extend_from_slice(&value.bits().to_be_bytes());
            }
        }
        bytes.extend_from_slice(&proxy.height.bits().to_be_bytes());
        bytes.extend_from_slice(&proxy.cost.bits().to_be_bytes());
    }
    bytes
}

pub(super) fn provenance_section(asset: &PlantFamilyAsset) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/provenance/v1");
    match &asset.source {
        PlantFamilySource::Imported(recipe) => {
            bytes.push(0);
            append_u64(&mut bytes, recipe.sources.len() as u64);
            for source in &recipe.sources {
                bytes.extend_from_slice(&source.id.to_be_bytes());
                match &source.locator {
                    PlantSourceLocator::Asset(asset) => {
                        bytes.push(0);
                        append_u64(&mut bytes, asset.value());
                    }
                    PlantSourceLocator::File(uri) => {
                        bytes.push(1);
                        append_string(&mut bytes, uri);
                    }
                }
                bytes.extend_from_slice(&source.content_hash);
                for value in [
                    &source.provenance.source,
                    &source.provenance.source_uri,
                    &source.provenance.license_id,
                    &source.provenance.license_uri,
                    &source.provenance.author,
                    &source.provenance.attribution,
                ] {
                    append_string(&mut bytes, value);
                }
                bytes.push(u8::from(source.provenance.requires_attribution));
            }
        }
        PlantFamilySource::Native { graph, .. } => {
            bytes.push(1);
            append_u64(&mut bytes, 1);
            bytes.extend_from_slice(&native_plant_source_id(asset.id).to_be_bytes());
            bytes.extend_from_slice(&graph.identity().bytes());
            bytes.extend_from_slice(&native_botanical_graph_content_hash(graph));
        }
    }
    bytes
}

pub(super) fn validation_section(compile: &PlantCompileOutput) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/validation/v1");
    append_u64(&mut bytes, compile.diagnostics.len() as u64);
    for diagnostic in &compile.diagnostics {
        bytes.push(match diagnostic.severity {
            PlantCompileDiagnosticSeverity::Info => 0,
            PlantCompileDiagnosticSeverity::Warning => 1,
            PlantCompileDiagnosticSeverity::Error => 2,
        });
        bytes.push(diagnostic_code_tag(diagnostic.code));
        append_optional_u128(&mut bytes, diagnostic.source);
        append_optional_selector(&mut bytes, diagnostic.selector.as_ref());
        append_string(&mut bytes, &diagnostic.path);
        append_string(&mut bytes, &diagnostic.message);
    }
    for value in [
        compile.statistics.sources,
        compile.statistics.meshes,
        compile.statistics.vertices,
        compile.statistics.indices,
        compile.statistics.joints,
        compile.statistics.materials,
        compile.statistics.rejected,
    ] {
        append_u64(&mut bytes, value);
    }
    append_u64(&mut bytes, compile.source_updates.len() as u64);
    for update in &compile.source_updates {
        bytes.extend_from_slice(&update.source.to_be_bytes());
        bytes.extend_from_slice(&update.previous);
        bytes.extend_from_slice(&update.current);
    }
    append_u64(&mut bytes, compile.conflicts.conflicts.len() as u64);
    for conflict in &compile.conflicts.conflicts {
        bytes.extend_from_slice(&conflict.target.to_be_bytes());
        bytes.extend_from_slice(&conflict.source.to_be_bytes());
        append_selector(&mut bytes, &conflict.selector);
        append_destination(&mut bytes, conflict.destination);
        bytes.push(match conflict.reason {
            PlantReimportConflictReason::MissingSource => 0,
            PlantReimportConflictReason::MissingElement => 1,
        });
    }
    bytes
}

fn append_normalized_mesh(
    bytes: &mut Vec<u8>,
    mesh: &NormalizedPlantMesh,
    include_materials: bool,
) {
    bytes.extend_from_slice(&mesh.source.to_be_bytes());
    append_selector(bytes, &mesh.selector);
    append_u64(bytes, mesh.vertices.len() as u64);
    for vertex in &mesh.vertices {
        for value in vertex.position_bits {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        for value in vertex.normal_snorm {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        for value in vertex.uv_bits {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        for value in vertex.tangent_snorm {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
    }
    append_u64(bytes, mesh.indices.len() as u64);
    for index in &mesh.indices {
        append_u32(bytes, *index);
    }
    if include_materials {
        append_u64(bytes, mesh.submeshes.len() as u64);
        for submesh in &mesh.submeshes {
            append_u32(bytes, submesh.first_index);
            append_u32(bytes, submesh.index_count);
            append_u32(bytes, submesh.material_slot);
        }
    } else {
        append_u64(bytes, 0);
    }
    // Skinning binds through the skeleton section, so a mesh row carries an empty stream.
    append_u64(bytes, 0);
}

fn append_import_settings(bytes: &mut Vec<u8>, settings: &PlantImportSettings) {
    bytes.push(match settings.units {
        SourceUnits::Meters => 0,
        SourceUnits::Centimeters => 1,
        SourceUnits::Millimeters => 2,
        SourceUnits::Feet => 3,
    });
    bytes.push(axis_tag(settings.up_axis));
    bytes.push(axis_tag(settings.forward_axis));
    bytes.push(match settings.handedness {
        SourceHandedness::Right => 0,
        SourceHandedness::Left => 1,
    });
    bytes.extend_from_slice(&settings.scale.bits().to_be_bytes());
    match &settings.pivot {
        PlantPivot::SourceOrigin => bytes.push(0),
        PlantPivot::BoundsBaseCenter => bytes.push(1),
        PlantPivot::Explicit(position) => {
            bytes.push(2);
            for value in position {
                bytes.extend_from_slice(&value.bits().to_be_bytes());
            }
        }
        PlantPivot::SemanticPart(part) => {
            bytes.push(3);
            bytes.extend_from_slice(&part.to_be_bytes());
        }
    }
    bytes.push(match settings.winding {
        SourceWinding::CounterClockwise => 0,
        SourceWinding::Clockwise => 1,
    });
    bytes.push(match settings.uv_origin {
        SourceUvOrigin::TopLeft => 0,
        SourceUvOrigin::BottomLeft => 1,
    });
    for value in settings.uv_scale.into_iter().chain(settings.uv_offset) {
        bytes.extend_from_slice(&value.bits().to_be_bytes());
    }
    bytes.push(match settings.tangent_policy {
        PlantTangentPolicy::Require => 0,
        PlantTangentPolicy::GenerateMissing => 1,
        PlantTangentPolicy::Regenerate => 2,
    });
}

fn append_dimensions(bytes: &mut Vec<u8>, dimensions: saffron_vegetation::PlantDimensions) {
    for value in [dimensions.height, dimensions.trunk_radius]
        .into_iter()
        .chain(dimensions.crown_radius)
        .chain(dimensions.root_radius)
        .chain(dimensions.local_bounds_min)
        .chain(dimensions.local_bounds_max)
    {
        bytes.extend_from_slice(&value.bits().to_be_bytes());
    }
}

fn append_field_channel(bytes: &mut Vec<u8>, channel: saffron_spatial::FieldChannel) {
    let (tag, user) = channel.canonical_code();
    bytes.push(tag);
    append_u64(bytes, user);
}

fn append_destination(bytes: &mut Vec<u8>, destination: PlantSemanticDestination) {
    match destination {
        PlantSemanticDestination::Part(id) => {
            bytes.push(0);
            bytes.extend_from_slice(&id.to_be_bytes());
        }
        PlantSemanticDestination::Spine(id) => {
            bytes.push(1);
            bytes.extend_from_slice(&id.to_be_bytes());
        }
        PlantSemanticDestination::MaterialSlot(slot) => {
            bytes.push(2);
            append_u32(bytes, slot);
        }
        PlantSemanticDestination::CollisionProxy(id) => {
            bytes.push(3);
            bytes.extend_from_slice(&id.to_be_bytes());
        }
        PlantSemanticDestination::NavigationProxy(id) => {
            bytes.push(4);
            bytes.extend_from_slice(&id.to_be_bytes());
        }
        PlantSemanticDestination::Phenotype(id) => {
            bytes.push(5);
            append_u32(bytes, id);
        }
    }
}

pub(super) fn append_selector(bytes: &mut Vec<u8>, selector: &PlantSourceSelector) {
    match selector {
        PlantSourceSelector::Whole => bytes.push(0),
        PlantSourceSelector::Element { id, path } => {
            bytes.push(1);
            bytes.extend_from_slice(&id.to_be_bytes());
            append_string(bytes, path);
        }
        PlantSourceSelector::Submesh { element, index } => {
            bytes.push(2);
            bytes.extend_from_slice(&element.to_be_bytes());
            append_u32(bytes, *index);
        }
    }
}

pub(super) fn append_optional_selector(
    bytes: &mut Vec<u8>,
    selector: Option<&PlantSourceSelector>,
) {
    match selector {
        Some(selector) => {
            bytes.push(1);
            append_selector(bytes, selector);
        }
        None => bytes.push(0),
    }
}

pub(super) fn append_skin(bytes: &mut Vec<u8>, skin: &[VertexSkin]) {
    append_u64(bytes, skin.len() as u64);
    for skin in skin {
        for joint in skin.joints {
            bytes.extend_from_slice(&joint.to_be_bytes());
        }
        for weight in skin.weights {
            append_u32(bytes, weight.to_bits());
        }
    }
}

pub(super) fn append_json(bytes: &mut Vec<u8>, value: &saffron_json::Value) {
    append_bytes(bytes, saffron_json::dump_json_sorted(value, -1).as_bytes());
}

/// Encodes one phenotype's intrinsic expression curve: each band as a presence byte plus
/// its two endpoints, then the shared edge ramp.
fn append_phenotype_response(bytes: &mut Vec<u8>, response: saffron_vegetation::PhenotypeResponse) {
    match response.season_window {
        Some((start, end)) => {
            bytes.push(1);
            bytes.extend_from_slice(&start.to_le_bytes());
            bytes.extend_from_slice(&end.to_le_bytes());
        }
        None => bytes.push(0),
    }
    for band in [response.health_band, response.moisture_band] {
        match band {
            Some((low, high)) => {
                bytes.push(1);
                bytes.extend_from_slice(&low.bits().to_le_bytes());
                bytes.extend_from_slice(&high.bits().to_le_bytes());
            }
            None => bytes.push(0),
        }
    }
    bytes.extend_from_slice(&response.ramp_mille.to_le_bytes());
}

pub(super) fn append_domain(bytes: &mut Vec<u8>, domain: &[u8]) {
    append_bytes(bytes, domain);
}

pub(super) fn append_string(bytes: &mut Vec<u8>, value: &str) {
    append_bytes(bytes, value.as_bytes());
}

pub(super) fn append_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    append_u64(bytes, value.len() as u64);
    bytes.extend_from_slice(value);
}

fn append_u128_list(bytes: &mut Vec<u8>, values: &[u128]) {
    append_u64(bytes, values.len() as u64);
    for value in values {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
}

fn append_optional_u128(bytes: &mut Vec<u8>, value: Option<u128>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        None => bytes.push(0),
    }
}

pub(super) fn append_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

pub(super) fn append_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn source_role_tag(role: PlantSourceRole) -> u8 {
    match role {
        PlantSourceRole::Geometry => 0,
        PlantSourceRole::Material => 1,
        PlantSourceRole::Skeleton => 2,
        PlantSourceRole::Collision => 3,
        PlantSourceRole::Navigation => 4,
    }
}

fn axis_tag(axis: SourceAxis) -> u8 {
    match axis {
        SourceAxis::PositiveX => 0,
        SourceAxis::NegativeX => 1,
        SourceAxis::PositiveY => 2,
        SourceAxis::NegativeY => 3,
        SourceAxis::PositiveZ => 4,
        SourceAxis::NegativeZ => 5,
    }
}

fn part_semantic_tag(semantic: PlantPartSemantic) -> u8 {
    match semantic {
        PlantPartSemantic::Trunk => 0,
        PlantPartSemantic::Branch => 1,
        PlantPartSemantic::Root => 2,
        PlantPartSemantic::Frond => 3,
        PlantPartSemantic::Leaf => 4,
        PlantPartSemantic::Flower => 5,
        PlantPartSemantic::Fruit => 6,
        PlantPartSemantic::Blade => 7,
    }
}

fn diagnostic_code_tag(code: PlantCompileDiagnosticCode) -> u8 {
    match code {
        PlantCompileDiagnosticCode::MissingSource => 0,
        PlantCompileDiagnosticCode::DuplicateSource => 1,
        PlantCompileDiagnosticCode::EmptySelection => 2,
        PlantCompileDiagnosticCode::InvalidGeometry => 3,
        PlantCompileDiagnosticCode::MissingMaterial => 4,
        PlantCompileDiagnosticCode::InvalidMaterial => 5,
        PlantCompileDiagnosticCode::InvalidSkeleton => 6,
        PlantCompileDiagnosticCode::MissingCoverageUv => 7,
        PlantCompileDiagnosticCode::InvalidLeafOrientation => 8,
        PlantCompileDiagnosticCode::BoundsMismatch => 9,
        PlantCompileDiagnosticCode::LimitExceeded => 10,
        PlantCompileDiagnosticCode::SourceChanged => 11,
        PlantCompileDiagnosticCode::OrphanedEdit => 12,
    }
}

#[cfg(test)]
mod tests {
    use super::super::recook_plant_family;
    use super::super::test_support::{
        fixed, fixture_server, imported_family, native_family, options, save_family,
    };
    use super::*;
    use crate::PlantRecookOutcome;
    use saffron_core::Uuid;
    use saffron_spatial::UnitInterval;
    use saffron_vegetation::{MechanicalResponse, PlantCompiledArtifactIndex};

    #[test]
    fn compiled_part_table_starts_with_canonical_family_tags() {
        let family = native_family(Uuid(8_001));
        let bytes = part_table_section(&family);
        let domain = b"saffron-anima/splantc/part-table/v2";
        let count_offset = 8 + domain.len();
        assert_eq!(
            u64::from_be_bytes(bytes[..8].try_into().unwrap()),
            u64::try_from(domain.len()).unwrap()
        );
        assert_eq!(&bytes[8..count_offset], domain);
        assert_eq!(
            u64::from_be_bytes(bytes[count_offset..count_offset + 8].try_into().unwrap()),
            1
        );
        assert_eq!(
            u64::from_be_bytes(
                bytes[count_offset + 8..count_offset + 16]
                    .try_into()
                    .unwrap()
            ),
            17
        );
    }

    #[test]
    fn the_cooked_part_table_returns_the_authored_mechanical_response() {
        // The writer emits mechanics into the part table and the wind prepass consumes them.
        // A reader that drifted from the writer by one field would decode a plausible wrong
        // number rather than fail, so the round trip is asserted on a distinctive value.
        let (_scratch, mut assets, material, mesh) = fixture_server("mechanics-round-trip");
        let mut asset = imported_family(material, mesh);
        asset.mechanics = MechanicalResponse {
            stiffness: fixed(7),
            damping: UnitInterval::from_bits(9_000),
            drag: fixed(3),
            flutter: fixed(5),
            bend_limit: UnitInterval::from_bits(21_000),
            damage_threshold: fixed(11),
            break_threshold: fixed(13),
        };
        let authored = asset.mechanics;
        let family = save_family(&mut assets, asset);
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };
        let bytes = std::fs::read(&published.publication.path).expect("artifact");
        let index = PlantCompiledArtifactIndex::open(
            &bytes,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .expect("artifact index");
        assert_eq!(
            index.mechanical_response(&bytes).expect("mechanics"),
            authored
        );
    }
}
