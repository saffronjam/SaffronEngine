//! Per-facet residency budgets, admission, and the load requests they produce.

use std::collections::BTreeMap;
use std::sync::Arc;

use saffron_spatial::{
    ResidencyFacet, ResidencyMask, ResidencySnapshot, SpatialSource, SpatialSourceId, WorldCellKey,
};

use crate::{Error, PlantPointColumns, Result, VegetationManifestCell};

use super::VegetationWorld;
use super::generation::{
    VegetationCellGeneration, VegetationCellGenerationId, effective_macro_points,
    macro_columns_optional, required_sections,
};

/// Explicit decoded-byte ceilings for each logical residency facet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationResidencyBudgets {
    /// Maximum render-facet decoded bytes.
    pub render: u64,
    /// Maximum physics-facet decoded bytes.
    pub physics: u64,
    /// Maximum simulation-facet decoded bytes.
    pub simulation: u64,
    /// Maximum editor-facet decoded bytes.
    pub editing: u64,
    /// Maximum navigation-facet decoded bytes.
    pub navigation: u64,
    /// Maximum network-interest decoded bytes.
    pub network: u64,
}

impl VegetationResidencyBudgets {
    /// No practical runtime ceiling while retaining checked accounting.
    pub const UNLIMITED: Self = Self {
        render: u64::MAX,
        physics: u64::MAX,
        simulation: u64::MAX,
        editing: u64::MAX,
        navigation: u64::MAX,
        network: u64::MAX,
    };

    const fn get(self, facet: ResidencyFacet) -> u64 {
        match facet {
            ResidencyFacet::Render => self.render,
            ResidencyFacet::Physics => self.physics,
            ResidencyFacet::Simulation => self.simulation,
            ResidencyFacet::Editing => self.editing,
            ResidencyFacet::Navigation => self.navigation,
            ResidencyFacet::Network => self.network,
        }
    }
}

impl Default for VegetationResidencyBudgets {
    fn default() -> Self {
        Self::UNLIMITED
    }
}

/// A coalesced missing-facet request for one cell generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationCellLoadRequest {
    pub cell: WorldCellKey,
    /// Logical facets missing from its complete published generation.
    pub facets: ResidencyMask,
    /// Highest contributing source priority.
    pub priority: i32,
    /// Monotonic residency revision used to reject late work.
    pub source_revision: u64,
}

/// Per-facet requested/resident byte accounting and queue status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationResidencyReport {
    /// Number of active predictive sources.
    pub source_count: usize,
    /// Number of cells with at least one requested facet.
    pub requested_cells: usize,
    /// Number of cells retaining a published facet.
    pub resident_cells: usize,
    /// Requested decoded bytes in [`ResidencyFacet::ALL`] order.
    pub requested_bytes: [u64; saffron_spatial::FACET_COUNT],
    /// Published decoded bytes in [`ResidencyFacet::ALL`] order.
    pub resident_bytes: [u64; saffron_spatial::FACET_COUNT],
    /// Configured decoded-byte ceilings in [`ResidencyFacet::ALL`] order.
    pub budgets: [u64; saffron_spatial::FACET_COUNT],
    /// Coalesced cell load requests.
    pub pending: Vec<VegetationCellLoadRequest>,
}

impl VegetationWorld {
    pub fn update_source(&mut self, source: SpatialSource) -> Result<()> {
        self.residency.update_source(source)?;
        self.advance_residency_revision()?;
        self.invalidate_cell_loads()?;
        self.unload_unrequested_facets()
    }

    /// Removes one predictive source and releases unreferenced facets.
    pub fn remove_source(&mut self, source: SpatialSourceId) -> Result<bool> {
        let removed = self.residency.remove_source(source);
        if removed {
            self.advance_residency_revision()?;
            self.invalidate_cell_loads()?;
            self.unload_unrequested_facets()?;
        }
        Ok(removed)
    }

    /// Active sources in stable identity order.
    #[must_use]
    pub fn sources(&self) -> Vec<SpatialSource> {
        self.residency.sources()
    }

    pub fn residency_report(&self) -> Result<VegetationResidencyReport> {
        let snapshots = self.residency.snapshots()?;
        let requested_bytes = self.requested_bytes(&snapshots)?;
        let mut resident_bytes = [0_u64; saffron_spatial::FACET_COUNT];
        let mut resident_cells = 0;
        for (cell, entry) in &self.cells {
            let generation = entry.slot.read();
            if generation.resident == ResidencyMask::NONE {
                continue;
            }
            resident_cells += 1;
            let manifest = self.require_manifest_cell(*cell)?;
            for (index, facet) in ResidencyFacet::ALL.into_iter().enumerate() {
                if generation.resident.contains(facet) {
                    resident_bytes[index] = resident_bytes[index]
                        .checked_add(facet_bytes(manifest, facet)?)
                        .ok_or(Error::NumericOverflow)?;
                }
            }
        }
        Ok(VegetationResidencyReport {
            source_count: self.residency.sources().len(),
            requested_cells: snapshots.len(),
            resident_cells,
            requested_bytes,
            resident_bytes,
            budgets: ResidencyFacet::ALL.map(|facet| self.budgets.get(facet)),
            pending: self.pending_loads_from(&snapshots)?,
        })
    }

    fn advance_residency_revision(&mut self) -> Result<()> {
        self.residency_revision = self
            .residency_revision
            .checked_add(1)
            .ok_or(Error::NumericOverflow)?;
        Ok(())
    }

    fn invalidate_cell_loads(&self) -> Result<()> {
        for entry in self.cells.values() {
            entry.slot.begin(self.residency_revision)?;
        }
        Ok(())
    }

    fn requested_bytes(
        &self,
        snapshots: &[ResidencySnapshot],
    ) -> Result<[u64; saffron_spatial::FACET_COUNT]> {
        let mut bytes = [0_u64; saffron_spatial::FACET_COUNT];
        for snapshot in snapshots {
            let Some(manifest) = self.manifest_cells.get(&snapshot.cell) else {
                continue;
            };
            for (index, facet) in ResidencyFacet::ALL.into_iter().enumerate() {
                if snapshot.reference_counts[index] != 0 {
                    bytes[index] = bytes[index]
                        .checked_add(facet_bytes(manifest, facet)?)
                        .ok_or(Error::NumericOverflow)?;
                }
            }
        }
        Ok(bytes)
    }

    fn pending_loads_from(
        &self,
        snapshots: &[ResidencySnapshot],
    ) -> Result<Vec<VegetationCellLoadRequest>> {
        let admitted = self.admitted_masks(snapshots)?;
        Ok(snapshots
            .iter()
            .filter_map(|snapshot| {
                let desired = admitted
                    .get(&snapshot.cell)
                    .copied()
                    .unwrap_or(ResidencyMask::NONE);
                let resident = self
                    .cells
                    .get(&snapshot.cell)
                    .map(|entry| entry.slot.read().resident)
                    .unwrap_or(ResidencyMask::NONE);
                let missing = subtract_masks(desired, resident);
                (missing != ResidencyMask::NONE).then_some(VegetationCellLoadRequest {
                    cell: snapshot.cell,
                    facets: missing,
                    priority: snapshot.priority,
                    source_revision: self.residency_revision,
                })
            })
            .collect())
    }

    fn admitted_masks(
        &self,
        snapshots: &[ResidencySnapshot],
    ) -> Result<BTreeMap<WorldCellKey, ResidencyMask>> {
        let mut ordered = snapshots.iter().collect::<Vec<_>>();
        ordered.sort_unstable_by(|left, right| left.admission_order(right));
        let mut admitted = BTreeMap::new();
        for (index, facet) in ResidencyFacet::ALL.into_iter().enumerate() {
            let mut used = 0_u64;
            let budget = self.budgets.get(facet);
            for snapshot in &ordered {
                if snapshot.reference_counts[index] == 0 {
                    continue;
                }
                let Some(manifest) = self.manifest_cells.get(&snapshot.cell) else {
                    continue;
                };
                let bytes = facet_bytes(manifest, facet)?;
                let Some(next) = used.checked_add(bytes) else {
                    continue;
                };
                if next > budget {
                    continue;
                }
                used = next;
                admitted
                    .entry(snapshot.cell)
                    .and_modify(|mask: &mut ResidencyMask| *mask = mask.with(facet))
                    .or_insert(ResidencyMask::one(facet));
            }
        }
        Ok(admitted)
    }

    fn unload_unrequested_facets(&mut self) -> Result<()> {
        let snapshots = self.residency.snapshots()?;
        let requested = self.admitted_masks(&snapshots)?;
        let cells = self.cells.keys().copied().collect::<Vec<_>>();
        for cell in cells {
            let desired = requested.get(&cell).copied().unwrap_or(ResidencyMask::NONE);
            let current = self.cells[&cell].slot.read();
            let retained = intersect_masks(current.resident, desired);
            if retained == current.resident {
                continue;
            }
            let token = self.cells[&cell].slot.begin(self.residency_revision)?;
            let mut facets = current.facets.clone();
            facets.retain(|kind, _| required_sections(retained).contains(kind));
            let base = macro_columns_optional(&facets);
            let state = self.effective.cells().get(&cell);
            let points = match base {
                Some(base) => effective_macro_points(base, state)?,
                None => PlantPointColumns::default(),
            };
            let disturbance_masks = state
                .map(|state| state.disturbance_masks.clone())
                .unwrap_or_default();
            let generation = Arc::new(VegetationCellGeneration::build(
                VegetationCellGenerationId {
                    cell,
                    generation: token.generation,
                },
                self.manifest_identity,
                retained,
                facets,
                points,
                self.family_tags.clone(),
                disturbance_masks,
            )?);
            let published = self.cells[&cell].slot.try_publish(token, generation)?;
            debug_assert!(published);
            self.bump_ecology_ground_revision();
        }
        Ok(())
    }
}

fn facet_bytes(cell: &VegetationManifestCell, facet: ResidencyFacet) -> Result<u64> {
    let required = required_sections(ResidencyMask::one(facet));
    cell.sections
        .iter()
        .filter(|section| required.contains(&section.kind))
        .try_fold(0_u64, |total, section| {
            total
                .checked_add(section.decoded_size)
                .ok_or(Error::NumericOverflow)
        })
}

pub(super) fn union_masks(left: ResidencyMask, right: ResidencyMask) -> ResidencyMask {
    right.iter().fold(left, ResidencyMask::with)
}

fn intersect_masks(left: ResidencyMask, right: ResidencyMask) -> ResidencyMask {
    left.iter()
        .filter(|facet| right.contains(*facet))
        .fold(ResidencyMask::NONE, ResidencyMask::with)
}

fn subtract_masks(left: ResidencyMask, right: ResidencyMask) -> ResidencyMask {
    left.iter()
        .filter(|facet| !right.contains(*facet))
        .fold(ResidencyMask::NONE, ResidencyMask::with)
}
