//! Persistent state, the prediction overlay, and bulk suppression of promoted plants.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use saffron_spatial::{ResidencyMask, WorldCellKey};

use crate::{
    DisturbanceTileKey, Error, PlantId, PlantPointColumns, Result, SaveStateEnvelope,
    VegetationCellFacet, VegetationCellSectionKind, VegetationMutationRecord, VegetationState,
    VegetationStateBinding, reduce_mutations,
};

use super::VegetationWorld;
use super::generation::{
    VegetationCellGeneration, VegetationCellGenerationId, effective_macro_points, macro_columns,
};
use super::load::StagedVegetationCellGeneration;

impl VegetationWorld {
    /// Canonical reduced persistent state.
    #[must_use]
    pub fn persistent_state(&self) -> &VegetationState {
        &self.persistent
    }

    /// Exact deterministic compatibility binding for save and network containers.
    pub fn state_binding(&self) -> Result<VegetationStateBinding> {
        VegetationStateBinding::from_manifest(&self.manifest)
    }

    /// Exports the current reduced state through the one strict snapshot container.
    pub fn export_state_snapshot(&self) -> Result<Vec<u8>> {
        SaveStateEnvelope {
            binding: self.state_binding()?,
            snapshot: self.persistent.clone(),
            tail: Vec::new(),
        }
        .canonical_bytes()
    }

    /// Imports, verifies, reduces, and atomically republishes one exact snapshot container.
    pub fn import_state_snapshot(&mut self, bytes: &[u8]) -> Result<()> {
        let envelope = SaveStateEnvelope::from_canonical_bytes(bytes, self.state_binding()?)?;
        self.replace_persistent_state(envelope.reduced_state()?)
    }

    /// Replaces the persistent state only when it matches the exact base generation.
    pub fn replace_persistent_state(&mut self, state: VegetationState) -> Result<()> {
        if state.manifest_identity() != self.manifest_identity.bytes() {
            return Err(Error::ManifestMismatch);
        }
        let cells = self.cells.keys().copied().collect::<Vec<_>>();
        let staged = self.stage_state_rebuilds(&state, &cells)?;
        self.persistent = state.clone();
        self.effective = state;
        self.predictions.clear();
        self.bump_ecology_planted_revision();
        self.publish_state_rebuilds(staged)
    }

    pub fn apply_confirmed_mutations(
        &mut self,
        records: &[VegetationMutationRecord],
    ) -> Result<crate::MutationReduction> {
        let mut candidate = self.persistent.clone();
        let reduction = reduce_mutations(&mut candidate, self.manifest_identity.bytes(), records)?;
        let confirmed = records
            .iter()
            .map(|record| record.header.transaction)
            .collect::<BTreeSet<_>>();
        let mut predictions = self.predictions.clone();
        for transaction in &confirmed {
            predictions.remove(transaction);
        }
        let effective = effective_prediction_state(&candidate, &predictions)?;
        let changed_cells = reduction
            .changed_cells
            .iter()
            .copied()
            .chain(
                self.predictions
                    .iter()
                    .filter(|(transaction, _)| confirmed.contains(transaction))
                    .flat_map(|(_, records)| records.iter().map(|record| record.header.cell)),
            )
            .chain(
                predictions
                    .values()
                    .flatten()
                    .map(|record| record.header.cell),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        // A planting that landed in ground the cook left empty adds a cell no generation covers, so
        // it republishes nothing yet redraws the dependency-region partition.
        let planted_cells_moved = candidate.cells().keys().ne(self.persistent.cells().keys());
        let staged = self.stage_state_rebuilds(&effective, &changed_cells)?;
        self.persistent = candidate;
        self.effective = effective;
        self.predictions = predictions;
        if planted_cells_moved {
            self.bump_ecology_planted_revision();
        }
        self.publish_state_rebuilds(staged)?;
        // A confirmed mutation changes what a cell renders as — a phenotype override, a
        // lifecycle change, a transform — so every adapter caching beside the generation id
        // must re-derive the cell, exactly as a promotion does.
        for cell in &changed_cells {
            self.bump_bulk_revision(*cell);
        }
        // Only a confirmed commit is observable. A prediction is transient and a replay is a
        // no-op, so neither reaches the ring — every consumer sees each transition exactly once.
        self.record_transitions(&reduction.transitions);
        Ok(reduction)
    }

    /// Applies transient predicted mutations above confirmed persistent state.
    pub fn apply_prediction(
        &mut self,
        records: &[VegetationMutationRecord],
    ) -> Result<crate::MutationReduction> {
        let mut effective = self.effective.clone();
        let reduction = reduce_mutations(&mut effective, self.manifest_identity.bytes(), records)?;
        let committed = reduction
            .committed_transactions
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let mut predictions = self.predictions.clone();
        for record in records {
            if committed.contains(&record.header.transaction) {
                predictions
                    .entry(record.header.transaction)
                    .or_default()
                    .push(record.clone());
            }
        }
        let staged = self.stage_state_rebuilds(&effective, &reduction.changed_cells)?;
        self.effective = effective;
        self.predictions = predictions;
        self.publish_state_rebuilds(staged)?;
        Ok(reduction)
    }

    /// Confirms one predicted transaction through the authoritative persistent reducer.
    pub fn confirm_prediction(&mut self, transaction: u128) -> Result<crate::MutationReduction> {
        let records = self.predictions.get(&transaction).cloned().ok_or_else(|| {
            Error::Mutation(format!(
                "prediction transaction {transaction} is not pending"
            ))
        })?;
        self.apply_confirmed_mutations(&records)
    }

    /// Rejects one prediction and republishes the remaining transient overlay.
    pub fn reject_prediction(&mut self, transaction: u128) -> Result<bool> {
        let Some(records) = self.predictions.get(&transaction).cloned() else {
            return Ok(false);
        };
        let mut predictions = self.predictions.clone();
        predictions.remove(&transaction);
        let effective = effective_prediction_state(&self.persistent, &predictions)?;
        let changed_cells = records
            .iter()
            .map(|record| record.header.cell)
            .chain(
                predictions
                    .values()
                    .flatten()
                    .map(|record| record.header.cell),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let staged = self.stage_state_rebuilds(&effective, &changed_cells)?;
        self.effective = effective;
        self.predictions = predictions;
        self.publish_state_rebuilds(staged)?;
        Ok(true)
    }

    /// Number of pending transient prediction transactions.
    #[must_use]
    pub fn prediction_count(&self) -> usize {
        self.predictions.len()
    }

    pub fn promote_plant(&mut self, plant: PlantId) -> Result<WorldCellKey> {
        let cell = self.owner_cell(plant)?;
        if !self.bulk_suppressed.insert(plant) {
            return Err(Error::Mutation(format!(
                "plant {plant} is already promoted"
            )));
        }
        self.bump_bulk_revision(cell);
        Ok(cell)
    }

    /// Restores `plant`'s bulk representation. Returns the plant's owner cell when it is still
    /// resident, and `None` when the cell has since unloaded (the suppression is cleared either
    /// way, so a later reload publishes the plant normally).
    ///
    /// # Errors
    ///
    /// [`Error::Mutation`] when the plant was not suppressed.
    pub fn demote_plant(&mut self, plant: PlantId) -> Result<Option<WorldCellKey>> {
        if !self.bulk_suppressed.remove(&plant) {
            return Err(Error::Mutation(format!("plant {plant} is not promoted")));
        }
        let cell = self.owner_cell(plant).ok();
        if let Some(cell) = cell {
            self.bump_bulk_revision(cell);
        }
        Ok(cell)
    }

    /// Whether a promoted entity owns `plant`, so its bulk representation is suppressed.
    #[must_use]
    pub fn is_bulk_suppressed(&self, plant: PlantId) -> bool {
        self.bulk_suppressed.contains(&plant)
    }

    /// Every bulk-suppressed plant, in canonical identity order.
    pub fn bulk_suppressed(&self) -> impl Iterator<Item = PlantId> + '_ {
        self.bulk_suppressed.iter().copied()
    }

    /// The cell's bulk-suppression revision. A render or collision adapter caches it beside the
    /// published generation id and re-derives the cell whenever either changes.
    #[must_use]
    pub fn cell_bulk_revision(&self, cell: WorldCellKey) -> u64 {
        self.cells.get(&cell).map_or(0, |entry| entry.bulk_revision)
    }

    fn owner_cell(&self, plant: PlantId) -> Result<WorldCellKey> {
        self.cells
            .iter()
            .find(|(_, entry)| entry.slot.read().slots.contains_key(&plant))
            .map(|(cell, _)| *cell)
            .ok_or(Error::PlantNotResident {
                plant: plant.to_string(),
            })
    }

    fn bump_bulk_revision(&mut self, cell: WorldCellKey) {
        if let Some(entry) = self.cells.get_mut(&cell) {
            entry.bulk_revision = entry.bulk_revision.wrapping_add(1);
        }
    }

    fn stage_state_rebuilds(
        &self,
        state: &VegetationState,
        cells: &[WorldCellKey],
    ) -> Result<Vec<StagedVegetationCellGeneration>> {
        struct Seed {
            cell: WorldCellKey,
            resident: ResidencyMask,
            facets: BTreeMap<VegetationCellSectionKind, VegetationCellFacet>,
            points: PlantPointColumns,
            disturbance_masks: BTreeMap<DisturbanceTileKey, Vec<i16>>,
        }

        let mut seeds = Vec::new();
        for cell in cells {
            let Some(entry) = self.cells.get(cell) else {
                continue;
            };
            let current = entry.slot.read();
            if current.resident == ResidencyMask::NONE {
                continue;
            }
            let base = macro_columns(&current.facets)?;
            seeds.push(Seed {
                cell: *cell,
                resident: current.resident,
                facets: current.facets.clone(),
                points: effective_macro_points(base, state.cells().get(cell))?,
                disturbance_masks: state
                    .cells()
                    .get(cell)
                    .map(|state| state.disturbance_masks.clone())
                    .unwrap_or_default(),
            });
        }

        let mut staged = Vec::new();
        for seed in seeds {
            let token = self.cells[&seed.cell].slot.begin(self.residency_revision)?;
            let generation = Arc::new(VegetationCellGeneration::build(
                VegetationCellGenerationId {
                    cell: seed.cell,
                    generation: token.generation,
                },
                self.manifest_identity,
                seed.resident,
                seed.facets,
                seed.points,
                self.family_tags.clone(),
                seed.disturbance_masks,
            )?);
            staged.push(StagedVegetationCellGeneration { token, generation });
        }
        Ok(staged)
    }

    fn publish_state_rebuilds(
        &mut self,
        staged: Vec<StagedVegetationCellGeneration>,
    ) -> Result<()> {
        for staged in staged {
            let published = self.publish_staged(staged)?;
            debug_assert!(published);
        }
        Ok(())
    }
}

fn effective_prediction_state(
    persistent: &VegetationState,
    predictions: &BTreeMap<u128, Vec<VegetationMutationRecord>>,
) -> Result<VegetationState> {
    let mut effective = persistent.clone();
    let records = predictions.values().flatten().cloned().collect::<Vec<_>>();
    reduce_mutations(&mut effective, persistent.manifest_identity(), &records)?;
    Ok(effective)
}
