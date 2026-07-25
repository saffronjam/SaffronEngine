//! Deterministic persistent vegetation mutations and their distinct transport envelopes.

use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::{DecisionScalar, FieldChannel, UnitInterval, WorldCellKey, WorldPosition};

use crate::hash::sha256;
use crate::{
    ContentHash, CookVersionSet, Error, InteractionPolicy, PlantId, PlantLifecycle, PlantPoint,
    PlantPointColumns, QuantizedOrientation, Result,
};

/// Common ordering, authority, idempotency, and optimistic-concurrency metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MutationHeader {
    /// Canonical cell changed by this record.
    pub cell: WorldCellKey,
    /// Atomic transaction shared by all participating cells.
    pub transaction: u128,
    /// Stable editor, save, server, or simulation authority.
    pub authority: u128,
    /// Authority-issued logical ordering tick.
    pub logical_tick: u64,
    /// Unique replay key for this operation.
    pub idempotency_key: u128,
    /// Required starting revision of this cell when optimistic concurrency is used.
    pub base_revision: Option<u64>,
}

/// Exact promotion state returned to the macro plant after high-fidelity simulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromotionOriginState {
    /// Exact position at demotion.
    pub position: WorldPosition,
    /// Quantized orientation at demotion.
    pub orientation: QuantizedOrientation,
    /// Quantized scale at demotion.
    pub scale: [DecisionScalar; 3],
    /// Linear velocity in Q15.16 metres per tick.
    pub linear_velocity: [DecisionScalar; 3],
    /// Angular velocity in Q15.16 turns per tick.
    pub angular_velocity: [DecisionScalar; 3],
}

/// One deterministic mutation understood by every persistence/transport envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VegetationMutation {
    /// Replace one quantized authored or simulated field tile.
    FieldTilePatch {
        /// Stable owning layer.
        layer: u128,
        /// Shared field vocabulary.
        channel: FieldChannel,
        /// Stable tile identity within the cell.
        tile: u128,
        /// Packed tile dimensions.
        dimensions: [u32; 3],
        /// Fixed quantization step.
        quantum_bits: i32,
        /// Canonical signed values.
        values: Vec<i32>,
    },
    /// Add an explicit authored anchor.
    AnchorAddition(PlantPoint),
    /// Persistently remove a cooked, authored, or runtime plant.
    Tombstone {
        /// Removed plant.
        plant: PlantId,
    },
    /// Persist an exact transform override without editing cooked base bytes.
    TransformOverride {
        /// Target plant.
        plant: PlantId,
        /// Exact position.
        position: WorldPosition,
        /// Quantized orientation.
        orientation: QuantizedOrientation,
        /// Fixed scale.
        scale: [DecisionScalar; 3],
    },
    /// Replace selected persistent biological/interaction values.
    StateOverride {
        /// Target plant.
        plant: PlantId,
        /// Optional lifecycle replacement.
        lifecycle: Option<PlantLifecycle>,
        /// Optional phenotype replacement.
        phenotype: Option<u32>,
        /// Optional health replacement.
        health: Option<UnitInterval>,
        /// Optional moisture replacement.
        moisture: Option<UnitInterval>,
        /// Optional fuel replacement.
        fuel: Option<UnitInterval>,
        /// Optional interaction-policy replacement.
        interaction_policy: Option<InteractionPolicy>,
    },
    /// Add a runtime-authority plant.
    Planting(PlantPoint),
    /// Apply health damage and an optional species phenotype.
    Damage {
        /// Target plant.
        plant: PlantId,
        /// Closed normalized damage amount.
        amount: UnitInterval,
        /// Species-declared damaged phenotype.
        phenotype: Option<u32>,
    },
    /// Replace persistent water/fuel state independently from current weather appearance.
    MoistureFuel {
        /// Target plant.
        plant: PlantId,
        /// Persistent moisture.
        moisture: UnitInterval,
        /// Persistent combustible fuel.
        fuel: UnitInterval,
    },
    /// Advance or explicitly replace biological lifecycle state.
    LifecycleTransition {
        /// Target plant.
        plant: PlantId,
        /// Required prior state when known.
        from: Option<PlantLifecycle>,
        /// New lifecycle state.
        to: PlantLifecycle,
        /// Monotonic biological tick; unrelated calendar movement never changes it.
        ecology_tick: u64,
    },
    /// Mark a plant harvested with a species-declared phenotype.
    Harvest {
        /// Target plant.
        plant: PlantId,
        /// Harvested phenotype.
        phenotype: u32,
    },
    /// Mark a plant burned and update its fuel/health.
    Burn {
        /// Target plant.
        plant: PlantId,
        /// Burned phenotype.
        phenotype: u32,
        /// Fuel remaining after the event.
        remaining_fuel: UnitInterval,
    },
    /// Set a plant alight. Vegetation persists that it is burning and keeps its fuel; heat
    /// propagation and smoke belong to a fire system.
    Ignite {
        /// Target plant.
        plant: PlantId,
    },
    /// Put a burning plant out, leaving whatever fuel and damage the fire left behind.
    Extinguish {
        /// Target plant.
        plant: PlantId,
    },
    /// Clear removal and start a new biological lifecycle for the same stable identity.
    Regrow {
        /// Target plant.
        plant: PlantId,
        /// Regrown lifecycle state.
        lifecycle: PlantLifecycle,
        /// Regrown phenotype.
        phenotype: u32,
        /// New biological tick.
        ecology_tick: u64,
    },
    /// Persist authoritative state produced while a macro plant was promoted.
    PromotionOriginState {
        /// Target plant.
        plant: PlantId,
        /// Returned authoritative state.
        state: PromotionOriginState,
    },
    /// Replace one signed disturbance-mask tile.
    DisturbanceMask {
        /// Stable disturbance class/category bits.
        categories: u32,
        /// Stable tile identity.
        tile: u128,
        /// Packed signed values.
        values: Vec<i16>,
    },
}

/// What one committed mutation did to the world, in gameplay terms rather than storage terms.
///
/// Scripts, VFX, audio, quests, fire, and navigation all consume the same typed transition, so a
/// reducer commit is the single place a vegetation change becomes observable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VegetationTransitionKind {
    /// A plant took damage; `health` is the value it settled at.
    Damaged {
        /// Damage applied.
        amount: UnitInterval,
        /// Resulting persistent health.
        health: UnitInterval,
    },
    /// A plant was harvested into its harvested phenotype.
    Harvested {
        /// Harvested phenotype.
        phenotype: u32,
    },
    /// A plant burned; `remaining_fuel` is what is left to burn.
    Burned {
        /// Burned phenotype.
        phenotype: u32,
        /// Fuel remaining after the event.
        remaining_fuel: UnitInterval,
    },
    /// A plant was removed from every downstream projection.
    Removed,
    /// A plant was added by an authority (a runtime planting or an authored anchor).
    Planted,
    /// A removed plant started a new biological lifecycle under the same identity.
    Regrew {
        /// Regrown lifecycle state.
        lifecycle: PlantLifecycle,
        /// Regrown phenotype.
        phenotype: u32,
    },
    /// A plant's biological lifecycle advanced or was replaced.
    LifecycleChanged {
        /// Required prior state when the mutation declared one.
        from: Option<PlantLifecycle>,
        /// New lifecycle state.
        to: PlantLifecycle,
    },
    /// A plant was set alight.
    Ignited,
    /// A burning plant was put out.
    Extinguished,
    /// A plant's persistent water and combustible fuel changed.
    Wetted {
        /// Persistent moisture.
        moisture: UnitInterval,
        /// Persistent fuel.
        fuel: UnitInterval,
    },
    /// A plant's biological or interaction values were replaced wholesale.
    StateReplaced,
    /// A plant's exact transform changed (an authored override, or a promoted view's write-back).
    Moved,
    /// A signed disturbance-mask tile changed: trample and crush truth, never cosmetic bend.
    Disturbed {
        /// Disturbance class bits.
        categories: u32,
    },
}

/// One typed transition a committed mutation produced, addressed by cell and (where the mutation
/// names one) by plant. Emitted once per committed record; an idempotent replay emits nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationTransition {
    /// The transaction that committed it.
    pub transaction: u128,
    /// The cell whose persistent state changed.
    pub cell: WorldCellKey,
    /// The plant it names, absent for a cell-wide change (a field or disturbance tile).
    pub plant: Option<PlantId>,
    /// What happened.
    pub kind: VegetationTransitionKind,
}

/// One mutation with its required common metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMutationRecord {
    /// Ordering and authority header.
    pub header: MutationHeader,
    /// Typed operation.
    pub mutation: VegetationMutation,
}

/// Canonical field-tile address in persistent state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FieldTileKey {
    /// Stable layer.
    pub layer: u128,
    /// Shared field channel.
    pub channel: FieldChannel,
    /// Stable tile identity.
    pub tile: u128,
}

/// Quantized field-tile state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldTileState {
    /// Packed dimensions.
    pub dimensions: [u32; 3],
    /// Quantization step.
    pub quantum_bits: i32,
    /// Canonical values.
    pub values: Vec<i32>,
}

/// Persistent delta layered over a cooked or explicitly added plant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlantPersistentState {
    /// Added authored/runtime point; absent for a delta targeting cooked base.
    pub addition: Option<PlantPoint>,
    /// Persistently removed from all downstream projections.
    pub tombstoned: bool,
    /// Exact persistent transform replacement.
    pub transform: Option<(WorldPosition, QuantizedOrientation, [DecisionScalar; 3])>,
    /// Persistent lifecycle replacement.
    pub lifecycle: Option<PlantLifecycle>,
    /// Species phenotype replacement.
    pub phenotype: Option<u32>,
    /// Monotonic biological age tick.
    pub ecology_tick: Option<u64>,
    /// Persistent health.
    pub health: Option<UnitInterval>,
    /// Persistent moisture baseline.
    pub moisture: Option<UnitInterval>,
    /// Persistent fuel baseline.
    pub fuel: Option<UnitInterval>,
    /// Gameplay interaction-policy replacement.
    pub interaction_policy: Option<InteractionPolicy>,
    /// Whether the plant is alight.
    pub ignited: bool,
    /// State returned by a promoted simulation representation.
    pub promotion_origin: Option<PromotionOriginState>,
}

/// Persistent disturbance tile address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DisturbanceTileKey {
    /// Disturbance category bits.
    pub categories: u32,
    /// Stable tile identity.
    pub tile: u128,
}

/// Canonical persistent delta for one world cell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VegetationCellState {
    /// Monotonic transaction revision.
    pub revision: u64,
    /// Quantized field truth.
    pub field_tiles: BTreeMap<FieldTileKey, FieldTileState>,
    /// Plant additions and deltas.
    pub plants: BTreeMap<PlantId, PlantPersistentState>,
    /// Persistent disturbance masks.
    pub disturbance_masks: BTreeMap<DisturbanceTileKey, Vec<i16>>,
}

/// Complete persistent vegetation state bound to one immutable cooked-base manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationState {
    manifest_identity: [u8; 32],
    cells: BTreeMap<WorldCellKey, VegetationCellState>,
    applied_transactions: BTreeMap<u128, [u8; 32]>,
    /// How far biology has advanced, under which rule set, with the boundary summaries a
    /// dependency-region catch-up reads.
    ecology: crate::EcologyState,
}

impl VegetationState {
    /// Creates empty persistent state for an exact base manifest.
    #[must_use]
    pub fn new(manifest_identity: [u8; 32]) -> Self {
        Self {
            manifest_identity,
            cells: BTreeMap::new(),
            applied_transactions: BTreeMap::new(),
            ecology: crate::EcologyState::new(),
        }
    }

    /// Exact immutable base-manifest identity.
    #[must_use]
    pub const fn manifest_identity(&self) -> [u8; 32] {
        self.manifest_identity
    }

    /// Cell states in canonical key order.
    #[must_use]
    pub fn cells(&self) -> &BTreeMap<WorldCellKey, VegetationCellState> {
        &self.cells
    }

    pub(crate) fn applied_transactions(&self) -> &BTreeMap<u128, [u8; 32]> {
        &self.applied_transactions
    }

    pub(crate) fn from_canonical_parts(
        manifest_identity: [u8; 32],
        cells: BTreeMap<WorldCellKey, VegetationCellState>,
        applied_transactions: BTreeMap<u128, [u8; 32]>,
        ecology: crate::EcologyState,
    ) -> Self {
        Self {
            manifest_identity,
            cells,
            applied_transactions,
            ecology,
        }
    }

    /// The persisted ecology state.
    #[must_use]
    pub const fn ecology(&self) -> &crate::EcologyState {
        &self.ecology
    }

    /// The persisted ecology state, for the simulation that advances it.
    pub const fn ecology_mut(&mut self) -> &mut crate::EcologyState {
        &mut self.ecology
    }

    /// Writes the canonical, interruption-detecting snapshot container.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        crate::state_codec::encode_state(self)
    }
}

/// Result of applying a mutation batch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MutationReduction {
    /// Transactions newly committed in deterministic order.
    pub committed_transactions: Vec<u128>,
    /// Exact replays ignored after signature verification.
    pub replayed_transactions: Vec<u128>,
    /// Cells published by newly committed transactions, in canonical order.
    pub changed_cells: Vec<WorldCellKey>,
    /// Typed transitions the committed records produced, in commit order.
    pub transitions: Vec<VegetationTransition>,
}

/// Applies one batch through the only persistent vegetation reducer.
pub fn reduce_mutations(
    state: &mut VegetationState,
    manifest_identity: [u8; 32],
    records: &[VegetationMutationRecord],
) -> Result<MutationReduction> {
    if state.manifest_identity != manifest_identity {
        return Err(Error::ManifestMismatch);
    }

    let mut transactions: BTreeMap<u128, Vec<&VegetationMutationRecord>> = BTreeMap::new();
    for record in records {
        if record.header.transaction == 0
            || record.header.authority == 0
            || record.header.idempotency_key == 0
        {
            return Err(Error::Mutation(
                "transaction, authority, and idempotency keys must be non-zero".to_owned(),
            ));
        }
        transactions
            .entry(record.header.transaction)
            .or_default()
            .push(record);
    }

    let mut ordered = Vec::with_capacity(transactions.len());
    for (transaction, transaction_records) in transactions {
        let mut deduplicated = BTreeMap::new();
        for record in transaction_records {
            if let Some(existing) = deduplicated.insert(record.header.idempotency_key, record)
                && existing != record
            {
                return Err(Error::Mutation(
                    "idempotency key was reused with different operation contents".to_owned(),
                ));
            }
        }
        let mut transaction_records: Vec<_> = deduplicated.into_values().collect();
        transaction_records.sort_by_key(|record| record_order_key(record));
        validate_transaction(&transaction_records)?;
        let signature = transaction_signature(&transaction_records)?;
        let first = transaction_records[0].header;
        ordered.push((
            (first.logical_tick, first.authority, transaction),
            transaction,
            signature,
            transaction_records,
        ));
    }
    ordered.sort_by_key(|entry| entry.0);

    let mut reduction = MutationReduction::default();
    let mut changed_cells = BTreeSet::new();
    for (_, transaction, signature, transaction_records) in ordered {
        if let Some(applied_signature) = state.applied_transactions.get(&transaction) {
            if applied_signature != &signature {
                return Err(Error::Mutation(
                    "transaction ID was reused with different canonical contents".to_owned(),
                ));
            }
            reduction.replayed_transactions.push(transaction);
            continue;
        }

        let mut candidate = state.clone();
        let touched: BTreeSet<_> = transaction_records
            .iter()
            .map(|record| record.header.cell)
            .collect();
        for cell in &touched {
            let expected = transaction_records
                .iter()
                .find(|record| record.header.cell == *cell)
                .and_then(|record| record.header.base_revision);
            let revision = candidate.cells.get(cell).map_or(0, |value| value.revision);
            if expected.is_some_and(|expected| expected != revision) {
                return Err(Error::Mutation(format!(
                    "cell precondition revision {expected:?} does not match {revision}"
                )));
            }
        }
        for record in &transaction_records {
            apply_mutation(&mut candidate, record)?;
            // One transition per committed record, in commit order; a replayed transaction
            // returns above without reaching this loop, so an exact replay emits nothing.
            if let Some(transition) = transition_for(&candidate, record) {
                reduction.transitions.push(transition);
            }
        }
        for cell in &touched {
            let state = candidate.cells.entry(*cell).or_default();
            state.revision = state
                .revision
                .checked_add(1)
                .ok_or(Error::NumericOverflow)?;
        }
        candidate
            .applied_transactions
            .insert(transaction, signature);
        *state = candidate;
        reduction.committed_transactions.push(transaction);
        changed_cells.extend(touched);
    }
    reduction.changed_cells = changed_cells.into_iter().collect();
    Ok(reduction)
}

/// Editor-only journal envelope retaining gesture grouping and exact inverse/preimage operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorJournalEnvelope {
    /// Editor gesture identity.
    pub gesture: u128,
    /// Forward records passed to the reducer.
    pub forward: Vec<VegetationMutationRecord>,
    /// Inverse records computed from captured preimages at gesture creation.
    pub inverse: Vec<VegetationMutationRecord>,
}

/// Exact compatibility identity required by a persistent vegetation state container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationStateBinding {
    /// Exact immutable world-manifest identity.
    pub manifest_identity: ContentHash,
    /// Exact canonical graph compiled into the manifest.
    pub cook_graph_identity: ContentHash,
    /// Complete schema/compiler/evaluator/numeric/simulation contract.
    pub versions: CookVersionSet,
    /// Canonical identity of every named seed namespace.
    pub seed_namespaces_identity: ContentHash,
}

/// Compact save envelope: canonical snapshot plus a bounded mutation tail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveStateEnvelope {
    /// Exact immutable-generation and deterministic-simulation contract.
    pub binding: VegetationStateBinding,
    /// Compact canonical snapshot.
    pub snapshot: VegetationState,
    /// Mutations after the snapshot boundary.
    pub tail: Vec<VegetationMutationRecord>,
}

impl SaveStateEnvelope {
    /// Reduces the tail over a clone of the snapshot.
    pub fn reduced_state(&self) -> Result<VegetationState> {
        let manifest_identity = self.binding.manifest_identity.bytes();
        if self.snapshot.manifest_identity != manifest_identity {
            return Err(Error::ManifestMismatch);
        }
        let mut state = self.snapshot.clone();
        reduce_mutations(&mut state, manifest_identity, &self.tail)?;
        Ok(state)
    }

    /// Reduces the tail and returns a new envelope with an empty tail.
    pub fn compact(self) -> Result<Self> {
        let snapshot = self.reduced_state()?;
        Ok(Self {
            binding: self.binding,
            snapshot,
            tail: Vec::new(),
        })
    }
}

/// Future network envelope with transport sequence separate from simulation logical ticks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkMutationEnvelope {
    /// Monotonic transport sequence.
    pub sequence: u64,
    /// Exact manifest understood by sender and receiver.
    pub manifest_identity: [u8; 32],
    /// Sequenced idempotent operations.
    pub operations: Vec<VegetationMutationRecord>,
    /// Optional authoritative snapshot for joining or correction.
    pub snapshot: Option<VegetationState>,
}

fn validate_transaction(records: &[&VegetationMutationRecord]) -> Result<()> {
    let first = records[0].header;
    let mut idempotency = BTreeSet::new();
    let mut cell_revisions = BTreeMap::new();
    for record in records {
        if record.header.authority != first.authority
            || record.header.logical_tick != first.logical_tick
            || !idempotency.insert(record.header.idempotency_key)
        {
            return Err(Error::Mutation(
                "one transaction must share authority/tick and have unique operation keys"
                    .to_owned(),
            ));
        }
        if let Some(existing) =
            cell_revisions.insert(record.header.cell, record.header.base_revision)
            && existing != record.header.base_revision
        {
            return Err(Error::Mutation(
                "one transaction declared conflicting cell preconditions".to_owned(),
            ));
        }
        validate_cell_ownership(record)?;
    }
    Ok(())
}

fn validate_cell_ownership(record: &VegetationMutationRecord) -> Result<()> {
    match &record.mutation {
        VegetationMutation::AnchorAddition(point) | VegetationMutation::Planting(point) => {
            point.validate()?;
            if point.owner != record.header.cell {
                return Err(Error::Mutation(
                    "added point is not owned by the mutation cell".to_owned(),
                ));
            }
        }
        VegetationMutation::TransformOverride { position, .. }
        | VegetationMutation::PromotionOriginState {
            state: PromotionOriginState { position, .. },
            ..
        } if position.cell() != record.header.cell => {
            return Err(Error::Mutation(
                "transformed point must be recorded in its canonical owner cell".to_owned(),
            ));
        }
        _ => {}
    }
    Ok(())
}

/// The typed transition a just-applied record produced, read from the record plus the state it
/// settled at. `None` for a change with no gameplay-visible transition (a quantized field tile is
/// world truth an adapter re-reads, not an event).
fn transition_for(
    state: &VegetationState,
    record: &VegetationMutationRecord,
) -> Option<VegetationTransition> {
    let cell = record.header.cell;
    let settled = |plant: &PlantId| {
        state
            .cells
            .get(&cell)
            .and_then(|cell| cell.plants.get(plant))
    };
    let (plant, kind) = match &record.mutation {
        VegetationMutation::FieldTilePatch { .. } => return None,
        VegetationMutation::AnchorAddition(point) | VegetationMutation::Planting(point) => {
            (Some(point.id), VegetationTransitionKind::Planted)
        }
        VegetationMutation::Tombstone { plant } => {
            (Some(*plant), VegetationTransitionKind::Removed)
        }
        VegetationMutation::TransformOverride { plant, .. }
        | VegetationMutation::PromotionOriginState { plant, .. } => {
            (Some(*plant), VegetationTransitionKind::Moved)
        }
        VegetationMutation::StateOverride { plant, .. } => {
            (Some(*plant), VegetationTransitionKind::StateReplaced)
        }
        VegetationMutation::Damage { plant, amount, .. } => (
            Some(*plant),
            VegetationTransitionKind::Damaged {
                amount: *amount,
                health: settled(plant)
                    .and_then(|state| state.health)
                    .unwrap_or(UnitInterval::ZERO),
            },
        ),
        VegetationMutation::MoistureFuel {
            plant,
            moisture,
            fuel,
        } => (
            Some(*plant),
            VegetationTransitionKind::Wetted {
                moisture: *moisture,
                fuel: *fuel,
            },
        ),
        VegetationMutation::LifecycleTransition {
            plant, from, to, ..
        } => (
            Some(*plant),
            VegetationTransitionKind::LifecycleChanged {
                from: *from,
                to: *to,
            },
        ),
        VegetationMutation::Harvest { plant, phenotype } => (
            Some(*plant),
            VegetationTransitionKind::Harvested {
                phenotype: *phenotype,
            },
        ),
        VegetationMutation::Burn {
            plant,
            phenotype,
            remaining_fuel,
        } => (
            Some(*plant),
            VegetationTransitionKind::Burned {
                phenotype: *phenotype,
                remaining_fuel: *remaining_fuel,
            },
        ),
        VegetationMutation::Ignite { plant } => (Some(*plant), VegetationTransitionKind::Ignited),
        VegetationMutation::Extinguish { plant } => {
            (Some(*plant), VegetationTransitionKind::Extinguished)
        }
        VegetationMutation::Regrow {
            plant,
            lifecycle,
            phenotype,
            ..
        } => (
            Some(*plant),
            VegetationTransitionKind::Regrew {
                lifecycle: *lifecycle,
                phenotype: *phenotype,
            },
        ),
        VegetationMutation::DisturbanceMask { categories, .. } => (
            None,
            VegetationTransitionKind::Disturbed {
                categories: *categories,
            },
        ),
    };
    Some(VegetationTransition {
        transaction: record.header.transaction,
        cell,
        plant,
        kind,
    })
}

fn apply_mutation(state: &mut VegetationState, record: &VegetationMutationRecord) -> Result<()> {
    let cell = state.cells.entry(record.header.cell).or_default();
    match &record.mutation {
        VegetationMutation::FieldTilePatch {
            layer,
            channel,
            tile,
            dimensions,
            quantum_bits,
            values,
        } => {
            let expected = dimensions
                .iter()
                .try_fold(1_u64, |product, dimension| {
                    product.checked_mul(u64::from(*dimension))
                })
                .ok_or(Error::NumericOverflow)?;
            if expected == 0 || usize::try_from(expected).ok() != Some(values.len()) {
                return Err(Error::Mutation(
                    "field tile dimensions do not match packed values".to_owned(),
                ));
            }
            cell.field_tiles.insert(
                FieldTileKey {
                    layer: *layer,
                    channel: *channel,
                    tile: *tile,
                },
                FieldTileState {
                    dimensions: *dimensions,
                    quantum_bits: *quantum_bits,
                    values: values.clone(),
                },
            );
        }
        VegetationMutation::AnchorAddition(point) => {
            if point.id.namespace()? != crate::PlantIdNamespace::Explicit {
                return Err(Error::Mutation(
                    "authored anchors require the explicit plant-ID namespace".to_owned(),
                ));
            }
            add_point(cell, point)?;
        }
        VegetationMutation::Planting(point) => {
            if point.id.namespace()? != crate::PlantIdNamespace::Runtime {
                return Err(Error::Mutation(
                    "planting requires the runtime plant-ID namespace".to_owned(),
                ));
            }
            add_point(cell, point)?;
        }
        VegetationMutation::Tombstone { plant } => {
            cell.plants.entry(*plant).or_default().tombstoned = true;
        }
        VegetationMutation::TransformOverride {
            plant,
            position,
            orientation,
            scale,
        } => {
            if scale.iter().any(|value| value.bits() <= 0) {
                return Err(Error::Mutation(
                    "transform override scale must be positive".to_owned(),
                ));
            }
            cell.plants.entry(*plant).or_default().transform =
                Some((*position, *orientation, *scale));
        }
        VegetationMutation::StateOverride {
            plant,
            lifecycle,
            phenotype,
            health,
            moisture,
            fuel,
            interaction_policy,
        } => {
            let target = cell.plants.entry(*plant).or_default();
            if let Some(value) = lifecycle {
                target.lifecycle = Some(*value);
            }
            if let Some(value) = phenotype {
                target.phenotype = Some(*value);
            }
            if let Some(value) = health {
                target.health = Some(*value);
            }
            if let Some(value) = moisture {
                target.moisture = Some(*value);
            }
            if let Some(value) = fuel {
                target.fuel = Some(*value);
            }
            if let Some(value) = interaction_policy {
                target.interaction_policy = Some(*value);
            }
        }
        VegetationMutation::Damage {
            plant,
            amount,
            phenotype,
        } => {
            let target = cell.plants.entry(*plant).or_default();
            let current = target.health.unwrap_or(UnitInterval::ONE).bits();
            target.health = Some(UnitInterval::from_bits(
                current.saturating_sub(amount.bits()),
            ));
            if let Some(phenotype) = phenotype {
                target.phenotype = Some(*phenotype);
            }
        }
        VegetationMutation::MoistureFuel {
            plant,
            moisture,
            fuel,
        } => {
            let target = cell.plants.entry(*plant).or_default();
            target.moisture = Some(*moisture);
            target.fuel = Some(*fuel);
        }
        VegetationMutation::LifecycleTransition {
            plant,
            from,
            to,
            ecology_tick,
        } => {
            let target = cell.plants.entry(*plant).or_default();
            let current = target
                .lifecycle
                .or_else(|| target.addition.as_ref().map(|point| point.lifecycle));
            if from.is_some() && current != *from {
                return Err(Error::Mutation(
                    "lifecycle transition precondition did not match".to_owned(),
                ));
            }
            if target.ecology_tick.is_some_and(|tick| *ecology_tick < tick) {
                return Err(Error::Mutation(
                    "ecology tick cannot move backwards".to_owned(),
                ));
            }
            target.lifecycle = Some(*to);
            target.ecology_tick = Some(*ecology_tick);
            target.tombstoned = *to == PlantLifecycle::Removed;
        }
        VegetationMutation::Harvest { plant, phenotype } => {
            let target = cell.plants.entry(*plant).or_default();
            target.phenotype = Some(*phenotype);
            target.lifecycle = Some(PlantLifecycle::Stump);
        }
        VegetationMutation::Burn {
            plant,
            phenotype,
            remaining_fuel,
        } => {
            let target = cell.plants.entry(*plant).or_default();
            target.phenotype = Some(*phenotype);
            target.fuel = Some(*remaining_fuel);
            target.health = Some(UnitInterval::ZERO);
            target.lifecycle = Some(PlantLifecycle::Dead);
        }
        VegetationMutation::Ignite { plant } => {
            cell.plants.entry(*plant).or_default().ignited = true;
        }
        VegetationMutation::Extinguish { plant } => {
            cell.plants.entry(*plant).or_default().ignited = false;
        }
        VegetationMutation::Regrow {
            plant,
            lifecycle,
            phenotype,
            ecology_tick,
        } => {
            if matches!(
                lifecycle,
                PlantLifecycle::Dead | PlantLifecycle::Stump | PlantLifecycle::Removed
            ) {
                return Err(Error::Mutation(
                    "regrow requires a living lifecycle state".to_owned(),
                ));
            }
            let target = cell.plants.entry(*plant).or_default();
            if target.ecology_tick.is_some_and(|tick| *ecology_tick < tick) {
                return Err(Error::Mutation(
                    "ecology tick cannot move backwards".to_owned(),
                ));
            }
            target.tombstoned = false;
            target.lifecycle = Some(*lifecycle);
            target.phenotype = Some(*phenotype);
            target.ecology_tick = Some(*ecology_tick);
            target.health = Some(UnitInterval::ONE);
        }
        VegetationMutation::PromotionOriginState { plant, state } => {
            let target = cell.plants.entry(*plant).or_default();
            target.transform = Some((state.position, state.orientation, state.scale));
            target.promotion_origin = Some(*state);
        }
        VegetationMutation::DisturbanceMask {
            categories,
            tile,
            values,
        } => {
            if *categories == 0 || values.is_empty() {
                return Err(Error::Mutation(
                    "disturbance mask requires categories and samples".to_owned(),
                ));
            }
            cell.disturbance_masks.insert(
                DisturbanceTileKey {
                    categories: *categories,
                    tile: *tile,
                },
                values.clone(),
            );
        }
    }
    Ok(())
}

fn add_point(cell: &mut VegetationCellState, point: &PlantPoint) -> Result<()> {
    if cell
        .plants
        .get(&point.id)
        .is_some_and(|existing| existing.addition.is_some())
    {
        return Err(Error::DuplicatePlantId(point.id.to_string()));
    }
    let state = cell.plants.entry(point.id).or_default();
    state.tombstoned = false;
    state.addition = Some(point.clone());
    state.lifecycle = Some(point.lifecycle);
    state.phenotype = Some(point.phenotype);
    state.ecology_tick = Some(point.ecology_tick);
    state.health = Some(point.health);
    state.moisture = Some(point.moisture);
    state.fuel = Some(point.fuel);
    state.interaction_policy = Some(point.interaction_policy);
    Ok(())
}

pub(crate) fn record_order_key(
    record: &VegetationMutationRecord,
) -> ([u8; 25], u8, [u8; 16], u128) {
    (
        record.header.cell.canonical_bytes(),
        mutation_tag(&record.mutation),
        mutation_plant_id(&record.mutation).map_or([0; 16], PlantId::bytes),
        record.header.idempotency_key,
    )
}

fn transaction_signature(records: &[&VegetationMutationRecord]) -> Result<[u8; 32]> {
    let mut bytes = b"saffron-anima/vegetation-transaction/v1\0".to_vec();
    push_len(&mut bytes, records.len())?;
    for record in records {
        push_record(&mut bytes, record)?;
    }
    Ok(sha256(&bytes))
}

fn push_record(bytes: &mut Vec<u8>, record: &VegetationMutationRecord) -> Result<()> {
    bytes.extend_from_slice(&record.header.cell.canonical_bytes());
    bytes.extend_from_slice(&record.header.transaction.to_be_bytes());
    bytes.extend_from_slice(&record.header.authority.to_be_bytes());
    bytes.extend_from_slice(&record.header.logical_tick.to_be_bytes());
    bytes.extend_from_slice(&record.header.idempotency_key.to_be_bytes());
    push_option_u64(bytes, record.header.base_revision);
    bytes.push(mutation_tag(&record.mutation));
    match &record.mutation {
        VegetationMutation::FieldTilePatch {
            layer,
            channel,
            tile,
            dimensions,
            quantum_bits,
            values,
        } => {
            bytes.extend_from_slice(&layer.to_be_bytes());
            push_field_channel(bytes, *channel);
            bytes.extend_from_slice(&tile.to_be_bytes());
            for dimension in dimensions {
                bytes.extend_from_slice(&dimension.to_be_bytes());
            }
            bytes.extend_from_slice(&quantum_bits.to_be_bytes());
            push_len(bytes, values.len())?;
            for value in values {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
        }
        VegetationMutation::AnchorAddition(point) | VegetationMutation::Planting(point) => {
            bytes.extend_from_slice(
                &PlantPointColumns::from_points(vec![point.clone()])?.canonical_bytes()?,
            );
        }
        VegetationMutation::Tombstone { plant } => bytes.extend_from_slice(&plant.bytes()),
        VegetationMutation::TransformOverride {
            plant,
            position,
            orientation,
            scale,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            push_position(bytes, *position);
            push_orientation(bytes, *orientation);
            push_scale(bytes, *scale);
        }
        VegetationMutation::StateOverride {
            plant,
            lifecycle,
            phenotype,
            health,
            moisture,
            fuel,
            interaction_policy,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            push_option_u32(bytes, lifecycle.map(|value| value as u32));
            push_option_u32(bytes, *phenotype);
            push_option_unit(bytes, *health);
            push_option_unit(bytes, *moisture);
            push_option_unit(bytes, *fuel);
            push_option_u32(bytes, interaction_policy.map(|value| value as u32));
        }
        VegetationMutation::Damage {
            plant,
            amount,
            phenotype,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            bytes.extend_from_slice(&amount.canonical_bytes());
            push_option_u32(bytes, *phenotype);
        }
        VegetationMutation::MoistureFuel {
            plant,
            moisture,
            fuel,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            bytes.extend_from_slice(&moisture.canonical_bytes());
            bytes.extend_from_slice(&fuel.canonical_bytes());
        }
        VegetationMutation::LifecycleTransition {
            plant,
            from,
            to,
            ecology_tick,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            push_option_u32(bytes, from.map(|value| value as u32));
            bytes.extend_from_slice(&(*to as u32).to_be_bytes());
            bytes.extend_from_slice(&ecology_tick.to_be_bytes());
        }
        VegetationMutation::Harvest { plant, phenotype } => {
            bytes.extend_from_slice(&plant.bytes());
            bytes.extend_from_slice(&phenotype.to_be_bytes());
        }
        VegetationMutation::Burn {
            plant,
            phenotype,
            remaining_fuel,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            bytes.extend_from_slice(&phenotype.to_be_bytes());
            bytes.extend_from_slice(&remaining_fuel.canonical_bytes());
        }
        VegetationMutation::Ignite { plant } | VegetationMutation::Extinguish { plant } => {
            bytes.extend_from_slice(&plant.bytes());
        }
        VegetationMutation::Regrow {
            plant,
            lifecycle,
            phenotype,
            ecology_tick,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            bytes.extend_from_slice(&(*lifecycle as u32).to_be_bytes());
            bytes.extend_from_slice(&phenotype.to_be_bytes());
            bytes.extend_from_slice(&ecology_tick.to_be_bytes());
        }
        VegetationMutation::PromotionOriginState { plant, state } => {
            bytes.extend_from_slice(&plant.bytes());
            push_promotion(bytes, *state);
        }
        VegetationMutation::DisturbanceMask {
            categories,
            tile,
            values,
        } => {
            bytes.extend_from_slice(&categories.to_be_bytes());
            bytes.extend_from_slice(&tile.to_be_bytes());
            push_len(bytes, values.len())?;
            for value in values {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
        }
    }
    Ok(())
}

pub(crate) fn mutation_tag(mutation: &VegetationMutation) -> u8 {
    match mutation {
        VegetationMutation::FieldTilePatch { .. } => 0,
        VegetationMutation::AnchorAddition(_) => 1,
        VegetationMutation::Tombstone { .. } => 2,
        VegetationMutation::TransformOverride { .. } => 3,
        VegetationMutation::StateOverride { .. } => 4,
        VegetationMutation::Planting(_) => 5,
        VegetationMutation::Damage { .. } => 6,
        VegetationMutation::MoistureFuel { .. } => 7,
        VegetationMutation::LifecycleTransition { .. } => 8,
        VegetationMutation::Harvest { .. } => 9,
        VegetationMutation::Burn { .. } => 10,
        VegetationMutation::Regrow { .. } => 11,
        VegetationMutation::PromotionOriginState { .. } => 12,
        VegetationMutation::DisturbanceMask { .. } => 13,
        VegetationMutation::Ignite { .. } => 14,
        VegetationMutation::Extinguish { .. } => 15,
    }
}

fn mutation_plant_id(mutation: &VegetationMutation) -> Option<PlantId> {
    match mutation {
        VegetationMutation::AnchorAddition(point) | VegetationMutation::Planting(point) => {
            Some(point.id)
        }
        VegetationMutation::Tombstone { plant }
        | VegetationMutation::TransformOverride { plant, .. }
        | VegetationMutation::StateOverride { plant, .. }
        | VegetationMutation::Damage { plant, .. }
        | VegetationMutation::MoistureFuel { plant, .. }
        | VegetationMutation::LifecycleTransition { plant, .. }
        | VegetationMutation::Harvest { plant, .. }
        | VegetationMutation::Burn { plant, .. }
        | VegetationMutation::Ignite { plant }
        | VegetationMutation::Extinguish { plant }
        | VegetationMutation::Regrow { plant, .. }
        | VegetationMutation::PromotionOriginState { plant, .. } => Some(*plant),
        VegetationMutation::FieldTilePatch { .. } | VegetationMutation::DisturbanceMask { .. } => {
            None
        }
    }
}

fn push_promotion(bytes: &mut Vec<u8>, state: PromotionOriginState) {
    push_position(bytes, state.position);
    push_orientation(bytes, state.orientation);
    push_scale(bytes, state.scale);
    push_scale(bytes, state.linear_velocity);
    push_scale(bytes, state.angular_velocity);
}

fn push_position(bytes: &mut Vec<u8>, position: WorldPosition) {
    for tick in position.global_ticks() {
        bytes.extend_from_slice(&tick.to_be_bytes());
    }
}

fn push_orientation(bytes: &mut Vec<u8>, orientation: QuantizedOrientation) {
    for lane in orientation.bits() {
        bytes.extend_from_slice(&lane.to_be_bytes());
    }
}

fn push_scale(bytes: &mut Vec<u8>, scale: [DecisionScalar; 3]) {
    for value in scale {
        bytes.extend_from_slice(&value.canonical_bytes());
    }
}

fn push_field_channel(bytes: &mut Vec<u8>, channel: FieldChannel) {
    let (tag, user) = match channel {
        FieldChannel::Altitude => (0, 0),
        FieldChannel::Slope => (1, 0),
        FieldChannel::Curvature => (2, 0),
        FieldChannel::Concavity => (3, 0),
        FieldChannel::Drainage => (4, 0),
        FieldChannel::Moisture => (5, 0),
        FieldChannel::Temperature => (6, 0),
        FieldChannel::Precipitation => (7, 0),
        FieldChannel::Sunlight => (8, 0),
        FieldChannel::Exposure => (9, 0),
        FieldChannel::WaterDistance => (10, 0),
        FieldChannel::WaterDepth => (11, 0),
        FieldChannel::SignedBlocker => (12, 0),
        FieldChannel::SplineDistance => (13, 0),
        FieldChannel::User(value) => (14, value),
    };
    bytes.push(tag);
    bytes.extend_from_slice(&user.to_be_bytes());
}

fn push_option_u32(bytes: &mut Vec<u8>, value: Option<u32>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        None => bytes.push(0),
    }
}

fn push_option_u64(bytes: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        None => bytes.push(0),
    }
}

fn push_option_unit(bytes: &mut Vec<u8>, value: Option<UnitInterval>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.canonical_bytes());
        }
        None => bytes.push(0),
    }
}

fn push_len(bytes: &mut Vec<u8>, length: usize) -> Result<()> {
    bytes.extend_from_slice(
        &u64::try_from(length)
            .map_err(|_| Error::NumericOverflow)?
            .to_be_bytes(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use saffron_core::Uuid;
    use saffron_spatial::{DecisionScalar, WorldBounds};

    use super::*;
    use crate::{PlantFlags, PlantIdNamespace};

    fn point(id: PlantId, cell: WorldCellKey) -> PlantPoint {
        let position = WorldPosition::from_global_ticks(
            cell.coordinates()
                .map(|coordinate| i128::from(coordinate) * 262_144 + 1),
        )
        .unwrap();
        PlantPoint {
            id,
            owner: cell,
            position,
            orientation: QuantizedOrientation::identity(),
            scale: [DecisionScalar::from_integer(1).unwrap(); 3],
            bounds: WorldBounds::new(
                position.global_ticks().map(|tick| tick - 1),
                position.global_ticks().map(|tick| tick + 2),
            )
            .unwrap(),
            family: Uuid(7),
            variation: 0,
            lifecycle: PlantLifecycle::Sprout,
            phenotype: 0,
            representation_class: 0,
            deterministic_key: 8,
            candidate: 9,
            parent: None,
            colony: None,
            ecology_tick: 10,
            health: UnitInterval::ONE,
            moisture: UnitInterval::from_bits(20_000),
            fuel: UnitInterval::from_bits(30_000),
            phenology: UnitInterval::ZERO,
            flags: PlantFlags::RUNTIME,
            interaction_policy: InteractionPolicy::Interactive,
            provenance: 0,
            attachment: None,
            surface_projection: [DecisionScalar::from_bits(0); 3],
        }
    }

    fn record(
        cell: WorldCellKey,
        transaction: u128,
        idempotency_key: u128,
        mutation: VegetationMutation,
    ) -> VegetationMutationRecord {
        VegetationMutationRecord {
            header: MutationHeader {
                cell,
                transaction,
                authority: 17,
                logical_tick: transaction as u64,
                idempotency_key,
                base_revision: Some(0),
            },
            mutation,
        }
    }

    #[test]
    fn committed_records_emit_one_typed_transition_and_replays_emit_none() {
        let manifest = [5; 32];
        let cell = WorldCellKey::base(1, 0, -2);
        let id = PlantId::runtime([6; 16]).unwrap();
        let mut state = VegetationState::new(manifest);

        // Plant it, then damage it, then harvest it: three records in one transaction.
        let planting = record(cell, 1, 21, VegetationMutation::Planting(point(id, cell)));
        let damage = record(
            cell,
            1,
            22,
            VegetationMutation::Damage {
                plant: id,
                amount: UnitInterval::from_bits(16_000),
                phenotype: Some(3),
            },
        );
        let harvest = record(
            cell,
            1,
            23,
            VegetationMutation::Harvest {
                plant: id,
                phenotype: 4,
            },
        );
        let records = [planting, damage, harvest];
        let reduction = reduce_mutations(&mut state, manifest, &records).unwrap();

        assert_eq!(reduction.committed_transactions, vec![1]);
        assert_eq!(reduction.transitions.len(), 3, "one per committed record");
        assert!(
            reduction
                .transitions
                .iter()
                .all(|transition| transition.cell == cell
                    && transition.plant == Some(id)
                    && transition.transaction == 1)
        );
        assert_eq!(
            reduction.transitions[0].kind,
            VegetationTransitionKind::Planted
        );
        // The damage transition reports the health the plant settled at, not just the amount.
        let VegetationTransitionKind::Damaged { amount, health } = reduction.transitions[1].kind
        else {
            panic!(
                "expected a damage transition, got {:?}",
                reduction.transitions[1].kind
            );
        };
        assert_eq!(amount, UnitInterval::from_bits(16_000));
        assert_eq!(
            health,
            state.cells()[&cell].plants[&id].health.unwrap(),
            "the emitted health matches the reduced state"
        );
        assert_eq!(
            reduction.transitions[2].kind,
            VegetationTransitionKind::Harvested { phenotype: 4 }
        );

        // An exact replay of the same transaction is idempotent, so it emits nothing.
        let replay = reduce_mutations(&mut state, manifest, &records).unwrap();
        assert_eq!(replay.replayed_transactions, vec![1]);
        assert!(
            replay.transitions.is_empty(),
            "a replay must not re-fire events"
        );
    }

    #[test]
    fn shuffled_transactions_and_exact_replays_reduce_identically() {
        let manifest = [3; 32];
        let cell = WorldCellKey::base(0, 0, 0);
        let id = PlantId::runtime([4; 16]).unwrap();
        assert_eq!(id.namespace().unwrap(), PlantIdNamespace::Runtime);
        let planting = record(cell, 1, 11, VegetationMutation::Planting(point(id, cell)));
        let moisture = VegetationMutationRecord {
            header: MutationHeader {
                cell,
                transaction: 2,
                authority: 17,
                logical_tick: 2,
                idempotency_key: 12,
                base_revision: Some(1),
            },
            mutation: VegetationMutation::MoistureFuel {
                plant: id,
                moisture: UnitInterval::from_bits(5),
                fuel: UnitInterval::from_bits(6),
            },
        };
        let mut ordered = VegetationState::new(manifest);
        reduce_mutations(
            &mut ordered,
            manifest,
            &[planting.clone(), moisture.clone()],
        )
        .unwrap();

        let mut shuffled = VegetationState::new(manifest);
        reduce_mutations(
            &mut shuffled,
            manifest,
            &[moisture.clone(), planting.clone(), moisture, planting],
        )
        .unwrap();
        assert_eq!(
            ordered.canonical_bytes().unwrap(),
            shuffled.canonical_bytes().unwrap()
        );
    }

    #[test]
    fn cross_cell_transaction_is_atomic() {
        let manifest = [5; 32];
        let a = WorldCellKey::base(0, 0, 0);
        let b = WorldCellKey::base(1, 0, 0);
        let id_a = PlantId::runtime([6; 16]).unwrap();
        let id_b = PlantId::runtime([7; 16]).unwrap();
        let mut second = record(b, 1, 12, VegetationMutation::Planting(point(id_b, b)));
        second.header.base_revision = Some(99);
        let mut state = VegetationState::new(manifest);
        assert!(
            reduce_mutations(
                &mut state,
                manifest,
                &[
                    record(a, 1, 11, VegetationMutation::Planting(point(id_a, a))),
                    second
                ]
            )
            .is_err()
        );
        assert!(state.cells().is_empty());
    }

    #[test]
    fn snapshot_tail_compaction_matches_full_reduction() {
        let manifest = [8; 32];
        let cell = WorldCellKey::base(0, 0, 0);
        let id = PlantId::runtime([9; 16]).unwrap();
        let planting = record(cell, 1, 11, VegetationMutation::Planting(point(id, cell)));
        let moisture = VegetationMutationRecord {
            header: MutationHeader {
                cell,
                transaction: 2,
                authority: 17,
                logical_tick: 2,
                idempotency_key: 12,
                base_revision: Some(1),
            },
            mutation: VegetationMutation::MoistureFuel {
                plant: id,
                moisture: UnitInterval::from_bits(5),
                fuel: UnitInterval::from_bits(6),
            },
        };
        let mut full = VegetationState::new(manifest);
        reduce_mutations(&mut full, manifest, &[planting.clone(), moisture.clone()]).unwrap();

        let compacted = SaveStateEnvelope {
            binding: VegetationStateBinding {
                manifest_identity: ContentHash::new(manifest),
                cook_graph_identity: ContentHash::new([9; 32]),
                versions: CookVersionSet::current(),
                seed_namespaces_identity: ContentHash::new([10; 32]),
            },
            snapshot: VegetationState::new(manifest),
            tail: vec![moisture.clone(), planting.clone(), moisture, planting],
        }
        .compact()
        .unwrap();
        assert!(compacted.tail.is_empty());
        assert_eq!(
            full.canonical_bytes().unwrap(),
            compacted.snapshot.canonical_bytes().unwrap()
        );
    }

    #[test]
    fn state_rejects_a_different_manifest() {
        let mut state = VegetationState::new([1; 32]);
        assert!(matches!(
            reduce_mutations(&mut state, [2; 32], &[]),
            Err(Error::ManifestMismatch)
        ));
    }
}
