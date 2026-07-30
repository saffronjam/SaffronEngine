//! The typed persistent mutations and the per-cell delta they reduce into.

use std::collections::BTreeMap;

use saffron_spatial::{DecisionScalar, FieldChannel, UnitInterval, WorldCellKey, WorldPosition};

use crate::{InteractionPolicy, PlantId, PlantLifecycle, PlantPoint, QuantizedOrientation, Result};

use super::encode::push_record;

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
        dimensions: [u32; 3],
        /// Fixed quantization step.
        quantum_bits: i32,
        /// Canonical signed values.
        values: Vec<i32>,
    },
    /// Add an explicit authored anchor.
    AnchorAddition(PlantPoint),
    /// Persistently remove a cooked, authored, or runtime plant.
    Tombstone { plant: PlantId },
    /// Persist an exact transform override without editing cooked base bytes.
    TransformOverride {
        plant: PlantId,
        position: WorldPosition,
        orientation: QuantizedOrientation,
        scale: [DecisionScalar; 3],
    },
    /// Replace selected persistent biological/interaction values.
    StateOverride {
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
        plant: PlantId,
        /// Closed normalized damage amount.
        amount: UnitInterval,
        /// Species-declared damaged phenotype.
        phenotype: Option<u32>,
    },
    /// Replace persistent water/fuel state independently from current weather appearance.
    MoistureFuel {
        plant: PlantId,
        moisture: UnitInterval,
        fuel: UnitInterval,
    },
    /// Advance or explicitly replace biological lifecycle state.
    LifecycleTransition {
        plant: PlantId,
        /// Required prior state when known.
        from: Option<PlantLifecycle>,
        /// New lifecycle state.
        to: PlantLifecycle,
        /// Monotonic biological tick; unrelated calendar movement never changes it.
        ecology_tick: u64,
    },
    /// Mark a plant harvested with a species-declared phenotype.
    Harvest { plant: PlantId, phenotype: u32 },
    /// Mark a plant burned and update its fuel/health.
    Burn {
        plant: PlantId,
        phenotype: u32,
        /// Fuel remaining after the event.
        remaining_fuel: UnitInterval,
    },
    /// Set a plant alight. Vegetation persists that it is burning and keeps its fuel; heat
    /// propagation and smoke belong to a fire system.
    Ignite { plant: PlantId },
    /// Put a burning plant out, leaving whatever fuel and damage the fire left behind.
    Extinguish { plant: PlantId },
    /// Clear removal and start a new biological lifecycle for the same stable identity.
    Regrow {
        plant: PlantId,
        /// Regrown lifecycle state.
        lifecycle: PlantLifecycle,
        phenotype: u32,
        /// New biological tick.
        ecology_tick: u64,
    },
    /// Persist authoritative state produced while a macro plant was promoted.
    PromotionOriginState {
        plant: PlantId,
        /// Returned authoritative state.
        state: PromotionOriginState,
    },
    /// Replace one signed disturbance-mask tile.
    DisturbanceMask {
        /// Stable disturbance class/category bits.
        categories: u32,
        tile: u128,
        /// Packed signed values.
        values: Vec<i16>,
    },
}

/// One mutation with its required common metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMutationRecord {
    /// Ordering and authority header.
    pub header: MutationHeader,
    pub mutation: VegetationMutation,
}

impl VegetationMutationRecord {
    /// Bytes this record occupies in a transaction's canonical form.
    ///
    /// The exact size the reducer hashes and a caller budgets against, rather than a guess from the
    /// wire form, which carries JSON framing the reducer never sees.
    ///
    /// # Errors
    ///
    /// [`Error::NumericOverflow`] when a payload length exceeds the canonical bound.
    pub fn canonical_byte_len(&self) -> Result<usize> {
        let mut bytes = Vec::new();
        push_record(&mut bytes, self)?;
        Ok(bytes.len())
    }
}

/// Canonical field-tile address in persistent state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FieldTileKey {
    pub layer: u128,
    pub channel: FieldChannel,
    /// Stable tile identity.
    pub tile: u128,
}

/// Quantized field-tile state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldTileState {
    pub dimensions: [u32; 3],
    /// Quantization step.
    pub quantum_bits: i32,
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
    pub disturbance_masks: BTreeMap<DisturbanceTileKey, Vec<i16>>,
}
