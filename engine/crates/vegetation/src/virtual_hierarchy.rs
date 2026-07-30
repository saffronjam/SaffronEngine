//! Plant-family adapters for the format-neutral portable geometry hierarchy.

use std::collections::BTreeMap;

use saffron_geometry::{
    MicroInstance, PortableAggregationMode, PortableBounds, PortableDeformationKind,
    PortableDeformationRegion, PortableHierarchyInput, PortableSourceMesh, PortableSourceSkin,
    PortableSourceSubmesh, PortableSourceVertex, PortableUseCombination, VirtualHierarchyMaterial,
    aggregate_virtual_hierarchy_materials,
};

use crate::{
    AlphaClassification, Error, MaterialSurface, NormalizedPlantFamily, NormalizedPlantMesh,
    PlantFamilyAsset, PlantPartSemantic, PlantSourceRole, PlantSourceSelector, Result,
};

/// Adapts one resolved plant material to the canonical hierarchy material contract.
#[must_use]
pub fn plant_hierarchy_material(
    slot: u32,
    surface: &MaterialSurface,
    alpha: AlphaClassification,
) -> VirtualHierarchyMaterial {
    VirtualHierarchyMaterial::from_surface(slot, surface, alpha)
}

/// Adapts the normalized plant-family vocabulary to the sole portable hierarchy cooker input.
pub fn plant_hierarchy_input(
    asset: &PlantFamilyAsset,
    family: &NormalizedPlantFamily,
    materials: &[VirtualHierarchyMaterial],
) -> Result<PortableHierarchyInput> {
    let materials_by_slot = materials
        .iter()
        .map(|material| (material.slot, *material))
        .collect::<BTreeMap<_, _>>();
    let family_bounds = PortableBounds {
        min_bits: family.dimensions.local_bounds_min.map(|value| value.bits()),
        max_bits: family.dimensions.local_bounds_max.map(|value| value.bits()),
    };
    let padding = deformation_padding(asset);
    let mut meshes = Vec::new();
    let mut micro_instances = Vec::new();

    for mesh in family
        .meshes
        .iter()
        .filter(|mesh| mesh.role == PlantSourceRole::Geometry)
    {
        let prototype = u32::try_from(meshes.len()).map_err(|_| Error::NumericOverflow)?;
        meshes.push(PortableSourceMesh {
            source: mesh.source,
            selector_hash: plant_prototype_selector_hash(mesh.source, &mesh.selector)?,
            vertices: mesh
                .vertices
                .iter()
                .map(|vertex| PortableSourceVertex {
                    position_bits: vertex.position_bits,
                    normal_snorm: vertex.normal_snorm,
                    uv_bits: vertex.uv_bits,
                    tangent_snorm: vertex.tangent_snorm,
                })
                .collect(),
            indices: mesh.indices.clone(),
            submeshes: mesh
                .submeshes
                .iter()
                .map(|submesh| PortableSourceSubmesh {
                    first_index: submesh.first_index,
                    index_count: submesh.index_count,
                    material: materials_by_slot
                        .get(&submesh.material_slot)
                        .copied()
                        .unwrap_or_else(|| VirtualHierarchyMaterial::opaque(submesh.material_slot)),
                })
                .collect(),
            skin: mesh
                .skin
                .iter()
                .map(|skin| PortableSourceSkin {
                    joints: skin.joints,
                    weights: skin.weights,
                })
                .collect(),
            aggregation: if disconnected_foliage(semantic_for_mesh(asset, mesh)) {
                PortableAggregationMode::Disconnected
            } else {
                PortableAggregationMode::Contiguous
            },
        });
        add_micro_instances(asset, mesh, prototype, &mut micro_instances);
    }

    let combinations = use_combinations(asset, family, &micro_instances);
    Ok(PortableHierarchyInput {
        meshes,
        micro_instances,
        combinations,
        deformation: deformation_regions(asset, family_bounds, padding),
        bounds: family_bounds,
        root_material: aggregate_virtual_hierarchy_materials(materials),
        deformation_padding: padding,
    })
}

/// One active-use mask per (variation, phenotype): a use draws when its part is active in both
/// the phenotype and its variation and its prototype's source contributes to the variation.
/// Empty authored active sets mean "all".
fn use_combinations(
    asset: &PlantFamilyAsset,
    family: &NormalizedPlantFamily,
    micro_instances: &[MicroInstance],
) -> Vec<PortableUseCombination> {
    let sources: Vec<u128> = family
        .meshes
        .iter()
        .filter(|mesh| mesh.role == PlantSourceRole::Geometry)
        .map(|mesh| mesh.source)
        .collect();
    let mask_words = micro_instances.len().div_ceil(32);
    let mut combinations = Vec::with_capacity(asset.phenotypes.len());
    for phenotype in &asset.phenotypes {
        let variation = asset
            .variations
            .iter()
            .find(|variation| variation.id == phenotype.variation);
        let mut active_words = vec![0_u32; mask_words];
        for (index, instance) in micro_instances.iter().enumerate() {
            let source = sources
                .get(instance.prototype as usize)
                .copied()
                .unwrap_or_default();
            let variation_active = variation.is_none_or(|variation| {
                (variation.sources.is_empty() || variation.sources.contains(&source))
                    && (variation.active_parts.is_empty()
                        || variation.active_parts.contains(&instance.part))
            });
            let phenotype_active = phenotype.active_parts.is_empty()
                || phenotype.active_parts.contains(&instance.part);
            if variation_active && phenotype_active {
                active_words[index / 32] |= 1 << (index % 32);
            }
        }
        combinations.push(PortableUseCombination {
            variation: phenotype.variation,
            phenotype: phenotype.id,
            active_words,
        });
    }
    combinations
}

/// The part an exact Part-destination semantic target binds this normalized row to.
fn target_part_for_mesh(asset: &PlantFamilyAsset, mesh: &NormalizedPlantMesh) -> Option<u128> {
    let crate::PlantFamilySource::Imported(recipe) = &asset.source else {
        return None;
    };
    recipe
        .semantic_targets
        .iter()
        .find_map(|target| match target.destination {
            crate::PlantSemanticDestination::Part(part)
                if target.source == mesh.source && target.selector == mesh.selector =>
            {
                Some(part)
            }
            _ => None,
        })
}

fn semantic_for_mesh(asset: &PlantFamilyAsset, mesh: &NormalizedPlantMesh) -> PlantPartSemantic {
    let part = match target_part_for_mesh(asset, mesh) {
        Some(id) => asset.parts.iter().find(|part| part.id == id),
        None => asset
            .parts
            .iter()
            .find(|part| part.sources.contains(&mesh.source)),
    };
    part.map_or(PlantPartSemantic::Trunk, |part| part.semantic)
}

fn disconnected_foliage(semantic: PlantPartSemantic) -> bool {
    matches!(
        semantic,
        PlantPartSemantic::Frond
            | PlantPartSemantic::Leaf
            | PlantPartSemantic::Flower
            | PlantPartSemantic::Fruit
            | PlantPartSemantic::Blade
    )
}

/// The canonical prototype identity binding one normalized source mesh to its hierarchy
/// prototype, checked against `GeometryPrototype::selector_hash` when geometry decodes.
pub fn plant_prototype_selector_hash(
    source: u128,
    selector: &PlantSourceSelector,
) -> Result<[u8; 32]> {
    let mut bytes = b"saffron-anima/virtual-prototype/v1\0".to_vec();
    bytes.extend_from_slice(&source.to_be_bytes());
    match selector {
        PlantSourceSelector::Whole => bytes.push(0),
        PlantSourceSelector::Element { id, path } => {
            bytes.push(1);
            bytes.extend_from_slice(&id.to_be_bytes());
            bytes.extend_from_slice(
                &u64::try_from(path.len())
                    .map_err(|_| Error::NumericOverflow)?
                    .to_be_bytes(),
            );
            bytes.extend_from_slice(path.as_bytes());
        }
        PlantSourceSelector::Submesh { element, index } => {
            bytes.push(2);
            bytes.extend_from_slice(&element.to_be_bytes());
            bytes.extend_from_slice(&index.to_be_bytes());
        }
    }
    Ok(crate::vegetation_content_hash(&bytes))
}

fn add_micro_instances(
    asset: &PlantFamilyAsset,
    mesh: &NormalizedPlantMesh,
    prototype: u32,
    output: &mut Vec<MicroInstance>,
) {
    let identity = [
        65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536,
    ];
    // A row an exact semantic target binds belongs to that part alone: the compile
    // splits a multi-part source into per-part rows, and a use placing more than its
    // own part's geometry draws coincident duplicates no phenotype mask can hide.
    if let Some(part) = target_part_for_mesh(asset, mesh) {
        output.push(MicroInstance {
            part,
            prototype,
            transform_bits: identity,
        });
        return;
    }
    for part in asset
        .parts
        .iter()
        .filter(|part| part.sources.contains(&mesh.source))
    {
        output.push(MicroInstance {
            part: part.id,
            prototype,
            transform_bits: identity,
        });
    }
    if !output
        .iter()
        .any(|instance| instance.prototype == prototype)
    {
        output.push(MicroInstance {
            part: mesh.source,
            prototype,
            transform_bits: identity,
        });
    }
}

fn deformation_padding(asset: &PlantFamilyAsset) -> i32 {
    let extent = (0..3)
        .map(|axis| {
            i64::from(asset.dimensions.local_bounds_max[axis].bits())
                .saturating_sub(i64::from(asset.dimensions.local_bounds_min[axis].bits()))
                .unsigned_abs()
        })
        .max()
        .unwrap_or_default();
    let bend = u64::from(asset.mechanics.bend_limit.bits());
    i32::try_from(extent.saturating_mul(bend) / u64::from(u16::MAX) / 2).unwrap_or(i32::MAX)
}

fn deformation_regions(
    asset: &PlantFamilyAsset,
    bounds: PortableBounds,
    padding: i32,
) -> Vec<PortableDeformationRegion> {
    let swept_bounds = PortableBounds {
        min_bits: bounds.min_bits.map(|value| value.saturating_sub(padding)),
        max_bits: bounds.max_bits.map(|value| value.saturating_add(padding)),
    };
    let mut regions = asset
        .parts
        .iter()
        .map(|part| PortableDeformationRegion {
            part: part.id,
            semantic: PortableDeformationKind(semantic_tag(part.semantic)),
            influences: asset
                .spines
                .iter()
                .filter(|spine| spine.part == part.id)
                .map(|spine| spine.id)
                .collect(),
            static_bounds: bounds,
            swept_bounds,
        })
        .collect::<Vec<_>>();
    for region in &mut regions {
        region.influences.sort_unstable();
        region.influences.dedup();
    }
    regions.sort_unstable_by_key(|region| region.part);
    regions
}

const fn semantic_tag(semantic: PlantPartSemantic) -> u8 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BotanicalGraphDocument, InteractionPolicy, MechanicalResponse, PLANT_ASSET_VERSION,
        PhenotypeRole, PlantDimensions, PlantFamilyAsset, PlantFamilySource, PlantPart,
        PlantPhenotype, PlantSourceRole, PlantVariation,
    };
    use saffron_spatial::{DecisionScalar, UnitInterval};

    fn identity_use(part: u128, prototype: u32) -> MicroInstance {
        MicroInstance {
            part,
            prototype,
            transform_bits: [
                65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536,
            ],
        }
    }

    #[test]
    fn phenotype_active_parts_mask_the_uses() {
        let fixed = |value: i32| DecisionScalar::from_integer(value).expect("scalar");
        let asset = PlantFamilyAsset {
            role: crate::PlantFamilyRole::Family,
            modules: Vec::new(),
            module_recursion_limit: crate::MAX_PLANT_MODULE_RECURSION,
            version: PLANT_ASSET_VERSION,
            id: saffron_core::Uuid(1),
            name: "Mask fixture".to_owned(),
            tags: Vec::new(),
            source: PlantFamilySource::Native {
                graph: BotanicalGraphDocument::sapling(0x5a11),
                grafts: Vec::new(),
            },
            parts: vec![
                PlantPart {
                    id: 40,
                    parent: None,
                    semantic: PlantPartSemantic::Trunk,
                    material_slot: 0,
                    sources: Vec::new(),
                },
                PlantPart {
                    id: 41,
                    parent: Some(40),
                    semantic: PlantPartSemantic::Fruit,
                    material_slot: 0,
                    sources: Vec::new(),
                },
            ],
            dimensions: PlantDimensions {
                height: fixed(4),
                trunk_radius: fixed(1),
                crown_radius: [fixed(1); 2],
                root_radius: [fixed(1); 2],
                local_bounds_min: [fixed(-1); 3],
                local_bounds_max: [fixed(1); 3],
            },
            material_slots: Vec::new(),
            spines: Vec::new(),
            mechanics: MechanicalResponse {
                stiffness: fixed(1),
                damping: UnitInterval::from_bits(1),
                drag: fixed(1),
                flutter: fixed(1),
                bend_limit: UnitInterval::from_bits(1),
                damage_threshold: fixed(1),
                break_threshold: fixed(2),
            },
            variations: vec![PlantVariation {
                id: 0,
                name: "Default".to_owned(),
                sources: Vec::new(),
                active_parts: Vec::new(),
            }],
            phenotypes: vec![
                PlantPhenotype {
                    id: 0,
                    role: PhenotypeRole::Healthy,
                    season_window: None,
                    variation: 0,
                    material_remap: Vec::new(),
                    active_parts: Vec::new(),
                },
                PlantPhenotype {
                    id: 1,
                    role: PhenotypeRole::Harvested,
                    season_window: None,
                    variation: 0,
                    material_remap: Vec::new(),
                    active_parts: vec![40],
                },
            ],
            collision_proxies: Vec::new(),
            navigation_proxies: Vec::new(),
            interaction_policy: InteractionPolicy::Decorative,
            habitat: None,
            ecology: crate::PlantEcologyDeclaration::default(),
        };
        let family = crate::NormalizedPlantFamily {
            family: saffron_core::Uuid(1),
            tags: Vec::new(),
            sources: Vec::new(),
            meshes: vec![
                crate::NormalizedPlantMesh {
                    source: 10,
                    role: PlantSourceRole::Geometry,
                    selector: crate::PlantSourceSelector::Whole,
                    vertices: Vec::new(),
                    indices: Vec::new(),
                    submeshes: Vec::new(),
                    skin: Vec::new(),
                },
                crate::NormalizedPlantMesh {
                    source: 11,
                    role: PlantSourceRole::Geometry,
                    selector: crate::PlantSourceSelector::Whole,
                    vertices: Vec::new(),
                    indices: Vec::new(),
                    submeshes: Vec::new(),
                    skin: Vec::new(),
                },
            ],
            joints: Vec::new(),
            materials: Vec::new(),
            dimensions: asset.dimensions,
        };
        let uses = vec![identity_use(40, 0), identity_use(41, 1)];
        let combinations = use_combinations(&asset, &family, &uses);
        assert_eq!(combinations.len(), 2);
        assert_eq!(
            (combinations[0].variation, combinations[0].phenotype),
            (0, 0)
        );
        assert_eq!(combinations[0].active_words, vec![0b11]);
        assert_eq!(
            (combinations[1].variation, combinations[1].phenotype),
            (0, 1)
        );
        assert_eq!(
            combinations[1].active_words,
            vec![0b01],
            "the harvested phenotype drops the fruit use"
        );
    }
}
