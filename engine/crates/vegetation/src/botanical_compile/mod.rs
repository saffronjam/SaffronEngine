//! Turning a grown botanical assembly into the compiled family every downstream system reads.
//!
//! A native family produces exactly the same normalized shapes an imported one does — quantized
//! meshes, structural joints, semantic parts, spines, and dimensions — so nothing downstream can
//! tell the two apart. Generation is integer-exact: positions are Q15.16 metres, normals and
//! tangents are signed normalized, and every trigonometric value comes from the graph's own integer
//! table, so an authored plant compiles to identical bytes on every target.

mod geometry;
mod structure;

pub use geometry::*;
pub use structure::*;

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, UnitInterval};

use crate::{
    BotanicalBudget, BotanicalGraphDocument, BotanicalModuleResolver, Error, InteractionPolicy,
    MAX_PLANT_MODULE_RECURSION, MechanicalResponse, PLANT_ASSET_VERSION, PlantEcologyDeclaration,
    PlantFamilyAsset, PlantFamilyRole, PlantFamilySource, Result, grow,
};

/// Builds a complete native plant family from a botanical graph.
///
/// The graph is grown once so the family's parts, spines, and dimensions are what the plant
/// actually is rather than an authored guess that could drift from it.
///
/// # Errors
///
/// Propagates growing and generation, and [`Error::ArtifactFormat`] when `materials` does not cover
/// every material slot the graph binds.
pub fn native_plant_family(
    id: Uuid,
    name: &str,
    graph: BotanicalGraphDocument,
    materials: Vec<Uuid>,
    modules: &dyn BotanicalModuleResolver,
) -> Result<PlantFamilyAsset> {
    // Every declared variation grows, because the family's parts, dimensions, and material slots
    // must contain all of them — a slot only the sapling binds is still a slot.
    let grown = (0..graph.variations.len())
        .map(|index| {
            grow(&graph, index, modules, &BotanicalBudget::COOK).map(|growth| growth.assembly)
        })
        .collect::<Result<Vec<_>>>()?;
    let structure = widest_family_structure(&grown)?;
    let slots = grown
        .iter()
        .flat_map(|assembly| {
            assembly
                .shells
                .iter()
                .map(|shell| shell.material_slot)
                .chain(
                    assembly
                        .elements
                        .iter()
                        .map(|element| element.material_slot),
                )
        })
        .max()
        .map_or(0, |highest| highest as usize + 1);
    if materials.len() < slots {
        return Err(Error::ArtifactFormat {
            format: "botanical family",
            field: "materialSlots".to_owned(),
        });
    }
    let variations = native_variations(&graph);
    let phenotypes = native_phenotypes(&grown);
    // Proxies come from the representative individual: a variation is the same structure at another
    // size, and the runtime scales a proxy with the instance it belongs to.
    let (collision_proxies, navigation_proxies) =
        derive_family_proxies(&grown[0], &structure.dimensions);
    Ok(PlantFamilyAsset {
        role: PlantFamilyRole::Family,
        modules: Vec::new(),
        module_recursion_limit: MAX_PLANT_MODULE_RECURSION,
        version: PLANT_ASSET_VERSION,
        id,
        name: name.to_owned(),
        tags: Vec::new(),
        source: PlantFamilySource::Native {
            graph,
            grafts: Vec::new(),
        },
        parts: structure.parts,
        dimensions: structure.dimensions,
        material_slots: materials,
        spines: structure.spines,
        mechanics: MechanicalResponse {
            stiffness: DecisionScalar::from_bits(2 << 16),
            damping: UnitInterval::from_bits(12_000),
            drag: DecisionScalar::from_bits(1 << 16),
            flutter: DecisionScalar::from_bits(1 << 14),
            bend_limit: UnitInterval::from_bits(8_000),
            damage_threshold: DecisionScalar::from_bits(3 << 16),
            break_threshold: DecisionScalar::from_bits(6 << 16),
        },
        variations,
        phenotypes,
        collision_proxies,
        navigation_proxies,
        interaction_policy: InteractionPolicy::Structural,
        habitat: None,
        ecology: PlantEcologyDeclaration::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NoBotanicalModules, PlantPartSemantic};

    /// A created native family is immediately valid, carries the plant's own structure, and
    /// round-trips through the canonical container.
    #[test]
    fn a_created_native_family_is_valid_and_grown() {
        let family = native_plant_family(
            Uuid(41),
            "Sapling",
            BotanicalGraphDocument::sapling(0x5a11),
            vec![Uuid(11), Uuid(12)],
            &NoBotanicalModules,
        )
        .expect("the sapling becomes a family");
        crate::validate_plant_family(&family).expect("a created family validates");
        assert!(!family.spines.is_empty());
        assert!(
            family
                .parts
                .iter()
                .any(|part| { part.semantic == PlantPartSemantic::Trunk })
        );
        let bytes = crate::write_plant_asset(&family).expect("write");
        assert_eq!(crate::read_plant_asset(&bytes).expect("read"), family);

        // Too few material slots is refused rather than silently binding slot zero twice.
        assert!(
            native_plant_family(
                Uuid(41),
                "Sapling",
                BotanicalGraphDocument::sapling(0x5a11),
                vec![Uuid(11)],
                &NoBotanicalModules,
            )
            .is_err()
        );
    }
}
