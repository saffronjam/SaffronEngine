use std::collections::BTreeMap;

use saffron_spatial::{DecisionScalar, UnitInterval};

use crate::{
    BotanicalAssembly, BotanicalAxis, BotanicalElement, BotanicalElementId, BotanicalGraphDocument,
    Error, PhenotypeRole, PlantCollisionProxy, PlantCollisionShape, PlantDimensions,
    PlantNavigationProxy, PlantPart, PlantPhenotype, PlantVariation, Result, StructuralSpine,
    native_variation_source_id, turn_sin_cos,
};

/// Collision proxies one grown plant may derive; a proxy per twig would be a body-per-branch
/// explosion in the runtime's batched collision residency.
pub const MAX_DERIVED_COLLISION_PROXIES: usize = 8;

/// The identity of the one navigation proxy a native family derives.
const NAVIGATION_PROXY_ID: u128 = 1 << 125;

/// Element classes that fall off a plant, and so distinguish one appearance from another.
const PERISHABLE: [BotanicalElement; 7] = [
    BotanicalElement::Leaf,
    BotanicalElement::Needle,
    BotanicalElement::Blade,
    BotanicalElement::Frond,
    BotanicalElement::Flower,
    BotanicalElement::Fruit,
    BotanicalElement::Bud,
];

/// The family structure one assembly declares: semantic parts, spines, and dimensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalStructure {
    /// Semantic parts, one per axis plus one per instanced element class.
    pub parts: Vec<PlantPart>,
    /// Structural spines, one per axis.
    pub spines: Vec<StructuralSpine>,
    /// Validated bounds and footprints.
    pub dimensions: PlantDimensions,
}

/// The stable part identity for one botanical element class.
///
/// Classes rather than individuals: a family declares "this is its leaves", and the thousands of
/// instanced leaves are micro transforms under that one part.
#[must_use]
pub fn class_part_id(element: BotanicalElement) -> u128 {
    (1_u128 << 120) | u128::from(element.tag())
}

/// Derives the semantic parts, spines, and dimensions the family declares.
///
/// # Errors
///
/// [`Error::ArtifactFormat`] when the assembly grew nothing.
pub fn derive_family_structure(assembly: &BotanicalAssembly) -> Result<BotanicalStructure> {
    if assembly.axes.is_empty() {
        return Err(Error::ArtifactFormat {
            format: "botanical family",
            field: "axes".to_owned(),
        });
    }
    // One part per element class present, parented by the class hierarchy the axes describe.
    let mut classes: BTreeMap<BotanicalElement, Option<BotanicalElement>> = BTreeMap::new();
    let axis_class: BTreeMap<BotanicalElementId, BotanicalElement> = assembly
        .axes
        .iter()
        .map(|axis| (axis.id, axis.element))
        .collect();
    for axis in &assembly.axes {
        let parent = axis
            .parent
            .and_then(|parent| axis_class.get(&parent).copied())
            .filter(|parent| *parent != axis.element);
        classes.entry(axis.element).or_insert(parent);
    }
    let frame_axis: BTreeMap<BotanicalElementId, BotanicalElementId> = assembly
        .frames
        .iter()
        .map(|frame| (frame.id, frame.axis))
        .collect();
    let mut slots: BTreeMap<BotanicalElement, u32> = assembly
        .shells
        .iter()
        .map(|shell| (shell.element, shell.material_slot))
        .collect();
    for element in &assembly.elements {
        let parent = frame_axis
            .get(&element.frame)
            .and_then(|axis| axis_class.get(axis).copied());
        classes.entry(element.element).or_insert(parent);
        slots
            .entry(element.element)
            .or_insert(element.material_slot);
    }

    let parts = classes
        .iter()
        .map(|(element, parent)| PlantPart {
            id: class_part_id(*element),
            parent: parent.map(class_part_id),
            semantic: element.part_semantic(),
            material_slot: slots.get(element).copied().unwrap_or_default(),
            sources: Vec::new(),
        })
        .collect();

    let spines = assembly
        .axes
        .iter()
        .map(|axis| StructuralSpine {
            id: axis.id.value(),
            part: class_part_id(axis.element),
            parent: axis.parent.map(BotanicalElementId::value),
            rest_points: axis.points.clone(),
            radii: axis.radii.clone(),
        })
        .collect();

    let (minimum, maximum) = assembly.local_bounds();
    // A footprint is the widest reach of the relevant axes, including their own radius: a straight
    // trunk still occupies ground, and a zero footprint would be a lie the validator rejects.
    let horizontal = |root: bool| -> [DecisionScalar; 2] {
        let mut extent = [1_i32; 2];
        for axis in assembly
            .axes
            .iter()
            .filter(|axis| matches!(axis.element, BotanicalElement::Root) == root)
        {
            for (point, radius) in axis.points.iter().zip(&axis.radii) {
                extent[0] = extent[0].max(point[0].bits().abs().saturating_add(radius.bits()));
                extent[1] = extent[1].max(point[2].bits().abs().saturating_add(radius.bits()));
            }
        }
        [
            DecisionScalar::from_bits(extent[0]),
            DecisionScalar::from_bits(extent[1]),
        ]
    };
    let trunk_radius = assembly
        .axes
        .iter()
        .filter(|axis| matches!(axis.element, BotanicalElement::Trunk))
        .filter_map(|axis| axis.radii.first().copied())
        .max_by_key(|radius| radius.bits())
        .unwrap_or_else(|| DecisionScalar::from_bits(1));

    Ok(BotanicalStructure {
        parts,
        spines,
        dimensions: PlantDimensions {
            height: DecisionScalar::from_bits(maximum[1].bits().max(1)),
            trunk_radius,
            crown_radius: horizontal(false),
            root_radius: horizontal(true),
            local_bounds_min: minimum,
            local_bounds_max: maximum,
        },
    })
}

/// The structure a family declares over every variation it grows: the union of their parts and
/// spines, and dimensions wide enough to contain all of them.
///
/// The spines are the representative individual's, because element identities do not depend on the
/// seed or the age: every variation describes the same skeleton at a different size.
///
/// # Errors
///
/// Propagates [`derive_family_structure`], and refuses an empty variation list.
pub fn widest_family_structure(grown: &[BotanicalAssembly]) -> Result<BotanicalStructure> {
    let mut structures = grown
        .iter()
        .map(derive_family_structure)
        .collect::<Result<Vec<_>>>()?
        .into_iter();
    let mut widest = structures.next().ok_or_else(|| Error::ArtifactFormat {
        format: "botanical family",
        field: "variations".to_owned(),
    })?;
    for structure in structures {
        for part in structure.parts {
            if !widest.parts.iter().any(|existing| existing.id == part.id) {
                widest.parts.push(part);
            }
        }
        let dimensions = structure.dimensions;
        widest.dimensions.height = widest.dimensions.height.max(dimensions.height);
        widest.dimensions.trunk_radius =
            widest.dimensions.trunk_radius.max(dimensions.trunk_radius);
        for lane in 0..2 {
            widest.dimensions.crown_radius[lane] =
                widest.dimensions.crown_radius[lane].max(dimensions.crown_radius[lane]);
            widest.dimensions.root_radius[lane] =
                widest.dimensions.root_radius[lane].max(dimensions.root_radius[lane]);
        }
        for lane in 0..3 {
            widest.dimensions.local_bounds_min[lane] = DecisionScalar::from_bits(
                widest.dimensions.local_bounds_min[lane]
                    .bits()
                    .min(dimensions.local_bounds_min[lane].bits()),
            );
            widest.dimensions.local_bounds_max[lane] = DecisionScalar::from_bits(
                widest.dimensions.local_bounds_max[lane]
                    .bits()
                    .max(dimensions.local_bounds_max[lane].bits()),
            );
        }
    }
    widest.parts.sort_by_key(|part| part.id);
    Ok(widest)
}

/// The family variation table one graph's declared individuals produce, one row per declared
/// variation naming its own grown geometry.
#[must_use]
pub fn native_variations(graph: &BotanicalGraphDocument) -> Vec<PlantVariation> {
    graph
        .variations
        .iter()
        .enumerate()
        .map(|(index, variation)| PlantVariation {
            id: index as u32,
            name: variation.name.clone(),
            sources: vec![native_variation_source_id(index)],
            active_parts: Vec::new(),
        })
        .collect()
}

/// The phenotype table one graph's grown individuals can express.
///
/// A phenotype is a selection over the classes a variation actually grew: flowers without fruit,
/// fruit without flowers, neither once harvested, and only the woody structure once dead. Roles that
/// are purely a material change — senescent, damaged, burned, wet — need authored per-role materials
/// and are not invented here.
#[must_use]
pub fn native_phenotypes(grown: &[BotanicalAssembly]) -> Vec<PlantPhenotype> {
    let mut phenotypes = Vec::new();
    let mut next = 0_u32;
    for (index, assembly) in grown.iter().enumerate() {
        let mut classes: BTreeMap<BotanicalElement, ()> = BTreeMap::new();
        for axis in &assembly.axes {
            classes.insert(axis.element, ());
        }
        for element in &assembly.elements {
            classes.insert(element.element, ());
        }
        let present: Vec<BotanicalElement> = classes.keys().copied().collect();
        let without = |excluded: &[BotanicalElement]| -> Vec<u128> {
            present
                .iter()
                .filter(|class| !excluded.contains(class))
                .map(|class| class_part_id(*class))
                .collect()
        };
        let mut push = |role: PhenotypeRole, active_parts: Vec<u128>| {
            phenotypes.push(PlantPhenotype {
                id: next,
                role,
                season_window: None,
                variation: index as u32,
                material_remap: Vec::new(),
                active_parts,
            });
            next += 1;
        };
        push(PhenotypeRole::Healthy, Vec::new());
        let flowering = present.contains(&BotanicalElement::Flower);
        let fruiting = present.contains(&BotanicalElement::Fruit);
        if flowering {
            push(
                PhenotypeRole::Flowering,
                without(&[BotanicalElement::Fruit]),
            );
        }
        if fruiting {
            push(
                PhenotypeRole::Fruiting,
                without(&[BotanicalElement::Flower]),
            );
        }
        if flowering || fruiting {
            push(
                PhenotypeRole::Harvested,
                without(&[BotanicalElement::Flower, BotanicalElement::Fruit]),
            );
        }
        if present.iter().any(|class| PERISHABLE.contains(class)) {
            push(PhenotypeRole::Dead, without(&PERISHABLE));
        }
    }
    phenotypes
}

/// The collision and navigation proxies one grown plant declares.
///
/// A native family's proxies are a result, like its dimensions — a stale authored value would be a
/// second truth about the same geometry. A capsule stands in for each axis thick enough for a
/// character to collide with, thickest first, and one octagonal footprint stands in for the plant on
/// the navigation seam.
#[must_use]
pub fn derive_family_proxies(
    assembly: &BotanicalAssembly,
    dimensions: &PlantDimensions,
) -> (Vec<PlantCollisionProxy>, Vec<PlantNavigationProxy>) {
    let thickest = assembly
        .axes
        .iter()
        .filter(|axis| matches!(axis.element, BotanicalElement::Trunk))
        .filter_map(|axis| axis.radii.first().map(|radius| radius.bits()))
        .max()
        .unwrap_or_else(|| dimensions.trunk_radius.bits())
        .max(1);
    // A quarter of the trunk is the thinnest thing worth colliding with: below that a character
    // brushes past a twig, and the body is cost without behaviour.
    let floor = (thickest / 4).max(1);
    let mut candidates: Vec<&BotanicalAxis> = assembly
        .axes
        .iter()
        .filter(|axis| {
            // Roots are below ground, so nothing walks into them.
            !matches!(axis.element, BotanicalElement::Root)
                && axis.radii.iter().any(|radius| radius.bits() >= floor)
        })
        .collect();
    candidates.sort_by_key(|axis| {
        (
            std::cmp::Reverse(
                axis.radii
                    .iter()
                    .map(|radius| radius.bits())
                    .max()
                    .unwrap_or_default(),
            ),
            axis.id,
        )
    });
    candidates.truncate(MAX_DERIVED_COLLISION_PROXIES);
    let mut collision: Vec<PlantCollisionProxy> = candidates
        .iter()
        .filter_map(|axis| {
            let base = axis.points.first()?;
            let tip = axis.points.last()?;
            let radius = axis.radii.iter().map(|radius| radius.bits()).max()?.max(1);
            let center = std::array::from_fn(|lane| {
                DecisionScalar::from_bits((base[lane].bits() + tip[lane].bits()) / 2)
            });
            let half_height = (axis.length().bits() / 2).max(1);
            Some(PlantCollisionProxy {
                id: axis.id.value(),
                shape: PlantCollisionShape::Capsule,
                part: class_part_id(axis.element),
                center,
                dimensions: [
                    DecisionScalar::from_bits(radius),
                    DecisionScalar::from_bits(half_height),
                    DecisionScalar::from_bits(radius),
                ],
                // A trunk is the plant; break it and the plant is felled rather than pruned.
                breakable: !matches!(axis.element, BotanicalElement::Trunk),
            })
        })
        .collect();
    collision.sort_by_key(|proxy| proxy.id);

    // An octagon of the trunk radius: a character routes around the stem, not around the canopy.
    let footprint_radius = dimensions.trunk_radius.bits().max(1);
    let one = i64::from(UnitInterval::ONE.bits());
    let footprint = (0..8)
        .map(|corner| {
            let (sin, cos) = turn_sin_cos(one * i64::from(corner) / 8);
            [
                DecisionScalar::from_bits(
                    i32::try_from(i64::from(footprint_radius) * cos / one).unwrap_or(i32::MAX),
                ),
                DecisionScalar::from_bits(
                    i32::try_from(i64::from(footprint_radius) * sin / one).unwrap_or(i32::MAX),
                ),
            ]
        })
        .collect();
    let navigation = vec![PlantNavigationProxy {
        id: NAVIGATION_PROXY_ID,
        footprint,
        height: DecisionScalar::from_bits(dimensions.height.bits().max(1)),
        // Neutral: the interaction policy decides whether this reads as an obstacle or a cost.
        cost: UnitInterval::ONE,
    }];
    (collision, navigation)
}

#[cfg(test)]
mod tests {
    use super::super::native_plant_family;
    use super::*;
    use crate::{BotanicalBudget, BotanicalOperator, NoBotanicalModules, grow};

    fn grown_once(document: &BotanicalGraphDocument) -> BotanicalAssembly {
        grow(document, 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .expect("the graph grows")
            .assembly
    }

    /// Parts follow the class hierarchy the axes describe, and one spine per axis travels with it.
    #[test]
    fn a_grown_plant_declares_its_parts_and_spines() {
        let assembly = grown_once(&crate::botanical::tests_support::birch());
        let structure = derive_family_structure(&assembly).expect("structure");
        assert_eq!(structure.spines.len(), assembly.axes.len());
        // Trunk, branch, root, leaf.
        assert_eq!(structure.parts.len(), 4);
        assert!(structure.dimensions.height.bits() > 0);
        assert!(structure.dimensions.root_radius[0].bits() > 0);
        let part = |element: BotanicalElement| {
            structure
                .parts
                .iter()
                .find(|part| part.id == class_part_id(element))
                .expect("part")
        };
        assert_eq!(
            part(BotanicalElement::Branch).parent,
            Some(class_part_id(BotanicalElement::Trunk))
        );
        assert_eq!(
            part(BotanicalElement::Leaf).parent,
            Some(class_part_id(BotanicalElement::Branch))
        );
        assert_eq!(part(BotanicalElement::Trunk).parent, None);
    }

    /// An empty assembly is refused rather than compiled into a family with no plant in it.
    #[test]
    fn an_empty_assembly_has_no_structure() {
        assert!(derive_family_structure(&BotanicalAssembly::default()).is_err());
    }

    /// Every declared variation becomes its own family variation naming its own geometry, and the
    /// family's dimensions contain all of them.
    #[test]
    fn variations_each_carry_their_own_geometry() {
        let mut document = BotanicalGraphDocument::sapling(0x5a11);
        document.variations.push(crate::BotanicalVariation {
            seed: 0x5a11,
            age: UnitInterval::from_bits(20_000),
            name: "Sapling".to_owned(),
        });
        let rows = native_variations(&document);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Mature");
        assert_eq!(rows[1].name, "Sapling");
        assert_ne!(rows[0].sources, rows[1].sources, "one source each");

        let grown: Vec<BotanicalAssembly> = (0..2)
            .map(|index| {
                grow(
                    &document,
                    index,
                    &NoBotanicalModules,
                    &BotanicalBudget::COOK,
                )
                .unwrap()
                .assembly
            })
            .collect();
        let widest = widest_family_structure(&grown).expect("structure");
        let mature = derive_family_structure(&grown[0]).expect("mature structure");
        assert_eq!(
            widest.dimensions.height, mature.dimensions.height,
            "the tallest variation sets the family height"
        );
        assert_eq!(
            widest.spines.len(),
            mature.spines.len(),
            "one skeleton declaration: every variation is the same structure at another size"
        );
    }

    /// A native family's proxies are derived from what it grew: a capsule per axis thick enough to
    /// collide with, and one footprint on the navigation seam.
    #[test]
    fn proxies_are_derived_from_the_grown_plant() {
        let document = BotanicalGraphDocument::sapling(0x5a11);
        let assembly = grown_once(&document);
        let structure = derive_family_structure(&assembly).unwrap();
        let (collision, navigation) = derive_family_proxies(&assembly, &structure.dimensions);

        assert!(!collision.is_empty());
        assert!(collision.len() <= MAX_DERIVED_COLLISION_PROXIES);
        assert!(
            collision
                .iter()
                .all(|proxy| proxy.dimensions.iter().all(|value| value.bits() > 0)),
            "every capsule has extent"
        );
        assert!(
            !collision
                .iter()
                .any(|proxy| proxy.part == class_part_id(BotanicalElement::Root)),
            "nothing walks into a root"
        );
        let trunk = collision
            .iter()
            .find(|proxy| proxy.part == class_part_id(BotanicalElement::Trunk))
            .expect("the trunk collides");
        assert!(!trunk.breakable, "breaking the trunk fells the plant");
        // Ids are the axis identities, so a proxy survives a parameter change like an edit does.
        let ids: BTreeMap<u128, ()> = collision.iter().map(|proxy| (proxy.id, ())).collect();
        assert_eq!(ids.len(), collision.len());

        assert_eq!(navigation.len(), 1);
        assert_eq!(navigation[0].footprint.len(), 8);
        assert_eq!(navigation[0].height, structure.dimensions.height);

        // A family built the usual way carries them, and the whole thing validates.
        let family = native_plant_family(
            saffron_core::Uuid(4_242),
            "Derived",
            document,
            vec![saffron_core::Uuid(1), saffron_core::Uuid(2)],
            &NoBotanicalModules,
        )
        .expect("the family builds");
        assert_eq!(family.collision_proxies, collision);
        assert_eq!(family.navigation_proxies, navigation);
        crate::validate_plant_family(&family).expect("a derived family is valid");
    }

    /// A phenotype is a selection over the classes a variation actually grew, so a family without
    /// flowers has no flowering appearance to offer.
    #[test]
    fn phenotypes_follow_the_classes_a_variation_grew() {
        let document = BotanicalGraphDocument::sapling(0x5a11);
        let roles: Vec<PhenotypeRole> = native_phenotypes(&[grown_once(&document)])
            .iter()
            .map(|phenotype| phenotype.role)
            .collect();
        assert_eq!(
            roles,
            vec![PhenotypeRole::Healthy, PhenotypeRole::Dead],
            "leaves can be lost; there are no flowers or fruit to gain"
        );

        // Add fruit and the harvest appearances appear with it.
        let mut fruiting = document.clone();
        for node in &mut fruiting.nodes {
            if let BotanicalOperator::Instance { element, .. } = &mut node.operator {
                *element = BotanicalElement::Fruit;
            }
        }
        let phenotypes = native_phenotypes(&[grown_once(&fruiting)]);
        let roles: Vec<PhenotypeRole> = phenotypes.iter().map(|phenotype| phenotype.role).collect();
        assert_eq!(
            roles,
            vec![
                PhenotypeRole::Healthy,
                PhenotypeRole::Fruiting,
                PhenotypeRole::Harvested,
                PhenotypeRole::Dead,
            ]
        );
        let harvested = phenotypes
            .iter()
            .find(|phenotype| phenotype.role == PhenotypeRole::Harvested)
            .expect("a harvested appearance");
        assert!(
            !harvested
                .active_parts
                .contains(&class_part_id(BotanicalElement::Fruit)),
            "a harvested plant has had its fruit taken"
        );
        assert!(
            harvested
                .active_parts
                .contains(&class_part_id(BotanicalElement::Trunk)),
            "and keeps its trunk"
        );
        // Identities are unique family-wide across variations.
        let ids: BTreeMap<u32, ()> = phenotypes
            .iter()
            .map(|phenotype| (phenotype.id, ()))
            .collect();
        assert_eq!(ids.len(), phenotypes.len());
    }
}
