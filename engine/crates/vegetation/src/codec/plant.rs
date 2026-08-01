//! The `.splant` plant-family document.

use super::enums::*;
use super::stream::{Reader, Writer};
use crate::hash::sha256;
use crate::*;

const PLANT_MAGIC: &[u8; 8] = b"SPLANT01";

/// SHA-256 identity of the `.splant` binary field vocabulary.
#[must_use]
pub fn plant_asset_schema_hash() -> [u8; 32] {
    sha256(b"saffron-anima/splant/schema/v6/typed-locators+per-source-provenance+role+selector+coordinate-policy+semantic-targets+family-tags+parts+dimensions+materials+spines+mechanics+variations+phenotype-response+collision+navigation+interaction+habitat+ecology+botanical+family-role+modules")
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
        write_phenotype_response(writer, phenotype.response)?;
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
    write_plant_ecology(&mut writer, &asset.ecology)?;
    writer.u32(match asset.role {
        crate::PlantFamilyRole::Family => 0,
        crate::PlantFamilyRole::Module => 1,
    });
    writer.u16(asset.module_recursion_limit);
    writer.vec(&asset.modules, |writer, module| {
        writer.uuid(module.plant);
        writer.u128(module.call_guid);
        writer.u32(module.variation);
        writer.fixed(module.scale);
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
                response: read_phenotype_response(reader)?,
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
        ecology: read_plant_ecology(&mut reader)?,
        role: match reader.u32()? {
            0 => crate::PlantFamilyRole::Family,
            1 => crate::PlantFamilyRole::Module,
            _ => return Err(reader.invalid("role")),
        },
        module_recursion_limit: reader.u16()?,
        modules: reader.vec(|reader| {
            Ok(crate::PlantModuleReference {
                plant: reader.uuid()?,
                call_guid: reader.u128()?,
                variation: reader.u32()?,
                scale: reader.fixed()?,
            })
        })?,
    };
    reader.complete()?;
    validate_plant_family(&asset)?;
    Ok(asset)
}

fn write_plant_ecology(
    writer: &mut Writer,
    ecology: &crate::PlantEcologyDeclaration,
) -> Result<()> {
    for tick in ecology.rules.stage_ticks {
        writer.u64(tick);
    }
    writer.unit(ecology.rules.shade_tolerance);
    writer.unit(ecology.rules.drought_tolerance);
    writer.unit(ecology.rules.propagation_chance);
    writer.u32(ecology.rules.spread_radius_m);
    writer.unit(ecology.rules.regrowth_chance);
    writer.unit(ecology.rules.deadfall_chance);
    writer.unit(ecology.rules.root_demand);
    writer.vec(&ecology.relations, |writer, relation| {
        writer.uuid(relation.family);
        writer.u32(relation.kind as u32);
        writer.unit(relation.strength);
        Ok(())
    })
}

fn read_plant_ecology(reader: &mut Reader<'_>) -> Result<crate::PlantEcologyDeclaration> {
    Ok(crate::PlantEcologyDeclaration {
        rules: crate::EcologySpeciesRules {
            stage_ticks: [reader.u64()?, reader.u64()?, reader.u64()?, reader.u64()?],
            shade_tolerance: reader.unit()?,
            drought_tolerance: reader.unit()?,
            propagation_chance: reader.unit()?,
            spread_radius_m: reader.u32()?,
            regrowth_chance: reader.unit()?,
            deadfall_chance: reader.unit()?,
            root_demand: reader.unit()?,
        },
        relations: reader.vec(|reader| {
            Ok(crate::PlantSpeciesRelation {
                family: reader.uuid()?,
                kind: crate::PlantRelationKind::try_from(reader.u32()?)?,
                strength: reader.unit()?,
            })
        })?,
    })
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
        PlantFamilySource::Native { graph, grafts } => {
            writer.u8(1);
            write_botanical_graph(writer, graph)?;
            writer.vec(grafts, |writer, source| {
                writer.u128(source.id);
                write_source_locator(writer, &source.locator)?;
                writer.u8(source_role_tag(source.role));
                write_source_selector(writer, &source.selector)?;
                writer.bytes(&source.content_hash);
                write_import_settings(writer, &source.settings)?;
                write_source_provenance(writer, &source.provenance)?;
                Ok(())
            })
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
        1 => Ok(PlantFamilySource::Native {
            graph: read_botanical_graph(reader)?,
            grafts: reader.vec(|reader| {
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
        }),
        _ => Err(reader.invalid("source")),
    }
}

fn write_botanical_graph(writer: &mut Writer, graph: &crate::BotanicalGraphDocument) -> Result<()> {
    writer.vec(&graph.variations, |writer, variation| {
        writer.u128(variation.seed);
        writer.unit(variation.age);
        writer.string(&variation.name)
    })?;
    writer.vec(&graph.nodes, |writer, node| {
        writer.u128(node.guid);
        writer.u32(node.version);
        writer.u32(node.semantic_revision);
        write_botanical_operator(writer, &node.operator)
    })?;
    writer.vec(&graph.edges, |writer, edge| {
        writer.u128(edge.from_node);
        writer.string(&edge.from_pin)?;
        writer.u128(edge.to_node);
        writer.string(&edge.to_pin)
    })?;
    writer.vec(&graph.edits, |writer, edit| {
        writer.u128(edit.target.value());
        match edit.action {
            crate::BotanicalEditAction::Transform {
                offset,
                roll,
                scale,
            } => {
                writer.u8(0);
                for component in offset {
                    writer.fixed(component);
                }
                writer.unit(roll);
                writer.fixed(scale);
            }
            crate::BotanicalEditAction::Trim { at } => {
                writer.u8(1);
                writer.unit(at);
            }
            crate::BotanicalEditAction::Remove => writer.u8(2),
            crate::BotanicalEditAction::Graft {
                ref source,
                ref selector,
            } => {
                writer.u8(3);
                writer.u128(*source);
                write_source_selector(writer, selector)?;
            }
        }
        Ok(())
    })
}

fn write_botanical_operator(
    writer: &mut Writer,
    operator: &crate::BotanicalOperator,
) -> Result<()> {
    use crate::BotanicalOperator as Op;
    match operator {
        Op::Trunk {
            element,
            length,
            base_radius,
            taper,
            segments,
        } => {
            writer.u8(0);
            writer.u32(element.tag());
            writer.fixed(*length);
            writer.fixed(*base_radius);
            writer.vec(taper.points(), |writer, (at, value)| {
                writer.unit(*at);
                writer.fixed(*value);
                Ok(())
            })?;
            writer.u32(*segments);
        }
        Op::Branch {
            element,
            length_ratio,
            radius_ratio,
            declination,
            jitter,
            segments,
        } => {
            writer.u8(1);
            writer.u32(element.tag());
            for value in [length_ratio, radius_ratio, declination, jitter] {
                writer.unit(*value);
            }
            writer.u32(*segments);
        }
        Op::Phyllotaxis {
            pattern,
            count,
            nodes,
            start,
            end,
            divergence,
        } => {
            writer.u8(2);
            writer.u32(pattern.tag());
            writer.u32(*count);
            writer.u32(*nodes);
            for value in [start, end, divergence] {
                writer.unit(*value);
            }
        }
        Op::Tropism {
            kind,
            strength,
            stimulus,
            plane_offset,
        } => {
            writer.u8(3);
            writer.u32(kind.tag());
            writer.unit(*strength);
            writer.fixed3(*stimulus);
            writer.fixed(*plane_offset);
        }
        Op::Prune {
            rule,
            threshold,
            count,
        } => {
            writer.u8(4);
            writer.u32(rule.tag());
            writer.fixed(*threshold);
            writer.u32(*count);
        }
        Op::Roots {
            depth_ratio,
            spread_ratio,
            count,
        } => {
            writer.u8(5);
            writer.unit(*depth_ratio);
            writer.unit(*spread_ratio);
            writer.u32(*count);
        }
        Op::Shell {
            material_slot,
            sides,
        } => {
            writer.u8(6);
            writer.u32(*material_slot);
            writer.u32(*sides);
        }
        Op::Instance {
            element,
            material_slot,
            size,
            jitter,
        } => {
            writer.u8(7);
            writer.u32(element.tag());
            writer.u32(*material_slot);
            writer.fixed(*size);
            writer.unit(*jitter);
        }
        Op::ModuleCall { call_guid } => {
            writer.u8(10);
            writer.u128(*call_guid);
        }
        Op::Family => writer.u8(8),
        Op::Drawn { element, points } => {
            writer.u8(9);
            writer.u32(element.tag());
            writer.vec(points, |writer, point| {
                for component in point.position {
                    writer.fixed(component);
                }
                writer.fixed(point.radius);
                Ok(())
            })?;
        }
    }
    Ok(())
}

fn read_botanical_graph(reader: &mut Reader<'_>) -> Result<crate::BotanicalGraphDocument> {
    let graph = crate::BotanicalGraphDocument {
        variations: reader.vec(|reader| {
            Ok(crate::BotanicalVariation {
                seed: reader.u128()?,
                age: reader.unit()?,
                name: reader.string()?,
            })
        })?,
        nodes: reader.vec(|reader| {
            Ok(crate::BotanicalNode {
                guid: reader.u128()?,
                version: reader.u32()?,
                semantic_revision: reader.u32()?,
                operator: read_botanical_operator(reader)?,
            })
        })?,
        edges: reader.vec(|reader| {
            Ok(crate::BotanicalEdge {
                from_node: reader.u128()?,
                from_pin: reader.string()?,
                to_node: reader.u128()?,
                to_pin: reader.string()?,
            })
        })?,
        edits: reader.vec(|reader| {
            Ok(crate::BotanicalManualEdit {
                target: crate::BotanicalElementId::from_value(reader.u128()?),
                action: match reader.u8()? {
                    0 => crate::BotanicalEditAction::Transform {
                        offset: [reader.fixed()?, reader.fixed()?, reader.fixed()?],
                        roll: reader.unit()?,
                        scale: reader.fixed()?,
                    },
                    1 => crate::BotanicalEditAction::Trim { at: reader.unit()? },
                    2 => crate::BotanicalEditAction::Remove,
                    3 => crate::BotanicalEditAction::Graft {
                        source: reader.u128()?,
                        selector: read_source_selector(reader)?,
                    },
                    _ => return Err(reader.invalid("edits.action")),
                },
            })
        })?,
    };
    graph.validate()?;
    Ok(graph)
}

fn read_botanical_operator(reader: &mut Reader<'_>) -> Result<crate::BotanicalOperator> {
    use crate::BotanicalOperator as Op;
    Ok(match reader.u8()? {
        0 => Op::Trunk {
            element: crate::BotanicalElement::try_from(reader.u32()?)?,
            length: reader.fixed()?,
            base_radius: reader.fixed()?,
            taper: saffron_spatial::DecisionCurve::new(
                reader.vec(|reader| Ok((reader.unit()?, reader.fixed()?)))?,
            )?,
            segments: reader.u32()?,
        },
        1 => Op::Branch {
            element: crate::BotanicalElement::try_from(reader.u32()?)?,
            length_ratio: reader.unit()?,
            radius_ratio: reader.unit()?,
            declination: reader.unit()?,
            jitter: reader.unit()?,
            segments: reader.u32()?,
        },
        2 => Op::Phyllotaxis {
            pattern: crate::PhyllotaxisPattern::try_from(reader.u32()?)?,
            count: reader.u32()?,
            nodes: reader.u32()?,
            start: reader.unit()?,
            end: reader.unit()?,
            divergence: reader.unit()?,
        },
        3 => Op::Tropism {
            kind: crate::TropismKind::try_from(reader.u32()?)?,
            strength: reader.unit()?,
            stimulus: reader.fixed3()?,
            plane_offset: reader.fixed()?,
        },
        4 => Op::Prune {
            rule: crate::PruneRule::try_from(reader.u32()?)?,
            threshold: reader.fixed()?,
            count: reader.u32()?,
        },
        5 => Op::Roots {
            depth_ratio: reader.unit()?,
            spread_ratio: reader.unit()?,
            count: reader.u32()?,
        },
        6 => Op::Shell {
            material_slot: reader.u32()?,
            sides: reader.u32()?,
        },
        7 => Op::Instance {
            element: crate::BotanicalElement::try_from(reader.u32()?)?,
            material_slot: reader.u32()?,
            size: reader.fixed()?,
            jitter: reader.unit()?,
        },
        8 => Op::Family,
        10 => Op::ModuleCall {
            call_guid: reader.u128()?,
        },
        9 => Op::Drawn {
            element: crate::BotanicalElement::try_from(reader.u32()?)?,
            points: reader.vec(|reader| {
                Ok(crate::BotanicalDrawnPoint {
                    position: [reader.fixed()?, reader.fixed()?, reader.fixed()?],
                    radius: reader.fixed()?,
                })
            })?,
        },
        _ => return Err(reader.invalid("source.native.operator")),
    })
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

fn write_phenotype_response(writer: &mut Writer, value: PhenotypeResponse) -> Result<()> {
    writer.option(value.season_window, |writer, (start, end)| {
        writer.u16(start);
        writer.u16(end);
        Ok(())
    })?;
    writer.option(value.health_band, |writer, (low, high)| {
        writer.unit(low);
        writer.unit(high);
        Ok(())
    })?;
    writer.option(value.moisture_band, |writer, (low, high)| {
        writer.unit(low);
        writer.unit(high);
        Ok(())
    })?;
    writer.u16(value.ramp_mille);
    Ok(())
}

fn read_phenotype_response(reader: &mut Reader<'_>) -> Result<PhenotypeResponse> {
    Ok(PhenotypeResponse {
        season_window: reader.option(|reader| Ok((reader.u16()?, reader.u16()?)))?,
        health_band: reader.option(|reader| Ok((reader.unit()?, reader.unit()?)))?,
        moisture_band: reader.option(|reader| Ok((reader.unit()?, reader.unit()?)))?,
        ramp_mille: reader.u16()?,
    })
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

#[cfg(test)]
mod tests {
    use saffron_core::Uuid;
    use saffron_spatial::{DecisionScalar, FieldChannel, UnitInterval};

    use super::*;
    use crate::codec::fixed;

    fn plant() -> PlantFamilyAsset {
        PlantFamilyAsset {
            role: crate::PlantFamilyRole::Family,
            modules: Vec::new(),
            module_recursion_limit: crate::MAX_PLANT_MODULE_RECURSION,
            version: PLANT_ASSET_VERSION,
            id: Uuid(11),
            name: "Oak".to_owned(),
            tags: vec![PlantTagId::new(7).unwrap(), PlantTagId::new(19).unwrap()],
            source: PlantFamilySource::Native {
                graph: BotanicalGraphDocument::sapling(0x5a11),
                grafts: Vec::new(),
            },
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
                sources: vec![crate::native_variation_source_id(0)],
                active_parts: Vec::new(),
            }],
            phenotypes: vec![PlantPhenotype {
                id: 0,
                role: PhenotypeRole::Healthy,
                response: PhenotypeResponse::default(),
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
            ecology: crate::PlantEcologyDeclaration::default(),
        }
    }

    /// What a consumer of a family keys on: everything the family declares, minus the observation
    /// a cook refreshes. A cook accepts the hash it read and publishes a generation in the same
    /// operation, so a consumer keyed on the observation cannot be reproduced by recooking the
    /// authored state that cook left behind.
    #[test]
    fn declared_identity_covers_authoring_and_not_the_observed_source_hash() {
        let mut asset = plant();
        let PlantFamilySource::Native { grafts, .. } = &mut asset.source else {
            panic!("the fixture family is native");
        };
        grafts.push(PlantSourceReference {
            id: 77,
            locator: PlantSourceLocator::Asset(Uuid(9_001)),
            role: PlantSourceRole::Geometry,
            selector: PlantSourceSelector::Element {
                id: 9_001,
                path: "hero".to_owned(),
            },
            content_hash: [0xAB; 32],
            settings: PlantImportSettings::default(),
            provenance: crate::SourceProvenance::default(),
        });
        let declared = asset.declared_identity().unwrap();

        let mut observed = asset.clone();
        let PlantFamilySource::Native { grafts, .. } = &mut observed.source else {
            panic!("the fixture family is native");
        };
        grafts[0].content_hash = [0x17; 32];
        assert_eq!(observed.declared_identity().unwrap(), declared);
        assert_ne!(
            write_plant_asset(&observed).unwrap(),
            write_plant_asset(&asset).unwrap(),
            "the document itself did move, which is what the identity must ignore"
        );

        // Everything else about the source is authoring, and re-keys.
        let mut retargeted = asset.clone();
        let PlantFamilySource::Native { grafts, .. } = &mut retargeted.source else {
            panic!("the fixture family is native");
        };
        grafts[0].locator = PlantSourceLocator::Asset(Uuid(9_002));
        assert_ne!(retargeted.declared_identity().unwrap(), declared);

        let mut renamed = asset.clone();
        renamed.name = "Elm".to_owned();
        assert_ne!(renamed.declared_identity().unwrap(), declared);

        // An imported family declares its sources the same way, and every declaration the identity
        // stands in for has to be one the canonical encoder accepts.
        let PlantFamilySource::Native { grafts, .. } = &asset.source else {
            panic!("the fixture family is native");
        };
        let mut sources = grafts.clone();
        sources[0].provenance = crate::SourceProvenance {
            source: "test".to_owned(),
            source_uri: "file://hero".to_owned(),
            license_id: "CC0-1.0".to_owned(),
            license_uri: "https://creativecommons.org/publicdomain/zero/1.0/".to_owned(),
            ..crate::SourceProvenance::default()
        };
        let mut imported = asset.clone();
        imported.source = PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
            semantic_targets: vec![PlantManualSemanticTarget {
                id: 1,
                source: 77,
                selector: sources[0].selector.clone(),
                destination: PlantSemanticDestination::Part(12),
            }],
            sources,
        });
        imported.variations[0].sources = vec![77];
        imported.parts[0].sources = vec![77];
        imported.declared_identity().unwrap();
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
    fn a_module_table_round_trips_and_is_matched_against_the_graph() {
        let mut native = plant();
        let call_guid = 0x5eed_u128;
        let mut graph = crate::BotanicalGraphDocument::sapling(0x1234);
        let leaves = graph
            .nodes
            .iter()
            .position(|node| matches!(node.operator, crate::BotanicalOperator::Instance { .. }))
            .expect("the sapling places leaves");
        graph.nodes[leaves].operator = crate::BotanicalOperator::ModuleCall { call_guid };
        native.source = crate::PlantFamilySource::Native {
            graph,
            grafts: Vec::new(),
        };
        native.modules = vec![crate::PlantModuleReference {
            plant: saffron_core::Uuid(0x9_0001),
            call_guid,
            variation: 0,
            scale: fixed(2),
        }];
        native.module_recursion_limit = 4;
        let bytes = write_plant_asset(&native).unwrap();
        let decoded = read_plant_asset(&bytes).unwrap();
        assert_eq!(decoded, native);
        assert_eq!(write_plant_asset(&decoded).unwrap(), bytes);

        let mut orphaned = native.clone();
        orphaned.modules[0].call_guid = call_guid + 1;
        assert!(write_plant_asset(&orphaned).is_err());
        let mut unbound = native.clone();
        unbound.modules.clear();
        assert!(write_plant_asset(&unbound).is_err());
        let mut self_call = native.clone();
        self_call.modules[0].plant = self_call.id;
        assert!(write_plant_asset(&self_call).is_err());
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
