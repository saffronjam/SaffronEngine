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
        deformation: deformation_regions(asset, family, family_bounds, padding),
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

/// The share of the family's authored bend a part reaching `part_top_bits` takes.
///
/// The vertex path scales sway by the square of a vertex's root-anchored height weight, so a
/// part whose crown reaches half the family height moves a quarter as far. Integer throughout,
/// in Q15.16: a cooked bound has to be the same byte on every target.
fn bend_share(padding: i32, part_top_bits: i32, family_top_bits: i32) -> i32 {
    if family_top_bits <= 0 {
        return padding;
    }
    let top = i128::from(part_top_bits.clamp(0, family_top_bits));
    let family = i128::from(family_top_bits);
    let scaled = i128::from(padding) * top * top / (family * family);
    i32::try_from(scaled).unwrap_or(padding)
}

/// Each part's own geometry bounds, by the same source-to-part rule the micro instances take:
/// an exact semantic target claims its mesh alone, otherwise every part naming the mesh's
/// source shares it. A part contributing no geometry has no entry.
fn part_bounds(
    asset: &PlantFamilyAsset,
    family: &NormalizedPlantFamily,
) -> BTreeMap<u128, PortableBounds> {
    let mut bounds: BTreeMap<u128, PortableBounds> = BTreeMap::new();
    for mesh in family
        .meshes
        .iter()
        .filter(|mesh| mesh.role == PlantSourceRole::Geometry)
    {
        let Some(mesh_bounds) = mesh
            .vertices
            .iter()
            .map(|vertex| vertex.position_bits)
            .fold(None::<PortableBounds>, |folded, position| {
                let point = PortableBounds {
                    min_bits: position,
                    max_bits: position,
                };
                Some(folded.map_or(point, |bounds| PortableBounds {
                    min_bits: std::array::from_fn(|axis| bounds.min_bits[axis].min(position[axis])),
                    max_bits: std::array::from_fn(|axis| bounds.max_bits[axis].max(position[axis])),
                }))
            })
        else {
            continue;
        };
        let mut widen = |part: u128| {
            bounds
                .entry(part)
                .and_modify(|current| {
                    *current = PortableBounds {
                        min_bits: std::array::from_fn(|axis| {
                            current.min_bits[axis].min(mesh_bounds.min_bits[axis])
                        }),
                        max_bits: std::array::from_fn(|axis| {
                            current.max_bits[axis].max(mesh_bounds.max_bits[axis])
                        }),
                    };
                })
                .or_insert(mesh_bounds);
        };
        if let Some(part) = target_part_for_mesh(asset, mesh) {
            widen(part);
            continue;
        }
        for part in asset
            .parts
            .iter()
            .filter(|part| part.sources.contains(&mesh.source))
        {
            widen(part.id);
        }
    }
    bounds
}

/// One deformation region per part, each on its OWN geometry and its own share of the bend.
///
/// The family box would be conservative for every part at once and useless for any of them: a
/// trunk and the leaves it carries would declare the same swept extent, and a consumer culling
/// on it could never reject one without the other.
fn deformation_regions(
    asset: &PlantFamilyAsset,
    family: &NormalizedPlantFamily,
    bounds: PortableBounds,
    padding: i32,
) -> Vec<PortableDeformationRegion> {
    let by_part = part_bounds(asset, family);
    let mut regions = asset
        .parts
        .iter()
        .map(|part| {
            let static_bounds = by_part.get(&part.id).copied().unwrap_or(bounds);
            let share = bend_share(padding, static_bounds.max_bits[1], bounds.max_bits[1]);
            PortableDeformationRegion {
                part: part.id,
                semantic: PortableDeformationKind(semantic_tag(part.semantic)),
                influences: asset
                    .spines
                    .iter()
                    .filter(|spine| spine.part == part.id)
                    .map(|spine| spine.id)
                    .collect(),
                static_bounds,
                swept_bounds: PortableBounds {
                    min_bits: static_bounds
                        .min_bits
                        .map(|value| value.saturating_sub(share)),
                    max_bits: static_bounds
                        .max_bits
                        .map(|value| value.saturating_add(share)),
                },
            }
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
        PhenotypeResponse, PhenotypeRole, PlantDimensions, PlantFamilyAsset, PlantFamilySource,
        PlantPart, PlantPhenotype, PlantSourceRole, PlantVariation,
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

    fn fixed(value: i32) -> DecisionScalar {
        DecisionScalar::from_integer(value).expect("scalar")
    }

    fn part(id: u128, parent: Option<u128>, semantic: PlantPartSemantic) -> PlantPart {
        PlantPart {
            id,
            parent,
            semantic,
            material_slot: 0,
            sources: vec![id],
        }
    }

    fn fixture_asset(
        parts: Vec<PlantPart>,
        top: i32,
        bend_limit: UnitInterval,
    ) -> PlantFamilyAsset {
        PlantFamilyAsset {
            role: crate::PlantFamilyRole::Family,
            modules: Vec::new(),
            module_recursion_limit: crate::MAX_PLANT_MODULE_RECURSION,
            version: PLANT_ASSET_VERSION,
            id: saffron_core::Uuid(1),
            name: "Fixture".to_owned(),
            tags: Vec::new(),
            source: PlantFamilySource::Native {
                graph: BotanicalGraphDocument::sapling(0x5a11),
                grafts: Vec::new(),
            },
            parts,
            dimensions: PlantDimensions {
                height: fixed(top),
                trunk_radius: fixed(1),
                crown_radius: [fixed(1); 2],
                root_radius: [fixed(1); 2],
                local_bounds_min: [fixed(-1), fixed(0), fixed(-1)],
                local_bounds_max: [fixed(1), fixed(top), fixed(1)],
            },
            material_slots: Vec::new(),
            spines: Vec::new(),
            mechanics: MechanicalResponse {
                stiffness: fixed(1),
                damping: UnitInterval::from_bits(1),
                drag: fixed(1),
                flutter: fixed(1),
                bend_limit,
                damage_threshold: fixed(1),
                break_threshold: fixed(2),
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
                response: PhenotypeResponse::default(),
                variation: 0,
                material_remap: Vec::new(),
                active_parts: Vec::new(),
            }],
            collision_proxies: Vec::new(),
            navigation_proxies: Vec::new(),
            interaction_policy: InteractionPolicy::Decorative,
            habitat: None,
            ecology: crate::PlantEcologyDeclaration::default(),
        }
    }

    fn fixture_mesh(source: u128, positions: &[[i32; 3]]) -> crate::NormalizedPlantMesh {
        crate::NormalizedPlantMesh {
            source,
            role: PlantSourceRole::Geometry,
            selector: crate::PlantSourceSelector::Whole,
            vertices: positions
                .iter()
                .map(|position| crate::NormalizedPlantVertex {
                    position_bits: position.map(|value| value * 65_536),
                    normal_snorm: [0, 32_767, 0],
                    uv_bits: [0; 2],
                    tangent_snorm: [32_767, 0, 0, 32_767],
                })
                .collect(),
            indices: Vec::new(),
            submeshes: Vec::new(),
            skin: Vec::new(),
        }
    }

    fn fixture_family(
        asset: &PlantFamilyAsset,
        meshes: Vec<crate::NormalizedPlantMesh>,
    ) -> crate::NormalizedPlantFamily {
        crate::NormalizedPlantFamily {
            family: saffron_core::Uuid(1),
            tags: Vec::new(),
            sources: Vec::new(),
            meshes,
            joints: Vec::new(),
            materials: Vec::new(),
            dimensions: asset.dimensions,
        }
    }

    #[test]
    fn phenotype_active_parts_mask_the_uses() {
        let mut asset = fixture_asset(
            vec![
                part(40, None, PlantPartSemantic::Trunk),
                part(41, Some(40), PlantPartSemantic::Fruit),
            ],
            4,
            UnitInterval::from_bits(1),
        );
        for part in &mut asset.parts {
            part.sources.clear();
        }
        asset.phenotypes.push(PlantPhenotype {
            id: 1,
            role: PhenotypeRole::Harvested,
            response: PhenotypeResponse::default(),
            variation: 0,
            material_remap: Vec::new(),
            active_parts: vec![40],
        });
        let family = fixture_family(&asset, vec![fixture_mesh(10, &[]), fixture_mesh(11, &[])]);
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

    /// An eight-metre family whose trunk occupies the lowest two metres and whose crown sits
    /// between six and eight: two parts that must not share one box.
    fn two_storey_family() -> (PlantFamilyAsset, crate::NormalizedPlantFamily) {
        let asset = fixture_asset(
            vec![
                part(1, None, PlantPartSemantic::Trunk),
                part(2, Some(1), PlantPartSemantic::Leaf),
            ],
            8,
            UnitInterval::ONE,
        );
        let family = fixture_family(
            &asset,
            vec![
                fixture_mesh(1, &[[-1, 0, -1], [1, 2, 1]]),
                fixture_mesh(2, &[[-3, 6, -3], [3, 8, 3]]),
            ],
        );
        (asset, family)
    }

    #[test]
    fn each_part_declares_its_own_box_not_the_familys() {
        let (asset, family) = two_storey_family();
        let input = plant_hierarchy_input(&asset, &family, &[]).expect("hierarchy input");
        let trunk = &input.deformation[0];
        let crown = &input.deformation[1];
        assert_eq!((trunk.part, crown.part), (1, 2));
        assert_eq!(
            (
                trunk.static_bounds.min_bits[1],
                trunk.static_bounds.max_bits[1]
            ),
            (0, 2 * 65_536),
            "the trunk region carries the trunk's own geometry"
        );
        assert_eq!(
            (
                crown.static_bounds.min_bits[1],
                crown.static_bounds.max_bits[1]
            ),
            (6 * 65_536, 8 * 65_536),
            "the crown region carries the crown's own geometry"
        );
        assert_ne!(
            trunk.static_bounds, crown.static_bounds,
            "the family box would make every part identical"
        );
        assert_eq!(
            input.bounds.max_bits[1],
            8 * 65_536,
            "the family box is still the whole plant"
        );
    }

    #[test]
    fn a_part_sweeps_by_its_own_share_of_the_bend() {
        let (asset, family) = two_storey_family();
        let input = plant_hierarchy_input(&asset, &family, &[]).expect("hierarchy input");
        let sweep = |region: &PortableDeformationRegion| {
            region.swept_bounds.max_bits[0] - region.static_bounds.max_bits[0]
        };
        let trunk = sweep(&input.deformation[0]);
        let crown = sweep(&input.deformation[1]);
        assert!(trunk >= 0 && crown > 0, "trunk {trunk} crown {crown}");
        // The vertex path scales sway by weight², so a part topping out at a quarter of the
        // family height sweeps a sixteenth as far — not the same distance.
        assert!(
            crown > trunk * 8,
            "the crown sweeps far more than the trunk: trunk {trunk} crown {crown}"
        );
        assert_eq!(
            crown, input.deformation_padding,
            "the part reaching the family top takes the whole bend"
        );
        for region in &input.deformation {
            assert!(
                region.swept_bounds.min_bits[1] <= region.static_bounds.min_bits[1]
                    && region.swept_bounds.max_bits[1] >= region.static_bounds.max_bits[1],
                "a swept box always encloses its static one"
            );
        }
    }
}
