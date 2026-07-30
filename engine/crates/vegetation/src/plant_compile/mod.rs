//! Deterministic, format-erased plant-family source normalization.

mod diagnostic;
mod imported;
mod material;
mod mesh;
mod native;
mod normalized;
mod source;

#[cfg(test)]
mod tests;

pub use diagnostic::*;
pub use normalized::*;
pub use source::*;

use imported::compile_imported_plant_family;
use material::*;
use mesh::*;
use native::compile_native_plant_family;

use saffron_core::Uuid;

use crate::{PlantFamilyAsset, PlantFamilySource, Result, validate_plant_family};

/// Semantic version of deterministic imported-family normalization.
pub const PLANT_SOURCE_COMPILER_VERSION: u32 = 2;

/// Stable source identity reserved for one family's embedded botanical graph.
#[must_use]
pub fn native_plant_source_id(family: Uuid) -> u128 {
    (1_u128 << 127) | u128::from(family.value())
}

/// Stable source identity of one variation's grown geometry.
///
/// Family-local, so it derives from the variation index alone: any family-id-derived value would
/// move whenever the catalog reassigns that id.
#[must_use]
pub fn native_variation_source_id(variation: usize) -> u128 {
    (1_u128 << 126) | u128::from(variation as u64)
}

/// Hashes the complete canonical embedded botanical graph source.
#[must_use]
pub fn native_botanical_graph_content_hash(graph: &crate::BotanicalGraphDocument) -> [u8; 32] {
    graph.identity().bytes()
}

/// Normalizes imported and native `.splant` sources through one validation/recook compiler.
pub fn compile_plant_family(
    asset: &PlantFamilyAsset,
    snapshots: &[PlantSourceSnapshot],
    limits: PlantCompileLimits,
    modules: &dyn crate::BotanicalModuleResolver,
) -> Result<PlantCompileOutput> {
    validate_plant_family(asset)?;
    match &asset.source {
        PlantFamilySource::Imported(recipe) => {
            compile_imported_plant_family(asset, recipe, snapshots, limits)
        }
        PlantFamilySource::Native { graph, grafts } => {
            compile_native_plant_family(asset, graph, grafts, snapshots, limits, modules)
        }
    }
}
