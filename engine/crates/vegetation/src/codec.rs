//! Canonical versioned codecs for vegetation-authored assets and sparse map chunks.

use saffron_core::Uuid;
use saffron_json::{Value, dump_json_sorted, parse_json};
use saffron_spatial::{
    DecisionScalar, DecisionVec3, FieldChannel, SurfaceAttachment, SurfacePrimitiveId,
    SurfaceProviderId, SurfaceRevision, UnitInterval, WorldBounds, WorldCellKey, WorldPosition,
};

use crate::hash::sha256;
use crate::*;

const PLANT_MAGIC: &[u8; 8] = b"SPLANT01";
const BIOME_MAGIC: &[u8; 8] = b"SBIOME01";
const MAP_MAGIC: &[u8; 8] = b"SVEGMAP1";
const MAP_CHUNK_MAGIC: &[u8; 8] = b"SVEGCH01";

/// SHA-256 identity of the `.splant` binary field vocabulary.
#[must_use]
pub fn plant_asset_schema_hash() -> [u8; 32] {
    sha256(b"saffron-anima/splant/schema/v4/typed-locators+per-source-provenance+role+selector+coordinate-policy+semantic-targets+family-tags+parts+dimensions+materials+spines+mechanics+variations+phenotypes+collision+navigation+interaction+habitat")
}

/// SHA-256 identity of the `.sbiome` binary field vocabulary.
#[must_use]
pub fn biome_asset_schema_hash() -> [u8; 32] {
    sha256(b"saffron-anima/sbiome/schema/v1/role+parameters+palette+density+clustering+suitability+competition+companions+succession+seeds+modules+policy+graph")
}

/// SHA-256 identity of the `.svegmap` manifest field vocabulary.
#[must_use]
pub fn vegetation_map_schema_hash() -> [u8; 32] {
    sha256(b"saffron-anima/svegmap/schema/v2/identity+bounds+chunk-layout+generation+canonical-content-addressed-inventory")
}

/// SHA-256 identity of one authored sparse map-chunk field vocabulary.
#[must_use]
pub fn vegetation_map_chunk_schema_hash() -> [u8; 32] {
    sha256(b"saffron-anima/svegmap-object/schema/v3/map+layer+global-or-cell+typed-field-anchor-graph-layer-editor-payload+revision+provenance")
}

/// Writes one plant family to canonical `.splant` bytes.
pub fn write_plant_asset(asset: &PlantFamilyAsset) -> Result<Vec<u8>> {
    validate_plant_family(asset)?;
    let mut writer = Writer::with_header(PLANT_MAGIC, asset.version, plant_asset_schema_hash());
    writer.uuid(asset.id);
    writer.string(&asset.name)?;
    writer.vec(&asset.tags, |writer, tag| {
        writer.u64(tag.value());
        Ok(())
    })?;
    write_plant_source(&mut writer, &asset.source)?;
    writer.vec(&asset.parts, |writer, part| {
        writer.u128(part.id);
        writer.option(part.parent, |writer, value| {
            writer.u128(value);
            Ok(())
        })?;
        writer.u8(plant_part_semantic_tag(part.semantic));
        writer.u32(part.material_slot);
        writer.vec(&part.sources, |writer, source| {
            writer.u128(*source);
            Ok(())
        })
    })?;
    write_dimensions(&mut writer, asset.dimensions);
    writer.vec(&asset.material_slots, |writer, material| {
        writer.uuid(*material);
        Ok(())
    })?;
    writer.vec(&asset.spines, write_spine)?;
    write_mechanics(&mut writer, asset.mechanics);
    writer.vec(&asset.variations, |writer, variation| {
        writer.u32(variation.id);
        writer.string(&variation.name)?;
        writer.vec(&variation.sources, |writer, source| {
            writer.u128(*source);
            Ok(())
        })?;
        writer.vec(&variation.active_parts, |writer, part| {
            writer.u128(*part);
            Ok(())
        })
    })?;
    writer.vec(&asset.phenotypes, |writer, phenotype| {
        writer.u32(phenotype.id);
        writer.u8(phenotype_role_tag(phenotype.role));
        writer.u32(phenotype.variation);
        writer.vec(&phenotype.material_remap, |writer, (from, to)| {
            writer.u32(*from);
            writer.u32(*to);
            Ok(())
        })?;
        writer.vec(&phenotype.active_parts, |writer, part| {
            writer.u128(*part);
            Ok(())
        })
    })?;
    writer.vec(&asset.collision_proxies, |writer, proxy| {
        writer.u128(proxy.id);
        writer.u8(collision_shape_tag(proxy.shape));
        writer.u128(proxy.part);
        writer.fixed3(proxy.center);
        writer.fixed3(proxy.dimensions);
        writer.bool(proxy.breakable);
        Ok(())
    })?;
    writer.vec(&asset.navigation_proxies, |writer, proxy| {
        writer.u128(proxy.id);
        writer.vec(&proxy.footprint, |writer, point| {
            writer.fixed2(*point);
            Ok(())
        })?;
        writer.fixed(proxy.height);
        writer.unit(proxy.cost);
        Ok(())
    })?;
    writer.u32(asset.interaction_policy as u32);
    writer.option(asset.habitat.as_ref(), |writer, habitat| {
        writer.vec(&habitat.fields, |writer, (channel, minimum, maximum)| {
            writer.field_channel(*channel);
            writer.fixed(*minimum);
            writer.fixed(*maximum);
            Ok(())
        })?;
        writer.vec(&habitat.surface_tags, |writer, tag| {
            writer.u64(*tag);
            Ok(())
        })?;
        writer.unit(habitat.shade_tolerance);
        Ok(())
    })?;
    Ok(writer.finish())
}

/// Reads and strictly validates one canonical `.splant` byte stream.
pub fn read_plant_asset(bytes: &[u8]) -> Result<PlantFamilyAsset> {
    let mut reader = Reader::with_header(
        bytes,
        PLANT_MAGIC,
        PLANT_ASSET_VERSION,
        plant_asset_schema_hash(),
        ".splant",
    )?;
    let asset = PlantFamilyAsset {
        version: PLANT_ASSET_VERSION,
        id: reader.uuid()?,
        name: reader.string()?,
        tags: reader.vec(|reader| PlantTagId::new(reader.u64()?))?,
        source: read_plant_source(&mut reader)?,
        parts: reader.vec(|reader| {
            Ok(PlantPart {
                id: reader.u128()?,
                parent: reader.option(Reader::u128)?,
                semantic: plant_part_semantic(reader.u8()?)?,
                material_slot: reader.u32()?,
                sources: reader.vec(Reader::u128)?,
            })
        })?,
        dimensions: read_dimensions(&mut reader)?,
        material_slots: reader.vec(Reader::uuid)?,
        spines: reader.vec(read_spine)?,
        mechanics: read_mechanics(&mut reader)?,
        variations: reader.vec(|reader| {
            Ok(PlantVariation {
                id: reader.u32()?,
                name: reader.string()?,
                sources: reader.vec(Reader::u128)?,
                active_parts: reader.vec(Reader::u128)?,
            })
        })?,
        phenotypes: reader.vec(|reader| {
            Ok(PlantPhenotype {
                id: reader.u32()?,
                role: phenotype_role(reader.u8()?)?,
                variation: reader.u32()?,
                material_remap: reader.vec(|reader| Ok((reader.u32()?, reader.u32()?)))?,
                active_parts: reader.vec(Reader::u128)?,
            })
        })?,
        collision_proxies: reader.vec(|reader| {
            Ok(PlantCollisionProxy {
                id: reader.u128()?,
                shape: collision_shape(reader.u8()?)?,
                part: reader.u128()?,
                center: reader.fixed3()?,
                dimensions: reader.fixed3()?,
                breakable: reader.bool()?,
            })
        })?,
        navigation_proxies: reader.vec(|reader| {
            Ok(PlantNavigationProxy {
                id: reader.u128()?,
                footprint: reader.vec(Reader::fixed2)?,
                height: reader.fixed()?,
                cost: reader.unit()?,
            })
        })?,
        interaction_policy: InteractionPolicy::try_from(reader.u32()?)?,
        habitat: reader.option(|reader| {
            Ok(HabitatPreferences {
                fields: reader.vec(|reader| {
                    Ok((reader.field_channel()?, reader.fixed()?, reader.fixed()?))
                })?,
                surface_tags: reader.vec(Reader::u64)?,
                shade_tolerance: reader.unit()?,
            })
        })?,
    };
    reader.complete()?;
    validate_plant_family(&asset)?;
    Ok(asset)
}

fn write_plant_source(writer: &mut Writer, source: &PlantFamilySource) -> Result<()> {
    match source {
        PlantFamilySource::Imported(recipe) => {
            writer.u8(0);
            writer.vec(&recipe.sources, |writer, source| {
                writer.u128(source.id);
                write_source_locator(writer, &source.locator)?;
                writer.u8(source_role_tag(source.role));
                write_source_selector(writer, &source.selector)?;
                writer.bytes(&source.content_hash);
                write_import_settings(writer, &source.settings)?;
                write_source_provenance(writer, &source.provenance)?;
                Ok(())
            })?;
            writer.vec(&recipe.semantic_targets, |writer, target| {
                writer.u128(target.id);
                writer.u128(target.source);
                write_source_selector(writer, &target.selector)?;
                write_semantic_destination(writer, target.destination);
                Ok(())
            })
        }
        PlantFamilySource::Native(graph) => {
            writer.u8(1);
            writer.bytes(&graph.schema_hash);
            writer.value(&graph.graph)
        }
    }
}

fn read_plant_source(reader: &mut Reader<'_>) -> Result<PlantFamilySource> {
    match reader.u8()? {
        0 => Ok(PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
            sources: reader.vec(|reader| {
                Ok(PlantSourceReference {
                    id: reader.u128()?,
                    locator: read_source_locator(reader)?,
                    role: source_role(reader.u8()?)?,
                    selector: read_source_selector(reader)?,
                    content_hash: reader.array()?,
                    settings: read_import_settings(reader)?,
                    provenance: read_source_provenance(reader)?,
                })
            })?,
            semantic_targets: reader.vec(|reader| {
                Ok(PlantManualSemanticTarget {
                    id: reader.u128()?,
                    source: reader.u128()?,
                    selector: read_source_selector(reader)?,
                    destination: read_semantic_destination(reader)?,
                })
            })?,
        })),
        1 => Ok(PlantFamilySource::Native(NativeBotanicalGraph {
            schema_hash: reader.array()?,
            graph: reader.value()?,
        })),
        _ => Err(reader.invalid("source")),
    }
}

fn write_source_provenance(writer: &mut Writer, value: &SourceProvenance) -> Result<()> {
    writer.string(&value.source)?;
    writer.string(&value.source_uri)?;
    writer.string(&value.license_id)?;
    writer.string(&value.license_uri)?;
    writer.string(&value.author)?;
    writer.string(&value.attribution)?;
    writer.bool(value.requires_attribution);
    Ok(())
}

fn read_source_provenance(reader: &mut Reader<'_>) -> Result<SourceProvenance> {
    Ok(SourceProvenance {
        source: reader.string()?,
        source_uri: reader.string()?,
        license_id: reader.string()?,
        license_uri: reader.string()?,
        author: reader.string()?,
        attribution: reader.string()?,
        requires_attribution: reader.bool()?,
    })
}

fn write_source_locator(writer: &mut Writer, locator: &PlantSourceLocator) -> Result<()> {
    match locator {
        PlantSourceLocator::Asset(asset) => {
            writer.u8(0);
            writer.uuid(*asset);
        }
        PlantSourceLocator::File(uri) => {
            writer.u8(1);
            writer.string(uri)?;
        }
    }
    Ok(())
}

fn read_source_locator(reader: &mut Reader<'_>) -> Result<PlantSourceLocator> {
    match reader.u8()? {
        0 => Ok(PlantSourceLocator::Asset(reader.uuid()?)),
        1 => Ok(PlantSourceLocator::File(reader.string()?)),
        _ => Err(reader.invalid("source.locator")),
    }
}

fn write_source_selector(writer: &mut Writer, selector: &PlantSourceSelector) -> Result<()> {
    match selector {
        PlantSourceSelector::Whole => writer.u8(0),
        PlantSourceSelector::Element { id, path } => {
            writer.u8(1);
            writer.u128(*id);
            writer.string(path)?;
        }
        PlantSourceSelector::Submesh { element, index } => {
            writer.u8(2);
            writer.u128(*element);
            writer.u32(*index);
        }
    }
    Ok(())
}

fn read_source_selector(reader: &mut Reader<'_>) -> Result<PlantSourceSelector> {
    match reader.u8()? {
        0 => Ok(PlantSourceSelector::Whole),
        1 => Ok(PlantSourceSelector::Element {
            id: reader.u128()?,
            path: reader.string()?,
        }),
        2 => Ok(PlantSourceSelector::Submesh {
            element: reader.u128()?,
            index: reader.u32()?,
        }),
        _ => Err(reader.invalid("source.selector")),
    }
}

fn write_import_settings(writer: &mut Writer, settings: &PlantImportSettings) -> Result<()> {
    writer.u8(source_units_tag(settings.units));
    writer.u8(source_axis_tag(settings.up_axis));
    writer.u8(source_axis_tag(settings.forward_axis));
    writer.u8(source_handedness_tag(settings.handedness));
    writer.fixed(settings.scale);
    write_plant_pivot(writer, &settings.pivot);
    writer.u8(source_winding_tag(settings.winding));
    writer.u8(source_uv_origin_tag(settings.uv_origin));
    writer.fixed2(settings.uv_scale);
    writer.fixed2(settings.uv_offset);
    writer.u8(tangent_policy_tag(settings.tangent_policy));
    Ok(())
}

fn read_import_settings(reader: &mut Reader<'_>) -> Result<PlantImportSettings> {
    Ok(PlantImportSettings {
        units: source_units(reader.u8()?)?,
        up_axis: source_axis(reader.u8()?)?,
        forward_axis: source_axis(reader.u8()?)?,
        handedness: source_handedness(reader.u8()?)?,
        scale: reader.fixed()?,
        pivot: read_plant_pivot(reader)?,
        winding: source_winding(reader.u8()?)?,
        uv_origin: source_uv_origin(reader.u8()?)?,
        uv_scale: reader.fixed2()?,
        uv_offset: reader.fixed2()?,
        tangent_policy: tangent_policy(reader.u8()?)?,
    })
}

fn write_plant_pivot(writer: &mut Writer, pivot: &PlantPivot) {
    match pivot {
        PlantPivot::SourceOrigin => writer.u8(0),
        PlantPivot::BoundsBaseCenter => writer.u8(1),
        PlantPivot::Explicit(position) => {
            writer.u8(2);
            writer.fixed3(*position);
        }
        PlantPivot::SemanticPart(part) => {
            writer.u8(3);
            writer.u128(*part);
        }
    }
}

fn read_plant_pivot(reader: &mut Reader<'_>) -> Result<PlantPivot> {
    match reader.u8()? {
        0 => Ok(PlantPivot::SourceOrigin),
        1 => Ok(PlantPivot::BoundsBaseCenter),
        2 => Ok(PlantPivot::Explicit(reader.fixed3()?)),
        3 => Ok(PlantPivot::SemanticPart(reader.u128()?)),
        _ => Err(reader.invalid("source.settings.pivot")),
    }
}

fn write_semantic_destination(writer: &mut Writer, destination: PlantSemanticDestination) {
    match destination {
        PlantSemanticDestination::Part(id) => {
            writer.u8(0);
            writer.u128(id);
        }
        PlantSemanticDestination::Spine(id) => {
            writer.u8(1);
            writer.u128(id);
        }
        PlantSemanticDestination::MaterialSlot(slot) => {
            writer.u8(2);
            writer.u32(slot);
        }
        PlantSemanticDestination::CollisionProxy(id) => {
            writer.u8(3);
            writer.u128(id);
        }
        PlantSemanticDestination::NavigationProxy(id) => {
            writer.u8(4);
            writer.u128(id);
        }
        PlantSemanticDestination::Phenotype(id) => {
            writer.u8(5);
            writer.u32(id);
        }
    }
}

fn read_semantic_destination(reader: &mut Reader<'_>) -> Result<PlantSemanticDestination> {
    match reader.u8()? {
        0 => Ok(PlantSemanticDestination::Part(reader.u128()?)),
        1 => Ok(PlantSemanticDestination::Spine(reader.u128()?)),
        2 => Ok(PlantSemanticDestination::MaterialSlot(reader.u32()?)),
        3 => Ok(PlantSemanticDestination::CollisionProxy(reader.u128()?)),
        4 => Ok(PlantSemanticDestination::NavigationProxy(reader.u128()?)),
        5 => Ok(PlantSemanticDestination::Phenotype(reader.u32()?)),
        _ => Err(reader.invalid("source.semanticTargets.destination")),
    }
}

fn write_dimensions(writer: &mut Writer, value: PlantDimensions) {
    writer.fixed(value.height);
    writer.fixed(value.trunk_radius);
    writer.fixed2(value.crown_radius);
    writer.fixed2(value.root_radius);
    writer.fixed3(value.local_bounds_min);
    writer.fixed3(value.local_bounds_max);
}

fn read_dimensions(reader: &mut Reader<'_>) -> Result<PlantDimensions> {
    Ok(PlantDimensions {
        height: reader.fixed()?,
        trunk_radius: reader.fixed()?,
        crown_radius: reader.fixed2()?,
        root_radius: reader.fixed2()?,
        local_bounds_min: reader.fixed3()?,
        local_bounds_max: reader.fixed3()?,
    })
}

fn write_spine(writer: &mut Writer, spine: &StructuralSpine) -> Result<()> {
    writer.u128(spine.id);
    writer.u128(spine.part);
    writer.option(spine.parent, |writer, value| {
        writer.u128(value);
        Ok(())
    })?;
    writer.vec(&spine.rest_points, |writer, point| {
        writer.fixed3(*point);
        Ok(())
    })?;
    writer.vec(&spine.radii, |writer, radius| {
        writer.fixed(*radius);
        Ok(())
    })
}

fn read_spine(reader: &mut Reader<'_>) -> Result<StructuralSpine> {
    Ok(StructuralSpine {
        id: reader.u128()?,
        part: reader.u128()?,
        parent: reader.option(Reader::u128)?,
        rest_points: reader.vec(Reader::fixed3)?,
        radii: reader.vec(Reader::fixed)?,
    })
}

fn write_mechanics(writer: &mut Writer, value: MechanicalResponse) {
    writer.fixed(value.stiffness);
    writer.unit(value.damping);
    writer.fixed(value.drag);
    writer.fixed(value.flutter);
    writer.unit(value.bend_limit);
    writer.fixed(value.damage_threshold);
    writer.fixed(value.break_threshold);
}

fn read_mechanics(reader: &mut Reader<'_>) -> Result<MechanicalResponse> {
    Ok(MechanicalResponse {
        stiffness: reader.fixed()?,
        damping: reader.unit()?,
        drag: reader.fixed()?,
        flutter: reader.fixed()?,
        bend_limit: reader.unit()?,
        damage_threshold: reader.fixed()?,
        break_threshold: reader.fixed()?,
    })
}

/// Writes one root biome or reusable module to canonical `.sbiome` bytes.
pub fn write_biome_asset(asset: &BiomeAsset) -> Result<Vec<u8>> {
    validate_biome(asset)?;
    let mut writer = Writer::with_header(BIOME_MAGIC, asset.version, biome_asset_schema_hash());
    writer.uuid(asset.id);
    writer.string(&asset.name)?;
    writer.u8(match asset.role {
        BiomeRole::Root => 0,
        BiomeRole::Module => 1,
    });
    writer.vec(&asset.parameters, |writer, parameter| {
        writer.u128(parameter.id);
        writer.string(&parameter.name)?;
        writer.u8(biome_parameter_type_tag(parameter.parameter_type));
        writer.value(&parameter.default_value)
    })?;
    writer.vec(&asset.palette, |writer, entry| {
        writer.uuid(entry.plant);
        writer.unit(entry.weight);
        writer.u128(entry.seed_namespace);
        Ok(())
    })?;
    writer.fixed(asset.density);
    writer.unit(asset.clustering);
    writer.vec(&asset.suitability, |writer, binding| {
        writer.field_channel(binding.channel);
        writer.fixed(binding.minimum);
        writer.fixed(binding.maximum);
        writer.fixed(binding.falloff);
        writer.u128(binding.node_guid);
        Ok(())
    })?;
    writer.vec(&asset.competition, |writer, rule| {
        writer.uuid(rule.first);
        writer.uuid(rule.second);
        writer.fixed(rule.spacing);
        writer.i32(rule.priority);
        Ok(())
    })?;
    writer.vec(&asset.companions, |writer, rule| {
        writer.uuid(rule.parent);
        writer.uuid(rule.child);
        writer.fixed(rule.minimum_distance);
        writer.fixed(rule.maximum_distance);
        writer.unit(rule.probability);
        Ok(())
    })?;
    writer.vec(&asset.succession, |writer, rule| {
        writer.uuid(rule.from);
        writer.uuid(rule.to);
        writer.u64(rule.minimum_tick);
        writer.unit(rule.probability);
        Ok(())
    })?;
    writer.vec(&asset.seed_namespaces, |writer, (name, namespace)| {
        writer.string(name)?;
        writer.u128(*namespace);
        Ok(())
    })?;
    writer.vec(&asset.modules, |writer, module| {
        writer.uuid(module.biome);
        writer.u128(module.call_guid);
        writer.vec(&module.bindings, |writer, (parameter, value)| {
            writer.u128(*parameter);
            writer.value(value)
        })
    })?;
    writer.u16(asset.policy.maximum_recursion);
    writer.fixed(asset.policy.maximum_influence_radius);
    writer.bool(asset.policy.require_authoritative_fields);
    writer.value(&asset.graph)?;
    Ok(writer.finish())
}

/// Reads and strictly validates one canonical `.sbiome` byte stream.
pub fn read_biome_asset(bytes: &[u8]) -> Result<BiomeAsset> {
    let mut reader = Reader::with_header(
        bytes,
        BIOME_MAGIC,
        BIOME_ASSET_VERSION,
        biome_asset_schema_hash(),
        ".sbiome",
    )?;
    let asset = BiomeAsset {
        version: BIOME_ASSET_VERSION,
        id: reader.uuid()?,
        name: reader.string()?,
        role: match reader.u8()? {
            0 => BiomeRole::Root,
            1 => BiomeRole::Module,
            _ => return Err(reader.invalid("role")),
        },
        parameters: reader.vec(|reader| {
            Ok(BiomeParameter {
                id: reader.u128()?,
                name: reader.string()?,
                parameter_type: biome_parameter_type(reader.u8()?)?,
                default_value: reader.value()?,
            })
        })?,
        palette: reader.vec(|reader| {
            Ok(BiomePaletteEntry {
                plant: reader.uuid()?,
                weight: reader.unit()?,
                seed_namespace: reader.u128()?,
            })
        })?,
        density: reader.fixed()?,
        clustering: reader.unit()?,
        suitability: reader.vec(|reader| {
            Ok(SuitabilityBinding {
                channel: reader.field_channel()?,
                minimum: reader.fixed()?,
                maximum: reader.fixed()?,
                falloff: reader.fixed()?,
                node_guid: reader.u128()?,
            })
        })?,
        competition: reader.vec(|reader| {
            Ok(CompetitionRule {
                first: reader.uuid()?,
                second: reader.uuid()?,
                spacing: reader.fixed()?,
                priority: reader.i32()?,
            })
        })?,
        companions: reader.vec(|reader| {
            Ok(CompanionRule {
                parent: reader.uuid()?,
                child: reader.uuid()?,
                minimum_distance: reader.fixed()?,
                maximum_distance: reader.fixed()?,
                probability: reader.unit()?,
            })
        })?,
        succession: reader.vec(|reader| {
            Ok(SuccessionRule {
                from: reader.uuid()?,
                to: reader.uuid()?,
                minimum_tick: reader.u64()?,
                probability: reader.unit()?,
            })
        })?,
        seed_namespaces: reader.vec(|reader| Ok((reader.string()?, reader.u128()?)))?,
        modules: reader.vec(|reader| {
            Ok(BiomeModuleReference {
                biome: reader.uuid()?,
                call_guid: reader.u128()?,
                bindings: reader.vec(|reader| Ok((reader.u128()?, reader.value()?)))?,
            })
        })?,
        policy: BiomeGraphPolicy {
            maximum_recursion: reader.u16()?,
            maximum_influence_radius: reader.fixed()?,
            require_authoritative_fields: reader.bool()?,
        },
        graph: reader.value()?,
    };
    reader.complete()?;
    validate_biome(&asset)?;
    Ok(asset)
}

fn biome_parameter_type_tag(value: BiomeParameterType) -> u8 {
    match value {
        BiomeParameterType::Scalar => 0,
        BiomeParameterType::Vector => 1,
        BiomeParameterType::Unit => 2,
        BiomeParameterType::Plant => 3,
        BiomeParameterType::Field => 4,
        BiomeParameterType::Boolean => 5,
    }
}

fn biome_parameter_type(value: u8) -> Result<BiomeParameterType> {
    match value {
        0 => Ok(BiomeParameterType::Scalar),
        1 => Ok(BiomeParameterType::Vector),
        2 => Ok(BiomeParameterType::Unit),
        3 => Ok(BiomeParameterType::Plant),
        4 => Ok(BiomeParameterType::Field),
        5 => Ok(BiomeParameterType::Boolean),
        _ => Err(invalid_enum(".sbiome", "parameters.type")),
    }
}

/// Writes one vegetation-map root to canonical `.svegmap` bytes.
pub fn write_vegetation_map_asset(asset: &VegetationMapAsset) -> Result<Vec<u8>> {
    validate_vegetation_map(asset)?;
    let mut writer = Writer::with_header(MAP_MAGIC, asset.version, vegetation_map_schema_hash());
    writer.uuid(asset.id);
    writer.string(&asset.name)?;
    writer.bounds(asset.bounds);
    writer.u8(asset.chunk_layout.level);
    writer.bytes(&asset.chunk_layout.schema_hash);
    writer.u64(asset.generation);
    writer.vec(&asset.inventory, |writer, reference| {
        write_map_chunk_key(writer, reference.key);
        writer.bytes(&reference.content_hash);
        writer.u64(reference.byte_length);
        writer.u64(reference.revision);
        Ok(())
    })?;
    Ok(writer.finish())
}

/// Reads and strictly validates one canonical `.svegmap` root byte stream.
pub fn read_vegetation_map_asset(bytes: &[u8]) -> Result<VegetationMapAsset> {
    let mut reader = Reader::with_header(
        bytes,
        MAP_MAGIC,
        VEGETATION_MAP_VERSION,
        vegetation_map_schema_hash(),
        ".svegmap",
    )?;
    let asset = VegetationMapAsset {
        version: VEGETATION_MAP_VERSION,
        id: reader.uuid()?,
        name: reader.string()?,
        bounds: reader.bounds()?,
        chunk_layout: VegetationMapChunkLayout {
            level: reader.u8()?,
            schema_hash: reader.array()?,
        },
        generation: reader.u64()?,
        inventory: reader.vec(|reader| {
            Ok(VegetationMapChunkReference {
                key: read_map_chunk_key(reader)?,
                content_hash: reader.array()?,
                byte_length: reader.u64()?,
                revision: reader.u64()?,
            })
        })?,
    };
    reader.complete()?;
    validate_vegetation_map(&asset)?;
    Ok(asset)
}

/// Writes one typed immutable authored map object to canonical internal bytes.
pub fn write_vegetation_map_chunk(chunk: &VegetationMapChunk) -> Result<Vec<u8>> {
    validate_map_chunk(chunk)?;
    let mut chunk = chunk.clone();
    canonicalize_map_chunk(&mut chunk);
    let mut writer = Writer::with_header(
        MAP_CHUNK_MAGIC,
        chunk.version,
        vegetation_map_chunk_schema_hash(),
    );
    writer.uuid(chunk.map);
    write_map_chunk_key(&mut writer, chunk.key);
    writer.u64(chunk.revision);
    match &chunk.payload {
        VegetationMapChunkPayload::Field(payload) => {
            writer.vec(&payload.fields, write_authored_field)?;
            writer.vec(&payload.blockers, write_authored_field)?;
        }
        VegetationMapChunkPayload::AnchorOverride(payload) => {
            writer.vec(&payload.explicit_plants, |writer, anchor| {
                writer.plant_id(anchor.id);
                writer.u128(anchor.layer);
                writer.uuid(anchor.family);
                write_plant_point(writer, &anchor.point)
            })?;
            writer.vec(&payload.pins, |writer, plant| {
                writer.plant_id(*plant);
                Ok(())
            })?;
            writer.vec(&payload.transform_overrides, |writer, value| {
                writer.plant_id(value.plant);
                writer.position(value.position);
                writer.fixed3(value.scale);
                Ok(())
            })?;
            writer.vec(&payload.state_overrides, |writer, value| {
                writer.plant_id(value.plant);
                writer.option(value.health, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.moisture, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.fuel, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.interaction_policy, |writer, value| {
                    writer.u32(value as u32);
                    Ok(())
                })
            })?;
            write_provenance(&mut writer, &payload.provenance)?;
        }
        VegetationMapChunkPayload::GraphInstance(instance) => {
            write_local_biome_instance(&mut writer, instance)?;
        }
        VegetationMapChunkPayload::LayerMetadata(layer) => write_layer(&mut writer, layer)?,
        VegetationMapChunkPayload::EditorMetadata(gestures) => {
            writer.vec(gestures, |writer, gesture| {
                writer.u128(gesture.gesture);
                writer.u128(gesture.layer);
                writer.vec(&gesture.samples, |writer, sample| {
                    writer.position(*sample);
                    Ok(())
                })
            })?;
        }
    }
    Ok(writer.finish())
}

fn write_provenance(writer: &mut Writer, provenance: &ProvenanceTable) -> Result<()> {
    writer.vec(provenance.decisions(), |writer, decision| {
        writer.vec(&decision.parents, |writer, parent| {
            writer.u32(parent.0);
            Ok(())
        })?;
        writer.vec(&decision.subgraph_path, |writer, call| {
            writer.u128(*call);
            Ok(())
        })?;
        writer.u128(decision.node);
        writer.string(decision.operator.as_wire())?;
        writer.u64(decision.candidate);
        writer.u8(provenance_outcome_tag(decision.outcome));
        Ok(())
    })?;
    writer.vec(provenance.records(), |writer, record| {
        writer.uuid(record.map);
        writer.u128(record.layer);
        writer.uuid(record.biome);
        writer.u32(record.decision.0);
        writer.u64(record.candidate);
        writer.option(record.family, |writer, family| {
            writer.uuid(family);
            Ok(())
        })?;
        writer.option(record.plant, |writer, plant| {
            writer.plant_id(plant);
            Ok(())
        })?;
        writer.u32(record.variation);
        Ok(())
    })
}

/// Reads and strictly validates one canonical immutable authored map object.
pub fn read_vegetation_map_chunk(bytes: &[u8]) -> Result<VegetationMapChunk> {
    let mut reader = Reader::with_header(
        bytes,
        MAP_CHUNK_MAGIC,
        VEGETATION_MAP_CHUNK_VERSION,
        vegetation_map_chunk_schema_hash(),
        ".svegmap chunk",
    )?;
    let map = reader.uuid()?;
    let key = read_map_chunk_key(&mut reader)?;
    let revision = reader.u64()?;
    let payload = match key.kind {
        VegetationMapChunkKind::Field => {
            VegetationMapChunkPayload::Field(VegetationMapFieldChunk {
                fields: reader.vec(read_authored_field)?,
                blockers: reader.vec(read_authored_field)?,
            })
        }
        VegetationMapChunkKind::AnchorOverride => {
            let explicit_plants = reader.vec(|reader| {
                Ok(ExplicitPlantAnchor {
                    id: reader.plant_id()?,
                    layer: reader.u128()?,
                    family: reader.uuid()?,
                    point: read_plant_point(reader)?,
                })
            })?;
            let pins = reader.vec(Reader::plant_id)?;
            let transform_overrides = reader.vec(|reader| {
                Ok(PlantTransformOverride {
                    plant: reader.plant_id()?,
                    position: reader.position()?,
                    scale: reader.fixed3()?,
                })
            })?;
            let state_overrides = reader.vec(|reader| {
                Ok(PlantStateOverride {
                    plant: reader.plant_id()?,
                    health: reader.option(Reader::unit)?,
                    moisture: reader.option(Reader::unit)?,
                    fuel: reader.option(Reader::unit)?,
                    interaction_policy: reader
                        .option(|reader| InteractionPolicy::try_from(reader.u32()?))?,
                })
            })?;
            VegetationMapChunkPayload::AnchorOverride(VegetationMapAnchorChunk {
                explicit_plants,
                pins,
                transform_overrides,
                state_overrides,
                provenance: read_provenance(&mut reader)?,
            })
        }
        VegetationMapChunkKind::GraphInstance => {
            VegetationMapChunkPayload::GraphInstance(read_local_biome_instance(&mut reader)?)
        }
        VegetationMapChunkKind::LayerMetadata => {
            VegetationMapChunkPayload::LayerMetadata(read_layer(&mut reader)?)
        }
        VegetationMapChunkKind::EditorMetadata => {
            VegetationMapChunkPayload::EditorMetadata(reader.vec(|reader| {
                Ok(BrushGestureMetadata {
                    gesture: reader.u128()?,
                    layer: reader.u128()?,
                    samples: reader.vec(Reader::position)?,
                })
            })?)
        }
    };
    let chunk = VegetationMapChunk {
        version: VEGETATION_MAP_CHUNK_VERSION,
        map,
        key,
        revision,
        payload,
    };
    reader.complete()?;
    validate_map_chunk(&chunk)?;
    Ok(chunk)
}

fn read_provenance(reader: &mut Reader<'_>) -> Result<ProvenanceTable> {
    let decisions: Vec<ProvenanceDecision> = reader.vec(|reader| {
        Ok(ProvenanceDecision {
            parents: reader.vec(|reader| Ok(ProvenanceDecisionHandle(reader.u32()?)))?,
            subgraph_path: reader.vec(Reader::u128)?,
            node: reader.u128()?,
            operator: GraphOperator::from_wire(&reader.string()?)
                .ok_or_else(|| reader.invalid("provenance.decision.operator"))?,
            candidate: reader.u64()?,
            outcome: provenance_outcome(reader.u8()?)?,
        })
    })?;
    let records: Vec<ProvenanceRecord> = reader.vec(|reader| {
        Ok(ProvenanceRecord {
            map: reader.uuid()?,
            layer: reader.u128()?,
            biome: reader.uuid()?,
            decision: ProvenanceDecisionHandle(reader.u32()?),
            candidate: reader.u64()?,
            family: reader.option(Reader::uuid)?,
            plant: reader.option(Reader::plant_id)?,
            variation: reader.u32()?,
        })
    })?;
    let mut provenance = ProvenanceTable::default();
    for (index, decision) in decisions.into_iter().enumerate() {
        let handle = provenance.intern_decision(decision);
        if usize::try_from(handle.0).ok() != Some(index) {
            return Err(reader.invalid("provenance.decisions"));
        }
    }
    for (index, record) in records.into_iter().enumerate() {
        let handle = provenance.intern(record);
        if usize::try_from(handle.0).ok() != Some(index) {
            return Err(reader.invalid("provenance.records"));
        }
    }
    Ok(provenance)
}

fn validate_map_chunk(chunk: &VegetationMapChunk) -> Result<()> {
    if chunk.version != VEGETATION_MAP_CHUNK_VERSION {
        return Err(Error::FormatVersion {
            format: ".svegmap chunk",
            found: chunk.version,
            expected: VEGETATION_MAP_CHUNK_VERSION,
        });
    }
    if chunk.map.value() == 0
        || chunk.key.layer == 0
        || chunk.key.kind != chunk.payload.kind()
        || !matches!(
            (chunk.key.kind, chunk.key.tile),
            (
                VegetationMapChunkKind::Field | VegetationMapChunkKind::AnchorOverride,
                VegetationMapTileKey::Cell(_)
            ) | (
                VegetationMapChunkKind::GraphInstance
                    | VegetationMapChunkKind::LayerMetadata
                    | VegetationMapChunkKind::EditorMetadata,
                VegetationMapTileKey::Global
            )
        )
    {
        return Err(Error::InvalidFormat {
            format: ".svegmap chunk",
            field: "map/key/payload".to_owned(),
        });
    }
    match &chunk.payload {
        VegetationMapChunkPayload::Field(payload) => {
            if payload.fields.is_empty() && payload.blockers.is_empty() {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "fields/blockers".to_owned(),
                });
            }
            let mut field_keys = std::collections::BTreeSet::new();
            for (blocker, field) in payload
                .fields
                .iter()
                .map(|field| (false, field))
                .chain(payload.blockers.iter().map(|field| (true, field)))
            {
                let sample_count =
                    field
                        .dimensions
                        .iter()
                        .try_fold(1_u64, |product, dimension| {
                            product
                                .checked_mul(u64::from(*dimension))
                                .ok_or(Error::NumericOverflow)
                        })?;
                if field.layer != chunk.key.layer
                    || field.quantum_bits <= 0
                    || field.dimensions.contains(&0)
                    || usize::try_from(sample_count).ok() != Some(field.values.len())
                    || !field_keys.insert((blocker, field.channel))
                {
                    return Err(Error::InvalidFormat {
                        format: ".svegmap chunk",
                        field: "fields/blockers".to_owned(),
                    });
                }
            }
        }
        VegetationMapChunkPayload::AnchorOverride(payload) => {
            let VegetationMapTileKey::Cell(cell) = chunk.key.tile else {
                unreachable!();
            };
            if payload.explicit_plants.is_empty()
                && payload.pins.is_empty()
                && payload.transform_overrides.is_empty()
                && payload.state_overrides.is_empty()
                && payload.provenance.records().is_empty()
            {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "anchorOverrides".to_owned(),
                });
            }
            let mut anchor_ids = std::collections::BTreeSet::new();
            for anchor in &payload.explicit_plants {
                anchor.point.validate()?;
                if anchor.id != anchor.point.id
                    || anchor.layer != chunk.key.layer
                    || anchor.family != anchor.point.family
                    || !cell.bounds().contains(anchor.point.position)
                    || anchor.id.namespace()? != PlantIdNamespace::Explicit
                    || payload
                        .provenance
                        .get(ProvenanceHandle(anchor.point.provenance))
                        .is_none()
                    || !anchor_ids.insert(anchor.id)
                {
                    return Err(Error::InvalidFormat {
                        format: ".svegmap chunk",
                        field: "explicitPlants".to_owned(),
                    });
                }
            }
            if !all_unique(payload.pins.iter().copied())
                || !all_unique(payload.transform_overrides.iter().map(|value| value.plant))
                || !all_unique(payload.state_overrides.iter().map(|value| value.plant))
            {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "pins/transformOverrides/stateOverrides".to_owned(),
                });
            }
            for (index, decision) in payload.provenance.decisions().iter().enumerate() {
                if decision.node == 0
                    || decision.parents.iter().any(|parent| {
                        usize::try_from(parent.0).map_or(true, |parent| parent >= index)
                    })
                {
                    return Err(Error::InvalidFormat {
                        format: ".svegmap chunk",
                        field: "provenance.decisions".to_owned(),
                    });
                }
            }
            for record in payload.provenance.records() {
                if record.map != chunk.map
                    || record.layer != chunk.key.layer
                    || payload.provenance.decision(record.decision).is_none()
                    || record.plant.is_some() && record.family.is_none()
                {
                    return Err(Error::InvalidFormat {
                        format: ".svegmap chunk",
                        field: "provenance.records".to_owned(),
                    });
                }
            }
        }
        VegetationMapChunkPayload::GraphInstance(instance) => {
            if instance.id != chunk.key.layer
                || instance.biome.value() == 0
                || !all_unique(instance.bindings.iter().map(|(parameter, _)| *parameter))
            {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "graphInstance".to_owned(),
                });
            }
        }
        VegetationMapChunkPayload::LayerMetadata(layer) => {
            if layer.id != chunk.key.layer
                || !all_unique(layer.dependencies.iter().copied())
                || layer.dependencies.contains(&layer.id)
            {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "layerMetadata.id".to_owned(),
                });
            }
        }
        VegetationMapChunkPayload::EditorMetadata(gestures) => {
            let mut ids = std::collections::BTreeSet::new();
            if gestures.is_empty()
                || gestures.iter().any(|gesture| {
                    gesture.gesture == 0
                        || gesture.layer != chunk.key.layer
                        || !ids.insert(gesture.gesture)
                })
            {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "editorMetadata".to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn canonicalize_map_chunk(chunk: &mut VegetationMapChunk) {
    match &mut chunk.payload {
        VegetationMapChunkPayload::Field(payload) => {
            payload.fields.sort_by_key(|field| field.channel);
            payload.blockers.sort_by_key(|field| field.channel);
        }
        VegetationMapChunkPayload::AnchorOverride(payload) => {
            payload.explicit_plants.sort_by_key(|anchor| anchor.id);
            payload.pins.sort_unstable();
            payload.transform_overrides.sort_by_key(|value| value.plant);
            payload.state_overrides.sort_by_key(|value| value.plant);
        }
        VegetationMapChunkPayload::GraphInstance(instance) => {
            instance.bindings.sort_by_key(|(parameter, _)| *parameter);
        }
        VegetationMapChunkPayload::LayerMetadata(layer) => {
            layer.dependencies.sort_unstable();
        }
        VegetationMapChunkPayload::EditorMetadata(gestures) => {
            gestures.sort_by_key(|gesture| gesture.gesture);
        }
    }
}

fn all_unique<T: Ord>(values: impl IntoIterator<Item = T>) -> bool {
    let mut unique = std::collections::BTreeSet::new();
    values.into_iter().all(|value| unique.insert(value))
}

fn write_local_biome_instance(writer: &mut Writer, instance: &LocalBiomeInstance) -> Result<()> {
    writer.u128(instance.id);
    writer.uuid(instance.biome);
    writer.bounds(instance.bounds);
    writer.vec(&instance.bindings, |writer, (parameter, value)| {
        writer.u128(*parameter);
        writer.value(value)
    })?;
    writer.u64(instance.revision);
    Ok(())
}

fn read_local_biome_instance(reader: &mut Reader<'_>) -> Result<LocalBiomeInstance> {
    Ok(LocalBiomeInstance {
        id: reader.u128()?,
        biome: reader.uuid()?,
        bounds: reader.bounds()?,
        bindings: reader.vec(|reader| Ok((reader.u128()?, reader.value()?)))?,
        revision: reader.u64()?,
    })
}

fn write_map_chunk_key(writer: &mut Writer, key: VegetationMapChunkKey) {
    writer.u128(key.layer);
    match key.tile {
        VegetationMapTileKey::Global => writer.u8(0),
        VegetationMapTileKey::Cell(cell) => {
            writer.u8(1);
            writer.cell(cell);
        }
    }
    writer.u8(match key.kind {
        VegetationMapChunkKind::Field => 0,
        VegetationMapChunkKind::AnchorOverride => 1,
        VegetationMapChunkKind::GraphInstance => 2,
        VegetationMapChunkKind::LayerMetadata => 3,
        VegetationMapChunkKind::EditorMetadata => 4,
    });
}

fn read_map_chunk_key(reader: &mut Reader<'_>) -> Result<VegetationMapChunkKey> {
    let layer = reader.u128()?;
    let tile = match reader.u8()? {
        0 => VegetationMapTileKey::Global,
        1 => VegetationMapTileKey::Cell(reader.cell()?),
        _ => return Err(invalid_enum(".svegmap chunk", "key.tile")),
    };
    let kind = match reader.u8()? {
        0 => VegetationMapChunkKind::Field,
        1 => VegetationMapChunkKind::AnchorOverride,
        2 => VegetationMapChunkKind::GraphInstance,
        3 => VegetationMapChunkKind::LayerMetadata,
        4 => VegetationMapChunkKind::EditorMetadata,
        _ => return Err(invalid_enum(".svegmap chunk", "key.kind")),
    };
    Ok(VegetationMapChunkKey { layer, tile, kind })
}

fn write_authored_field(writer: &mut Writer, field: &AuthoredFieldTile) -> Result<()> {
    writer.field_channel(field.channel);
    writer.u128(field.layer);
    for dimension in field.dimensions {
        writer.u32(dimension);
    }
    writer.i32(field.quantum_bits);
    writer.vec(&field.values, |writer, value| {
        writer.i32(*value);
        Ok(())
    })
}

fn read_authored_field(reader: &mut Reader<'_>) -> Result<AuthoredFieldTile> {
    Ok(AuthoredFieldTile {
        channel: reader.field_channel()?,
        layer: reader.u128()?,
        dimensions: [reader.u32()?, reader.u32()?, reader.u32()?],
        quantum_bits: reader.i32()?,
        values: reader.vec(Reader::i32)?,
    })
}

fn write_layer(writer: &mut Writer, layer: &VegetationLayer) -> Result<()> {
    writer.u128(layer.id);
    writer.string(&layer.name)?;
    writer.u8(match layer.coordinate_space {
        LayerCoordinateSpace::World => 0,
        LayerCoordinateSpace::Surface => 1,
        LayerCoordinateSpace::OwnerLocal => 2,
    });
    writer.bounds(layer.bounds);
    write_layer_operator(writer, &layer.operator)?;
    writer.vec(&layer.dependencies, |writer, dependency| {
        writer.u128(*dependency);
        Ok(())
    })?;
    writer.i32(layer.order);
    writer.bool(layer.locked);
    writer.bool(layer.muted);
    writer.u64(layer.revision);
    Ok(())
}

fn read_layer(reader: &mut Reader<'_>) -> Result<VegetationLayer> {
    Ok(VegetationLayer {
        id: reader.u128()?,
        name: reader.string()?,
        coordinate_space: match reader.u8()? {
            0 => LayerCoordinateSpace::World,
            1 => LayerCoordinateSpace::Surface,
            2 => LayerCoordinateSpace::OwnerLocal,
            _ => return Err(reader.invalid("layers.coordinateSpace")),
        },
        bounds: reader.bounds()?,
        operator: read_layer_operator(reader)?,
        dependencies: reader.vec(Reader::u128)?,
        order: reader.i32()?,
        locked: reader.bool()?,
        muted: reader.bool()?,
        revision: reader.u64()?,
    })
}

fn write_layer_operator(writer: &mut Writer, operator: &VegetationLayerOperator) -> Result<()> {
    match operator {
        VegetationLayerOperator::ScalarField(field) => {
            writer.u8(0);
            write_field_layer(writer, field);
        }
        VegetationLayerOperator::VectorField {
            channel,
            tile_set,
            value,
            blend,
        } => {
            writer.u8(1);
            writer.field_channel(*channel);
            writer.u128(*tile_set);
            writer.fixed3([value.x, value.y, value.z]);
            writer.u8(field_blend_tag(*blend));
        }
        VegetationLayerOperator::SpeciesWeights(weights) => {
            writer.u8(2);
            writer.vec(weights, |writer, weight| {
                writer.uuid(weight.family);
                writer.unit(weight.weight);
                Ok(())
            })?;
        }
        VegetationLayerOperator::Density(field) => {
            writer.u8(3);
            write_field_layer(writer, field);
        }
        VegetationLayerOperator::Mask {
            tile_set,
            operation,
        } => {
            writer.u8(4);
            writer.u128(*tile_set);
            writer.u8(inclusion_tag(*operation));
        }
        VegetationLayerOperator::Volume(volume) => {
            writer.u8(5);
            writer.bounds(volume.bounds);
            writer.u8(inclusion_tag(volume.operation));
            writer.fixed(volume.falloff);
        }
        VegetationLayerOperator::Spline(spline) => {
            writer.u8(6);
            writer.u128(spline.spline);
            writer.vec(&spline.points, |writer, point| {
                writer.position(*point);
                Ok(())
            })?;
            writer.fixed(spline.radius);
            writer.u8(inclusion_tag(spline.operation));
        }
        VegetationLayerOperator::Anchors(plants) => {
            writer.u8(7);
            write_plant_ids(writer, plants)?;
        }
        VegetationLayerOperator::Pins(plants) => {
            writer.u8(8);
            write_plant_ids(writer, plants)?;
        }
        VegetationLayerOperator::TransformOverrides(values) => {
            writer.u8(9);
            writer.vec(values, |writer, value| {
                writer.plant_id(value.plant);
                writer.position(value.position);
                writer.fixed3(value.scale);
                Ok(())
            })?;
        }
        VegetationLayerOperator::StateOverrides(values) => {
            writer.u8(10);
            writer.vec(values, |writer, value| {
                writer.plant_id(value.plant);
                writer.option(value.health, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.moisture, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.fuel, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.interaction_policy, |writer, value| {
                    writer.u32(value as u32);
                    Ok(())
                })
            })?;
        }
        VegetationLayerOperator::Blocker {
            tile_set,
            categories,
        } => {
            writer.u8(11);
            writer.u128(*tile_set);
            writer.u32(*categories);
        }
    }
    Ok(())
}

fn read_layer_operator(reader: &mut Reader<'_>) -> Result<VegetationLayerOperator> {
    match reader.u8()? {
        0 => Ok(VegetationLayerOperator::ScalarField(read_field_layer(
            reader,
        )?)),
        1 => {
            let channel = reader.field_channel()?;
            let tile_set = reader.u128()?;
            let fixed = reader.fixed3()?;
            Ok(VegetationLayerOperator::VectorField {
                channel,
                tile_set,
                value: DecisionVec3 {
                    x: fixed[0],
                    y: fixed[1],
                    z: fixed[2],
                },
                blend: field_blend(reader.u8()?)?,
            })
        }
        2 => Ok(VegetationLayerOperator::SpeciesWeights(reader.vec(
            |reader| {
                Ok(SpeciesWeight {
                    family: reader.uuid()?,
                    weight: reader.unit()?,
                })
            },
        )?)),
        3 => Ok(VegetationLayerOperator::Density(read_field_layer(reader)?)),
        4 => Ok(VegetationLayerOperator::Mask {
            tile_set: reader.u128()?,
            operation: inclusion(reader.u8()?)?,
        }),
        5 => Ok(VegetationLayerOperator::Volume(VolumeLayer {
            bounds: reader.bounds()?,
            operation: inclusion(reader.u8()?)?,
            falloff: reader.fixed()?,
        })),
        6 => Ok(VegetationLayerOperator::Spline(SplineLayer {
            spline: reader.u128()?,
            points: reader.vec(Reader::position)?,
            radius: reader.fixed()?,
            operation: inclusion(reader.u8()?)?,
        })),
        7 => Ok(VegetationLayerOperator::Anchors(
            reader.vec(Reader::plant_id)?,
        )),
        8 => Ok(VegetationLayerOperator::Pins(reader.vec(Reader::plant_id)?)),
        9 => Ok(VegetationLayerOperator::TransformOverrides(reader.vec(
            |reader| {
                Ok(PlantTransformOverride {
                    plant: reader.plant_id()?,
                    position: reader.position()?,
                    scale: reader.fixed3()?,
                })
            },
        )?)),
        10 => Ok(VegetationLayerOperator::StateOverrides(reader.vec(
            |reader| {
                Ok(PlantStateOverride {
                    plant: reader.plant_id()?,
                    health: reader.option(Reader::unit)?,
                    moisture: reader.option(Reader::unit)?,
                    fuel: reader.option(Reader::unit)?,
                    interaction_policy: reader
                        .option(|reader| InteractionPolicy::try_from(reader.u32()?))?,
                })
            },
        )?)),
        11 => Ok(VegetationLayerOperator::Blocker {
            tile_set: reader.u128()?,
            categories: reader.u32()?,
        }),
        _ => Err(reader.invalid("layers.operator")),
    }
}

fn write_field_layer(writer: &mut Writer, field: &FieldTileLayer) {
    writer.field_channel(field.channel);
    writer.u128(field.tile_set);
    writer.u8(field_blend_tag(field.blend));
    writer.unit(field.weight);
}

fn read_field_layer(reader: &mut Reader<'_>) -> Result<FieldTileLayer> {
    Ok(FieldTileLayer {
        channel: reader.field_channel()?,
        tile_set: reader.u128()?,
        blend: field_blend(reader.u8()?)?,
        weight: reader.unit()?,
    })
}

fn write_plant_ids(writer: &mut Writer, plants: &[PlantId]) -> Result<()> {
    writer.vec(plants, |writer, plant| {
        writer.plant_id(*plant);
        Ok(())
    })
}

fn field_blend_tag(value: FieldBlendOperator) -> u8 {
    match value {
        FieldBlendOperator::Replace => 0,
        FieldBlendOperator::Add => 1,
        FieldBlendOperator::Multiply => 2,
        FieldBlendOperator::Minimum => 3,
        FieldBlendOperator::Maximum => 4,
    }
}

fn field_blend(value: u8) -> Result<FieldBlendOperator> {
    match value {
        0 => Ok(FieldBlendOperator::Replace),
        1 => Ok(FieldBlendOperator::Add),
        2 => Ok(FieldBlendOperator::Multiply),
        3 => Ok(FieldBlendOperator::Minimum),
        4 => Ok(FieldBlendOperator::Maximum),
        _ => Err(invalid_enum(".svegmap", "layers.blend")),
    }
}

fn inclusion_tag(value: InclusionOperator) -> u8 {
    match value {
        InclusionOperator::Include => 0,
        InclusionOperator::Exclude => 1,
    }
}

fn inclusion(value: u8) -> Result<InclusionOperator> {
    match value {
        0 => Ok(InclusionOperator::Include),
        1 => Ok(InclusionOperator::Exclude),
        _ => Err(invalid_enum(".svegmap", "layers.inclusion")),
    }
}

fn provenance_outcome_tag(value: ProvenanceDecisionOutcome) -> u8 {
    match value {
        ProvenanceDecisionOutcome::Produced => 0,
        ProvenanceDecisionOutcome::Retained => 1,
        ProvenanceDecisionOutcome::Accepted => 2,
        ProvenanceDecisionOutcome::Rejected => 3,
    }
}

fn provenance_outcome(value: u8) -> Result<ProvenanceDecisionOutcome> {
    match value {
        0 => Ok(ProvenanceDecisionOutcome::Produced),
        1 => Ok(ProvenanceDecisionOutcome::Retained),
        2 => Ok(ProvenanceDecisionOutcome::Accepted),
        3 => Ok(ProvenanceDecisionOutcome::Rejected),
        _ => Err(invalid_enum(
            ".svegmap chunk",
            "provenance.decision.outcome",
        )),
    }
}

fn write_plant_point(writer: &mut Writer, point: &PlantPoint) -> Result<()> {
    point.validate()?;
    writer.plant_id(point.id);
    writer.cell(point.owner);
    writer.position(point.position);
    for lane in point.orientation.bits() {
        writer.i16(lane);
    }
    writer.fixed3(point.scale);
    writer.bounds(point.bounds);
    writer.uuid(point.family);
    writer.u32(point.variation);
    writer.u32(point.lifecycle as u32);
    writer.u32(point.phenotype);
    writer.u32(point.representation_class);
    writer.u128(point.deterministic_key);
    writer.u64(point.candidate);
    writer.option(point.parent, |writer, value| {
        writer.plant_id(value);
        Ok(())
    })?;
    writer.option(point.colony, |writer, value| {
        writer.plant_id(value);
        Ok(())
    })?;
    writer.u64(point.ecology_tick);
    writer.unit(point.health);
    writer.unit(point.moisture);
    writer.unit(point.fuel);
    writer.unit(point.phenology);
    writer.u32(point.flags.bits());
    writer.u32(point.interaction_policy as u32);
    writer.u32(point.provenance);
    writer.option(point.attachment, |writer, value| {
        writer.u64(value.provider.0);
        writer.u64(value.primitive.0);
        for barycentric in value.barycentric {
            writer.unit(barycentric);
        }
        writer.u64(value.revision.0);
        Ok(())
    })?;
    writer.fixed3(point.surface_projection);
    Ok(())
}

fn read_plant_point(reader: &mut Reader<'_>) -> Result<PlantPoint> {
    let point = PlantPoint {
        id: reader.plant_id()?,
        owner: reader.cell()?,
        position: reader.position()?,
        orientation: QuantizedOrientation::new([
            reader.i16()?,
            reader.i16()?,
            reader.i16()?,
            reader.i16()?,
        ])?,
        scale: reader.fixed3()?,
        bounds: reader.bounds()?,
        family: reader.uuid()?,
        variation: reader.u32()?,
        lifecycle: PlantLifecycle::try_from(reader.u32()?)?,
        phenotype: reader.u32()?,
        representation_class: reader.u32()?,
        deterministic_key: reader.u128()?,
        candidate: reader.u64()?,
        parent: reader.option(Reader::plant_id)?,
        colony: reader.option(Reader::plant_id)?,
        ecology_tick: reader.u64()?,
        health: reader.unit()?,
        moisture: reader.unit()?,
        fuel: reader.unit()?,
        phenology: reader.unit()?,
        flags: PlantFlags::from_bits(reader.u32()?)?,
        interaction_policy: InteractionPolicy::try_from(reader.u32()?)?,
        provenance: reader.u32()?,
        attachment: reader.option(|reader| {
            Ok(SurfaceAttachment::new(
                SurfaceProviderId(reader.u64()?),
                SurfacePrimitiveId(reader.u64()?),
                [reader.unit()?, reader.unit()?, reader.unit()?],
                SurfaceRevision(reader.u64()?),
            )?)
        })?,
        surface_projection: reader.fixed3()?,
    };
    point.validate()?;
    Ok(point)
}

fn source_units_tag(value: SourceUnits) -> u8 {
    match value {
        SourceUnits::Meters => 0,
        SourceUnits::Centimeters => 1,
        SourceUnits::Millimeters => 2,
        SourceUnits::Feet => 3,
    }
}

fn source_units(value: u8) -> Result<SourceUnits> {
    match value {
        0 => Ok(SourceUnits::Meters),
        1 => Ok(SourceUnits::Centimeters),
        2 => Ok(SourceUnits::Millimeters),
        3 => Ok(SourceUnits::Feet),
        _ => Err(invalid_enum(".splant", "source.units")),
    }
}

fn source_axis_tag(value: SourceAxis) -> u8 {
    match value {
        SourceAxis::PositiveX => 0,
        SourceAxis::NegativeX => 1,
        SourceAxis::PositiveY => 2,
        SourceAxis::NegativeY => 3,
        SourceAxis::PositiveZ => 4,
        SourceAxis::NegativeZ => 5,
    }
}

fn source_axis(value: u8) -> Result<SourceAxis> {
    match value {
        0 => Ok(SourceAxis::PositiveX),
        1 => Ok(SourceAxis::NegativeX),
        2 => Ok(SourceAxis::PositiveY),
        3 => Ok(SourceAxis::NegativeY),
        4 => Ok(SourceAxis::PositiveZ),
        5 => Ok(SourceAxis::NegativeZ),
        _ => Err(invalid_enum(".splant", "source.axis")),
    }
}

fn source_role_tag(value: PlantSourceRole) -> u8 {
    match value {
        PlantSourceRole::Geometry => 0,
        PlantSourceRole::Material => 1,
        PlantSourceRole::Skeleton => 2,
        PlantSourceRole::Collision => 3,
        PlantSourceRole::Navigation => 4,
    }
}

fn source_role(value: u8) -> Result<PlantSourceRole> {
    match value {
        0 => Ok(PlantSourceRole::Geometry),
        1 => Ok(PlantSourceRole::Material),
        2 => Ok(PlantSourceRole::Skeleton),
        3 => Ok(PlantSourceRole::Collision),
        4 => Ok(PlantSourceRole::Navigation),
        _ => Err(invalid_enum(".splant", "source.role")),
    }
}

fn source_handedness_tag(value: SourceHandedness) -> u8 {
    match value {
        SourceHandedness::Right => 0,
        SourceHandedness::Left => 1,
    }
}

fn source_handedness(value: u8) -> Result<SourceHandedness> {
    match value {
        0 => Ok(SourceHandedness::Right),
        1 => Ok(SourceHandedness::Left),
        _ => Err(invalid_enum(".splant", "source.settings.handedness")),
    }
}

fn source_winding_tag(value: SourceWinding) -> u8 {
    match value {
        SourceWinding::CounterClockwise => 0,
        SourceWinding::Clockwise => 1,
    }
}

fn source_winding(value: u8) -> Result<SourceWinding> {
    match value {
        0 => Ok(SourceWinding::CounterClockwise),
        1 => Ok(SourceWinding::Clockwise),
        _ => Err(invalid_enum(".splant", "source.settings.winding")),
    }
}

fn source_uv_origin_tag(value: SourceUvOrigin) -> u8 {
    match value {
        SourceUvOrigin::TopLeft => 0,
        SourceUvOrigin::BottomLeft => 1,
    }
}

fn source_uv_origin(value: u8) -> Result<SourceUvOrigin> {
    match value {
        0 => Ok(SourceUvOrigin::TopLeft),
        1 => Ok(SourceUvOrigin::BottomLeft),
        _ => Err(invalid_enum(".splant", "source.settings.uvOrigin")),
    }
}

fn tangent_policy_tag(value: PlantTangentPolicy) -> u8 {
    match value {
        PlantTangentPolicy::Require => 0,
        PlantTangentPolicy::GenerateMissing => 1,
        PlantTangentPolicy::Regenerate => 2,
    }
}

fn tangent_policy(value: u8) -> Result<PlantTangentPolicy> {
    match value {
        0 => Ok(PlantTangentPolicy::Require),
        1 => Ok(PlantTangentPolicy::GenerateMissing),
        2 => Ok(PlantTangentPolicy::Regenerate),
        _ => Err(invalid_enum(".splant", "source.settings.tangentPolicy")),
    }
}

fn plant_part_semantic_tag(value: PlantPartSemantic) -> u8 {
    match value {
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

fn plant_part_semantic(value: u8) -> Result<PlantPartSemantic> {
    match value {
        0 => Ok(PlantPartSemantic::Trunk),
        1 => Ok(PlantPartSemantic::Branch),
        2 => Ok(PlantPartSemantic::Root),
        3 => Ok(PlantPartSemantic::Frond),
        4 => Ok(PlantPartSemantic::Leaf),
        5 => Ok(PlantPartSemantic::Flower),
        6 => Ok(PlantPartSemantic::Fruit),
        7 => Ok(PlantPartSemantic::Blade),
        _ => Err(invalid_enum(".splant", "parts.semantic")),
    }
}

fn phenotype_role_tag(value: PhenotypeRole) -> u8 {
    match value {
        PhenotypeRole::Healthy => 0,
        PhenotypeRole::Harvested => 1,
        PhenotypeRole::Damaged => 2,
        PhenotypeRole::Burned => 3,
        PhenotypeRole::Dead => 4,
    }
}

fn phenotype_role(value: u8) -> Result<PhenotypeRole> {
    match value {
        0 => Ok(PhenotypeRole::Healthy),
        1 => Ok(PhenotypeRole::Harvested),
        2 => Ok(PhenotypeRole::Damaged),
        3 => Ok(PhenotypeRole::Burned),
        4 => Ok(PhenotypeRole::Dead),
        _ => Err(invalid_enum(".splant", "phenotypes.role")),
    }
}

fn collision_shape_tag(value: PlantCollisionShape) -> u8 {
    match value {
        PlantCollisionShape::Box => 0,
        PlantCollisionShape::Sphere => 1,
        PlantCollisionShape::Capsule => 2,
        PlantCollisionShape::ConvexHull => 3,
    }
}

fn collision_shape(value: u8) -> Result<PlantCollisionShape> {
    match value {
        0 => Ok(PlantCollisionShape::Box),
        1 => Ok(PlantCollisionShape::Sphere),
        2 => Ok(PlantCollisionShape::Capsule),
        3 => Ok(PlantCollisionShape::ConvexHull),
        _ => Err(invalid_enum(".splant", "collision.shape")),
    }
}

fn invalid_enum(format: &'static str, field: &str) -> Error {
    Error::InvalidFormat {
        format,
        field: field.to_owned(),
    }
}

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn with_header(magic: &[u8; 8], version: u32, schema: [u8; 32]) -> Self {
        let mut writer = Self { bytes: Vec::new() };
        writer.bytes(magic);
        writer.u32(version);
        writer.bytes(&schema);
        writer
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_be_bytes());
    }

    fn i16(&mut self, value: i16) {
        self.bytes(&value.to_be_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_be_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.bytes(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_be_bytes());
    }

    fn u128(&mut self, value: u128) {
        self.bytes(&value.to_be_bytes());
    }

    fn i128(&mut self, value: i128) {
        self.bytes(&value.to_be_bytes());
    }

    fn uuid(&mut self, value: Uuid) {
        self.u64(value.value());
    }

    fn plant_id(&mut self, value: PlantId) {
        self.bytes(&value.bytes());
    }

    fn cell(&mut self, value: WorldCellKey) {
        self.bytes(&value.canonical_bytes());
    }

    fn position(&mut self, value: WorldPosition) {
        for tick in value.global_ticks() {
            self.i128(tick);
        }
    }

    fn bounds(&mut self, value: WorldBounds) {
        for tick in value.min_ticks() {
            self.i128(tick);
        }
        for tick in value.max_ticks_exclusive() {
            self.i128(tick);
        }
    }

    fn fixed(&mut self, value: DecisionScalar) {
        self.bytes(&value.canonical_bytes());
    }

    fn fixed2(&mut self, value: [DecisionScalar; 2]) {
        for scalar in value {
            self.fixed(scalar);
        }
    }

    fn fixed3(&mut self, value: [DecisionScalar; 3]) {
        for scalar in value {
            self.fixed(scalar);
        }
    }

    fn unit(&mut self, value: UnitInterval) {
        self.u16(value.bits());
    }

    fn string(&mut self, value: &str) -> Result<()> {
        self.u32(u32::try_from(value.len()).map_err(|_| Error::NumericOverflow)?);
        self.bytes(value.as_bytes());
        Ok(())
    }

    fn value(&mut self, value: &Value) -> Result<()> {
        self.string(&dump_json_sorted(value, -1))
    }

    fn vec<T>(
        &mut self,
        values: &[T],
        mut write: impl FnMut(&mut Self, &T) -> Result<()>,
    ) -> Result<()> {
        self.u32(u32::try_from(values.len()).map_err(|_| Error::NumericOverflow)?);
        for value in values {
            write(self, value)?;
        }
        Ok(())
    }

    fn option<T>(
        &mut self,
        value: Option<T>,
        write: impl FnOnce(&mut Self, T) -> Result<()>,
    ) -> Result<()> {
        match value {
            Some(value) => {
                self.bool(true);
                write(self, value)
            }
            None => {
                self.bool(false);
                Ok(())
            }
        }
    }

    fn field_channel(&mut self, channel: FieldChannel) {
        let (tag, user) = match channel {
            FieldChannel::Altitude => (0, 0),
            FieldChannel::Slope => (1, 0),
            FieldChannel::Curvature => (2, 0),
            FieldChannel::Concavity => (3, 0),
            FieldChannel::Drainage => (4, 0),
            FieldChannel::Moisture => (5, 0),
            FieldChannel::Temperature => (6, 0),
            FieldChannel::Precipitation => (7, 0),
            FieldChannel::Sunlight => (8, 0),
            FieldChannel::Exposure => (9, 0),
            FieldChannel::WaterDistance => (10, 0),
            FieldChannel::WaterDepth => (11, 0),
            FieldChannel::SignedBlocker => (12, 0),
            FieldChannel::SplineDistance => (13, 0),
            FieldChannel::User(value) => (14, value),
        };
        self.u8(tag);
        self.u64(user);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
    format: &'static str,
}

impl<'a> Reader<'a> {
    fn with_header(
        bytes: &'a [u8],
        magic: &[u8; 8],
        expected_version: u32,
        expected_schema: [u8; 32],
        format: &'static str,
    ) -> Result<Self> {
        let mut reader = Self {
            bytes,
            cursor: 0,
            format,
        };
        if reader.take(8)? != magic {
            return Err(reader.invalid("magic"));
        }
        let found = reader.u32()?;
        if found != expected_version {
            return Err(Error::FormatVersion {
                format,
                found,
                expected: expected_version,
            });
        }
        if reader.take(32)? != expected_schema {
            return Err(reader.invalid("schemaHash"));
        }
        Ok(reader)
    }

    fn complete(&self) -> Result<()> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(self.invalid("trailingBytes"))
        }
    }

    fn invalid(&self, field: &str) -> Error {
        Error::InvalidFormat {
            format: self.format,
            field: field.to_owned(),
        }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(Error::NumericOverflow)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or_else(|| self.invalid("truncated"))?;
        self.cursor = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| self.invalid("array"))
    }

    fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.invalid("bool")),
        }
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn u128(&mut self) -> Result<u128> {
        Ok(u128::from_be_bytes(self.array()?))
    }

    fn i128(&mut self) -> Result<i128> {
        Ok(i128::from_be_bytes(self.array()?))
    }

    fn uuid(&mut self) -> Result<Uuid> {
        Ok(Uuid(self.u64()?))
    }

    fn plant_id(&mut self) -> Result<PlantId> {
        PlantId::from_canonical_bytes(self.array()?)
    }

    fn cell(&mut self) -> Result<WorldCellKey> {
        Ok(WorldCellKey::from_canonical_bytes(self.array()?)?)
    }

    fn position(&mut self) -> Result<WorldPosition> {
        Ok(WorldPosition::from_global_ticks([
            self.i128()?,
            self.i128()?,
            self.i128()?,
        ])?)
    }

    fn bounds(&mut self) -> Result<WorldBounds> {
        Ok(WorldBounds::new(
            [self.i128()?, self.i128()?, self.i128()?],
            [self.i128()?, self.i128()?, self.i128()?],
        )?)
    }

    fn fixed(&mut self) -> Result<DecisionScalar> {
        Ok(DecisionScalar::from_bits(self.i32()?))
    }

    fn fixed2(&mut self) -> Result<[DecisionScalar; 2]> {
        Ok([self.fixed()?, self.fixed()?])
    }

    fn fixed3(&mut self) -> Result<[DecisionScalar; 3]> {
        Ok([self.fixed()?, self.fixed()?, self.fixed()?])
    }

    fn unit(&mut self) -> Result<UnitInterval> {
        Ok(UnitInterval::from_bits(self.u16()?))
    }

    fn string(&mut self) -> Result<String> {
        let length = usize::try_from(self.u32()?).map_err(|_| Error::NumericOverflow)?;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| self.invalid("utf8"))
    }

    fn value(&mut self) -> Result<Value> {
        Ok(parse_json(&self.string()?)?)
    }

    fn vec<T>(&mut self, mut read: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let length = usize::try_from(self.u32()?).map_err(|_| Error::NumericOverflow)?;
        let mut values = Vec::with_capacity(length);
        for _ in 0..length {
            values.push(read(self)?);
        }
        Ok(values)
    }

    fn option<T>(&mut self, read: impl FnOnce(&mut Self) -> Result<T>) -> Result<Option<T>> {
        if self.bool()? {
            Ok(Some(read(self)?))
        } else {
            Ok(None)
        }
    }

    fn field_channel(&mut self) -> Result<FieldChannel> {
        let tag = self.u8()?;
        let user = self.u64()?;
        match (tag, user) {
            (0, 0) => Ok(FieldChannel::Altitude),
            (1, 0) => Ok(FieldChannel::Slope),
            (2, 0) => Ok(FieldChannel::Curvature),
            (3, 0) => Ok(FieldChannel::Concavity),
            (4, 0) => Ok(FieldChannel::Drainage),
            (5, 0) => Ok(FieldChannel::Moisture),
            (6, 0) => Ok(FieldChannel::Temperature),
            (7, 0) => Ok(FieldChannel::Precipitation),
            (8, 0) => Ok(FieldChannel::Sunlight),
            (9, 0) => Ok(FieldChannel::Exposure),
            (10, 0) => Ok(FieldChannel::WaterDistance),
            (11, 0) => Ok(FieldChannel::WaterDepth),
            (12, 0) => Ok(FieldChannel::SignedBlocker),
            (13, 0) => Ok(FieldChannel::SplineDistance),
            (14, value) => Ok(FieldChannel::User(value)),
            _ => Err(self.invalid("fieldChannel")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(value: i32) -> DecisionScalar {
        DecisionScalar::from_integer(value).unwrap()
    }

    fn plant() -> PlantFamilyAsset {
        PlantFamilyAsset {
            version: PLANT_ASSET_VERSION,
            id: Uuid(11),
            name: "Oak".to_owned(),
            tags: vec![PlantTagId::new(7).unwrap(), PlantTagId::new(19).unwrap()],
            source: PlantFamilySource::Native(NativeBotanicalGraph {
                schema_hash: [1; 32],
                graph: Value::Object(Default::default()),
            }),
            parts: vec![PlantPart {
                id: 12,
                parent: None,
                semantic: PlantPartSemantic::Trunk,
                material_slot: 0,
                sources: Vec::new(),
            }],
            dimensions: PlantDimensions {
                height: fixed(8),
                trunk_radius: fixed(1),
                crown_radius: [fixed(3); 2],
                root_radius: [fixed(4); 2],
                local_bounds_min: [fixed(-4), fixed(0), fixed(-4)],
                local_bounds_max: [fixed(4), fixed(8), fixed(4)],
            },
            material_slots: vec![Uuid(13)],
            spines: Vec::new(),
            mechanics: MechanicalResponse {
                stiffness: fixed(2),
                damping: UnitInterval::from_bits(1),
                drag: fixed(1),
                flutter: DecisionScalar::from_bits(1),
                bend_limit: UnitInterval::from_bits(2),
                damage_threshold: fixed(3),
                break_threshold: fixed(4),
            },
            variations: vec![PlantVariation {
                id: 0,
                name: "Default".to_owned(),
                sources: Vec::new(),
                active_parts: Vec::new(),
            }],
            phenotypes: vec![PlantPhenotype {
                id: 0,
                role: PhenotypeRole::Healthy,
                variation: 0,
                material_remap: Vec::new(),
                active_parts: Vec::new(),
            }],
            collision_proxies: Vec::new(),
            navigation_proxies: Vec::new(),
            interaction_policy: InteractionPolicy::Structural,
            habitat: Some(HabitatPreferences {
                fields: vec![(FieldChannel::Moisture, fixed(0), fixed(1))],
                surface_tags: vec![14],
                shade_tolerance: UnitInterval::from_bits(32_768),
            }),
        }
    }

    #[test]
    fn plant_asset_round_trips_canonical_bytes() {
        let asset = plant();
        let bytes = write_plant_asset(&asset).unwrap();
        let decoded = read_plant_asset(&bytes).unwrap();
        assert_eq!(decoded, asset);
        assert_eq!(write_plant_asset(&decoded).unwrap(), bytes);
    }

    #[test]
    fn plant_asset_rejects_noncanonical_family_tags() {
        let mut asset = plant();
        asset.tags.swap(0, 1);
        assert!(matches!(
            write_plant_asset(&asset),
            Err(Error::InvalidFormat { field, .. }) if field == "tags"
        ));

        asset.tags = vec![PlantTagId::new(7).unwrap(); 2];
        assert!(matches!(
            write_plant_asset(&asset),
            Err(Error::InvalidFormat { field, .. }) if field == "tags"
        ));
    }

    #[test]
    fn biome_asset_round_trips_canonical_bytes() {
        let asset = BiomeAsset {
            version: BIOME_ASSET_VERSION,
            id: Uuid(21),
            name: "Temperate forest".to_owned(),
            role: BiomeRole::Root,
            parameters: vec![BiomeParameter {
                id: 22,
                name: "canopy".to_owned(),
                parameter_type: BiomeParameterType::Unit,
                default_value: Value::from(1),
            }],
            palette: vec![BiomePaletteEntry {
                plant: Uuid(11),
                weight: UnitInterval::ONE,
                seed_namespace: 23,
            }],
            density: fixed(1),
            clustering: UnitInterval::from_bits(24),
            suitability: Vec::new(),
            competition: Vec::new(),
            companions: Vec::new(),
            succession: Vec::new(),
            seed_namespaces: vec![("canopy".to_owned(), 23)],
            modules: Vec::new(),
            policy: BiomeGraphPolicy {
                maximum_recursion: 8,
                maximum_influence_radius: fixed(64),
                require_authoritative_fields: true,
            },
            graph: Value::Object(Default::default()),
        };
        let bytes = write_biome_asset(&asset).unwrap();
        let decoded = read_biome_asset(&bytes).unwrap();
        assert_eq!(decoded, asset);
        assert_eq!(write_biome_asset(&decoded).unwrap(), bytes);
    }

    #[test]
    fn map_root_and_typed_objects_round_trip_canonical_bytes() {
        let bounds = WorldBounds::new([0; 3], [1024; 3]).unwrap();
        let layer = VegetationLayer {
            id: 32,
            name: "Density".to_owned(),
            coordinate_space: LayerCoordinateSpace::World,
            bounds,
            operator: VegetationLayerOperator::Density(FieldTileLayer {
                channel: FieldChannel::Moisture,
                tile_set: 33,
                blend: FieldBlendOperator::Multiply,
                weight: UnitInterval::ONE,
            }),
            dependencies: Vec::new(),
            order: 0,
            locked: false,
            muted: false,
            revision: 1,
        };
        let layer_chunk = VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map: Uuid(31),
            key: VegetationMapChunkKey {
                layer: layer.id,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::LayerMetadata,
            },
            revision: layer.revision,
            payload: VegetationMapChunkPayload::LayerMetadata(layer),
        };
        let field_chunk = VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map: Uuid(31),
            key: VegetationMapChunkKey {
                layer: 32,
                tile: VegetationMapTileKey::Cell(WorldCellKey::base(0, 0, 0)),
                kind: VegetationMapChunkKind::Field,
            },
            revision: 2,
            payload: VegetationMapChunkPayload::Field(VegetationMapFieldChunk {
                fields: vec![AuthoredFieldTile {
                    channel: FieldChannel::Moisture,
                    layer: 32,
                    dimensions: [2, 1, 1],
                    quantum_bits: 1,
                    values: vec![4, 5],
                }],
                blockers: Vec::new(),
            }),
        };
        for chunk in [&layer_chunk, &field_chunk] {
            let bytes = write_vegetation_map_chunk(chunk).unwrap();
            let decoded = read_vegetation_map_chunk(&bytes).unwrap();
            assert_eq!(&decoded, chunk);
            assert_eq!(write_vegetation_map_chunk(&decoded).unwrap(), bytes);
        }
        let mut inventory = vec![
            field_chunk.reference().unwrap(),
            layer_chunk.reference().unwrap(),
        ];
        inventory.sort_by_key(VegetationMapChunkReference::order_key);
        let map = VegetationMapAsset {
            version: VEGETATION_MAP_VERSION,
            id: Uuid(31),
            name: "World vegetation".to_owned(),
            bounds,
            chunk_layout: VegetationMapChunkLayout {
                level: 0,
                schema_hash: vegetation_map_chunk_schema_hash(),
            },
            generation: 1,
            inventory,
        };
        let map_bytes = write_vegetation_map_asset(&map).unwrap();
        assert_eq!(read_vegetation_map_asset(&map_bytes).unwrap(), map);
        assert_eq!(write_vegetation_map_asset(&map).unwrap(), map_bytes);
    }

    #[test]
    fn map_codecs_reject_truncation_corruption_and_old_versions() {
        let root = VegetationMapAsset {
            version: VEGETATION_MAP_VERSION,
            id: Uuid(31),
            name: "World vegetation".to_owned(),
            bounds: WorldBounds::new([0; 3], [1024; 3]).unwrap(),
            chunk_layout: VegetationMapChunkLayout {
                level: 0,
                schema_hash: vegetation_map_chunk_schema_hash(),
            },
            generation: 0,
            inventory: Vec::new(),
        };
        let root_bytes = write_vegetation_map_asset(&root).unwrap();
        assert!(read_vegetation_map_asset(&root_bytes[..root_bytes.len() - 1]).is_err());
        let mut old_root = root_bytes;
        old_root[11] = (VEGETATION_MAP_VERSION - 1) as u8;
        assert!(matches!(
            read_vegetation_map_asset(&old_root),
            Err(Error::FormatVersion { .. })
        ));

        let chunk = VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map: Uuid(31),
            key: VegetationMapChunkKey {
                layer: 32,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::EditorMetadata,
            },
            revision: 1,
            payload: VegetationMapChunkPayload::EditorMetadata(vec![BrushGestureMetadata {
                gesture: 1,
                layer: 32,
                samples: Vec::new(),
            }]),
        };
        let bytes = write_vegetation_map_chunk(&chunk).unwrap();
        assert!(read_vegetation_map_chunk(&bytes[..bytes.len() - 1]).is_err());
        let mut corrupt_schema = bytes.clone();
        corrupt_schema[12] ^= 1;
        assert!(read_vegetation_map_chunk(&corrupt_schema).is_err());
        let mut old_version = bytes;
        old_version[11] = (VEGETATION_MAP_CHUNK_VERSION - 1) as u8;
        assert!(matches!(
            read_vegetation_map_chunk(&old_version),
            Err(Error::FormatVersion { .. })
        ));
    }

    #[test]
    fn codecs_reject_trailing_bytes_and_schema_changes() {
        let mut bytes = write_plant_asset(&plant()).unwrap();
        bytes.push(0);
        assert!(read_plant_asset(&bytes).is_err());
        let mut bytes = write_plant_asset(&plant()).unwrap();
        bytes[12] ^= 1;
        assert!(read_plant_asset(&bytes).is_err());
        let mut old_version = write_plant_asset(&plant()).unwrap();
        old_version[11] = (PLANT_ASSET_VERSION - 1) as u8;
        assert!(matches!(
            read_plant_asset(&old_version),
            Err(Error::FormatVersion { .. })
        ));
    }
}
