//! Persistent state, the prediction overlay, and bulk suppression of promoted plants.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use saffron_spatial::{ResidencyMask, WorldCellKey};

use crate::{
    Error, PlantId, PlantPointColumns, Result, SaveStateEnvelope, VegetationCellFacet,
    VegetationCellSectionKind, VegetationMutation, VegetationMutationRecord, VegetationState,
    VegetationStateBinding, reduce_mutations,
};

use super::VegetationWorld;
use super::generation::{
    CellPersistentOverlay, VegetationCellGeneration, VegetationCellGenerationId,
    effective_macro_points, macro_columns,
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

    /// Highest transport sequence accepted from a network envelope.
    #[must_use]
    pub const fn network_sequence(&self) -> u64 {
        self.network_sequence
    }

    /// Accepts one sequenced network envelope, reporting whether it advanced the stream.
    ///
    /// A carried authoritative snapshot replaces persistent state first — that is how a receiver
    /// joins, and how one too far behind is corrected — and the sequenced operations then reduce
    /// through the same confirmed path an authored mutation takes. A sequence at or below the last
    /// accepted one is a retransmission and is dropped without touching the world.
    ///
    /// # Errors
    ///
    /// [`Error::ManifestMismatch`] when the sender and receiver disagree on the immutable base,
    /// [`Error::Network`] when an operation falls outside the seated interest declaration, and
    /// any reducer error the operations raise.
    pub fn receive_network_envelope(
        &mut self,
        envelope: &crate::NetworkMutationEnvelope,
    ) -> Result<bool> {
        if envelope.manifest_identity != self.manifest_identity.bytes() {
            return Err(Error::ManifestMismatch);
        }
        if let Some(interest) = &self.network_interest
            && let Some(record) = envelope
                .operations
                .iter()
                .find(|record| interest.facets(record.header.cell) == ResidencyMask::NONE)
        {
            return Err(Error::Network(format!(
                "operation targets cell {}, which this peer declared no interest in",
                record.header.cell
            )));
        }
        if envelope.sequence <= self.network_sequence {
            return Ok(false);
        }
        if let Some(snapshot) = &envelope.snapshot {
            self.replace_persistent_state(snapshot.clone())?;
        }
        self.apply_confirmed_mutations(&envelope.operations)?;
        self.network_sequence = envelope.sequence;
        Ok(true)
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
        self.plant_authority.clear();
        self.authority_epoch = self.authority_epoch.wrapping_add(1);
        self.bump_ecology_planted_revision();
        self.publish_state_rebuilds(staged)
    }

    /// The authority that last claimed simulation ownership of `plant`, absent while no authority
    /// has moved, restored, or removed it since the last wholesale state replacement.
    #[must_use]
    pub fn plant_simulation_authority(&self, plant: PlantId) -> Option<u128> {
        self.plant_authority.get(&plant).copied()
    }

    /// How many times persistent state has been replaced wholesale. A holder of a live
    /// representation compares the epoch it took the representation at against this one: a move
    /// means the world it observed was superseded by another authority's answer.
    #[must_use]
    pub const fn authority_epoch(&self) -> u64 {
        self.authority_epoch
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
        // Only a committed transaction moves ownership; a replay claims nothing it did not
        // already own, and a prediction is transient.
        let committed = reduction
            .committed_transactions
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        for record in records
            .iter()
            .filter(|record| committed.contains(&record.header.transaction))
        {
            if let Some(plant) = claims_simulation_ownership(&record.mutation) {
                self.plant_authority.insert(plant, record.header.authority);
            }
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

    /// Suppresses `plant`'s bulk representation for `authority`, which becomes the plant's
    /// simulation owner until another authority claims it.
    pub fn promote_plant(&mut self, plant: PlantId, authority: u128) -> Result<WorldCellKey> {
        let cell = self.owner_cell(plant)?;
        if !self.bulk_suppressed.insert(plant) {
            return Err(Error::Mutation(format!(
                "plant {plant} is already promoted"
            )));
        }
        self.plant_authority.insert(plant, authority);
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
            overlay: CellPersistentOverlay,
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
                overlay: CellPersistentOverlay::from_state(state.cells().get(cell)),
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
                seed.overlay,
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

/// The plant a mutation takes simulation ownership of, if it takes any. Where the plant *is*,
/// whether it exists at all, and what its whole delta reads are the authority's answers; how
/// healthy or wet it is is not.
fn claims_simulation_ownership(mutation: &VegetationMutation) -> Option<PlantId> {
    match mutation {
        VegetationMutation::TransformOverride { plant, .. }
        | VegetationMutation::PromotionOriginState { plant, .. }
        | VegetationMutation::PlantDeltaRestore { plant, .. }
        | VegetationMutation::Tombstone { plant }
        | VegetationMutation::Regrow { plant, .. } => Some(*plant),
        // Removal is an existence claim; every other lifecycle step is biology.
        VegetationMutation::LifecycleTransition { plant, to, .. } => {
            (*to == crate::PlantLifecycle::Removed).then_some(*plant)
        }
        VegetationMutation::AnchorAddition(point) | VegetationMutation::Planting(point) => {
            Some(point.id)
        }
        VegetationMutation::StateOverride { .. }
        | VegetationMutation::Damage { .. }
        | VegetationMutation::MoistureFuel { .. }
        | VegetationMutation::Harvest { .. }
        | VegetationMutation::Burn { .. }
        | VegetationMutation::Ignite { .. }
        | VegetationMutation::Extinguish { .. }
        | VegetationMutation::FieldTilePatch { .. }
        | VegetationMutation::FieldTileClear { .. }
        | VegetationMutation::DisturbanceMask { .. }
        | VegetationMutation::DisturbanceMaskClear { .. } => None,
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
