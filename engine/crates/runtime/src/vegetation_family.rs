//! Play-session cache of resolved `.splant` family assets.
//!
//! Collision residency and promotion both read a plant's family declarations — proxies, material
//! slots, phenotypes — for every plant of that family in every resident cell. One load per family
//! per session serves both, and a family that fails to load is remembered as unavailable so the
//! warning is emitted once rather than per plant.

use std::collections::HashMap;
use std::sync::Arc;

use saffron_assets::{AssetServer, load_plant_family_asset};
use saffron_core::Uuid;
use saffron_vegetation::PlantFamilyAsset;

/// Lookup-only family cache; no iteration order ever escapes it.
#[derive(Default)]
pub(crate) struct PlantFamilyCache {
    entries: HashMap<Uuid, Option<Arc<PlantFamilyAsset>>>,
    failed: usize,
}

impl PlantFamilyCache {
    /// The family's asset, or `None` when it could not be loaded.
    pub(crate) fn get(
        &mut self,
        family: Uuid,
        assets: &AssetServer,
    ) -> Option<Arc<PlantFamilyAsset>> {
        if let Some(cached) = self.entries.get(&family) {
            return cached.clone();
        }
        let entry = match load_plant_family_asset(assets, family) {
            Ok(asset) => Some(Arc::new(asset)),
            Err(error) => {
                tracing::warn!(
                    "vegetation: plant family {family} failed to load, its plants carry no \
                     derived bodies or promoted views: {error}"
                );
                self.failed += 1;
                None
            }
        };
        self.entries.insert(family, entry.clone());
        entry
    }

    /// How many distinct families failed to load.
    pub(crate) fn failed_families(&self) -> usize {
        self.failed
    }

    /// Drops every cached family (a new play session re-resolves them).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.failed = 0;
    }
}
