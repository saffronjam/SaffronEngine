//! Immutable runtime cell generations, facet residency, and vegetation queries.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use glam::DVec3;
use saffron_core::Uuid;
use saffron_spatial::{
    GenerationSlot, GenerationToken, ResidencyFacet, ResidencyManager, ResidencyMask,
    ResidencySnapshot, SpatialSource, SpatialSourceId, UnitInterval, WorldBounds, WorldCellKey,
    WorldPosition,
};

use crate::{
    ContentHash, DisturbanceTileKey, Error, InteractionPolicy, MicroFieldTile, PlantFlags, PlantId,
    PlantLifecycle, PlantPoint, PlantPointColumns, PlantTagId, ProvenanceHandle, ProvenanceRecord,
    ProvenanceTable, Result, SaveStateEnvelope, VegetationBaseManifest,
    VegetationCellArtifactIndex, VegetationCellFacet, VegetationCellSectionKind,
    VegetationCellState, VegetationManifestCell, VegetationMutationRecord,
    VegetationRejectionDiagnosticsFacet, VegetationState, VegetationStateBinding,
    decode_vegetation_cell_facet, reduce_mutations,
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

/// Stable identity of one completely published runtime cell generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VegetationCellGenerationId {
    /// Canonical cell.
    pub cell: WorldCellKey,
    /// Monotonic generation within the cell publication slot.
    pub generation: u64,
}

/// Opaque generation-tagged reference to a stable plant identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VegetationPlantHandle {
    /// Stable public identity.
    pub plant: PlantId,
    /// Cell generation that produced the handle.
    pub generation: VegetationCellGenerationId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlantSlot {
    row: u32,
    generation: u64,
}

/// Immutable query result; no internal row or slot escapes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationPlantSnapshot {
    /// Stable plant identity.
    pub plant: PlantId,
    /// Generation-tagged lookup handle.
    pub handle: VegetationPlantHandle,
    /// Exact world position.
    pub position: WorldPosition,
    /// Conservative world bounds.
    pub bounds: WorldBounds,
    /// Plant-family identity.
    pub family: Uuid,
    /// Canonically ordered family tags.
    pub tags: Vec<PlantTagId>,
    /// Biological lifecycle state.
    pub lifecycle: PlantLifecycle,
    /// Species phenotype.
    pub phenotype: u32,
    /// Gameplay interaction policy.
    pub interaction_policy: InteractionPolicy,
    /// Persistent health.
    pub health: UnitInterval,
    /// Persistent moisture.
    pub moisture: UnitInterval,
    /// Persistent fuel.
    pub fuel: UnitInterval,
    /// Compact accepted-point provenance when the editing facet is resident.
    pub provenance: Option<ProvenanceRecord>,
}

/// Closed filters shared by every vegetation macro query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VegetationQueryFilter {
    /// Allowed families; empty accepts every family.
    pub families: Vec<Uuid>,
    /// Every listed tag must be present on the family.
    pub required_tags: BTreeSet<PlantTagId>,
    /// Allowed lifecycle values; empty accepts every lifecycle.
    pub lifecycles: BTreeSet<PlantLifecycle>,
    /// Allowed interaction policies; empty accepts every policy.
    pub interaction_policies: Vec<InteractionPolicy>,
}

impl VegetationQueryFilter {
    fn matches(&self, point: &PlantPoint, tags: &[PlantTagId]) -> bool {
        (self.families.is_empty() || self.families.contains(&point.family))
            && self.required_tags.iter().all(|tag| tags.contains(tag))
            && (self.lifecycles.is_empty() || self.lifecycles.contains(&point.lifecycle))
            && (self.interaction_policies.is_empty()
                || self
                    .interaction_policies
                    .contains(&point.interaction_policy))
    }
}

/// Finite world-space ray used only by the vegetation query surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VegetationQueryRay {
    /// Exact quantized origin.
    pub origin: WorldPosition,
    /// Normalized world-space direction.
    pub direction: DVec3,
    /// Inclusive maximum distance in metres.
    pub max_distance_m: f64,
}

impl VegetationQueryRay {
    /// Validates and normalizes a finite non-zero direction.
    pub fn new(origin: WorldPosition, direction: DVec3, max_distance_m: f64) -> Result<Self> {
        if !direction.is_finite()
            || direction.length_squared() == 0.0
            || !max_distance_m.is_finite()
            || max_distance_m < 0.0
        {
            return Err(Error::ArtifactFormat {
                format: "vegetation runtime query",
                field: "ray".to_owned(),
            });
        }
        Ok(Self {
            origin,
            direction: direction.normalize(),
            max_distance_m,
        })
    }
}

/// One bounds-level vegetation ray hit.
#[derive(Clone, Debug, PartialEq)]
pub struct VegetationRayHit {
    /// Matching plant.
    pub plant: VegetationPlantSnapshot,
    /// Entry distance into its conservative bounds.
    pub distance_m: f64,
}

/// One nearest-plant result.
#[derive(Clone, Debug, PartialEq)]
pub struct VegetationNearestHit {
    /// Matching plant.
    pub plant: VegetationPlantSnapshot,
    /// Euclidean point-to-bounds distance.
    pub distance_m: f64,
}

/// A coalesced missing-facet request for one cell generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationCellLoadRequest {
    /// Canonical requested cell.
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

/// Privately staged, fully validated cell replacement.
pub struct StagedVegetationCellGeneration {
    token: GenerationToken,
    generation: Arc<VegetationCellGeneration>,
}

/// Immutable work packet that can validate and decode a cell on any worker thread.
pub struct VegetationCellLoad {
    token: GenerationToken,
    facets: ResidencyMask,
    manifest_cell: VegetationManifestCell,
    platform_profile: ContentHash,
    manifest_identity: ContentHash,
    current: Arc<VegetationCellGeneration>,
    persistent: Option<VegetationCellState>,
    family_tags: Arc<BTreeMap<u64, Vec<PlantTagId>>>,
}

impl VegetationCellLoad {
    /// Requested generation token used for deterministic cancellation diagnostics.
    #[must_use]
    pub const fn token(&self) -> GenerationToken {
        self.token
    }

    /// Validates and decodes the requested facets without touching the published world.
    pub fn stage(self, bytes: &[u8]) -> Result<StagedVegetationCellGeneration> {
        validate_artifact_against_manifest(self.platform_profile, &self.manifest_cell, bytes)?;
        let index = VegetationCellArtifactIndex::open(bytes)?;
        let mut decoded = self.current.facets.clone();
        let resident = union_masks(self.current.resident, self.facets);
        for kind in required_sections(self.facets) {
            let section = index
                .section(bytes, kind)?
                .ok_or_else(|| Error::ArtifactFormat {
                    format: ".svegcell",
                    field: format!("missing runtime facet {}", kind as u16),
                })?;
            decoded.insert(kind, decode_vegetation_cell_facet(kind, section)?);
        }
        decoded.retain(|kind, _| required_sections(resident).contains(kind));
        let base = macro_columns(&decoded)?;
        let macro_points = effective_macro_points(base, self.persistent.as_ref())?;
        let disturbance_masks = self
            .persistent
            .map(|state| state.disturbance_masks)
            .unwrap_or_default();
        let generation = Arc::new(VegetationCellGeneration::build(
            VegetationCellGenerationId {
                cell: self.token.cell,
                generation: self.token.generation,
            },
            self.manifest_identity,
            resident,
            decoded,
            macro_points,
            self.family_tags,
            disturbance_masks,
        )?);
        Ok(StagedVegetationCellGeneration {
            token: self.token,
            generation,
        })
    }
}

/// Complete immutable data visible to readers for one cell generation.
#[derive(Clone, Debug)]
pub struct VegetationCellGeneration {
    id: VegetationCellGenerationId,
    manifest_identity: ContentHash,
    resident: ResidencyMask,
    facets: BTreeMap<VegetationCellSectionKind, VegetationCellFacet>,
    macro_points: PlantPointColumns,
    slots: BTreeMap<PlantId, PlantSlot>,
    bvh: MacroBvh,
    family_tags: Arc<BTreeMap<u64, Vec<PlantTagId>>>,
    disturbance_masks: BTreeMap<DisturbanceTileKey, Vec<i16>>,
}

impl VegetationCellGeneration {
    fn empty(
        cell: WorldCellKey,
        manifest_identity: ContentHash,
        family_tags: Arc<BTreeMap<u64, Vec<PlantTagId>>>,
    ) -> Result<Self> {
        Self::build(
            VegetationCellGenerationId {
                cell,
                generation: 0,
            },
            manifest_identity,
            ResidencyMask::NONE,
            BTreeMap::new(),
            PlantPointColumns::default(),
            family_tags,
            BTreeMap::new(),
        )
    }

    fn build(
        id: VegetationCellGenerationId,
        manifest_identity: ContentHash,
        resident: ResidencyMask,
        facets: BTreeMap<VegetationCellSectionKind, VegetationCellFacet>,
        macro_points: PlantPointColumns,
        family_tags: Arc<BTreeMap<u64, Vec<PlantTagId>>>,
        disturbance_masks: BTreeMap<DisturbanceTileKey, Vec<i16>>,
    ) -> Result<Self> {
        let rows = macro_points.row_count()?;
        let mut slots = BTreeMap::new();
        let mut bounds = Vec::new();
        bounds
            .try_reserve_exact(rows)
            .map_err(|source| Error::MemoryReservation {
                resource: "vegetation runtime BVH bounds",
                source,
            })?;
        for row in 0..rows {
            let id_value = macro_points.ids[row];
            let row = u32::try_from(row).map_err(|_| Error::NumericOverflow)?;
            if slots
                .insert(
                    id_value,
                    PlantSlot {
                        row,
                        generation: id.generation,
                    },
                )
                .is_some()
            {
                return Err(Error::DuplicatePlantId(id_value.to_string()));
            }
            bounds.push(macro_points.bounds[row as usize]);
        }
        let bvh = MacroBvh::build(&bounds)?;
        Ok(Self {
            id,
            manifest_identity,
            resident,
            facets,
            macro_points,
            slots,
            bvh,
            family_tags,
            disturbance_masks,
        })
    }

    /// Complete generation identity.
    #[must_use]
    pub const fn id(&self) -> VegetationCellGenerationId {
        self.id
    }

    /// Exact immutable base manifest identity.
    #[must_use]
    pub const fn manifest_identity(&self) -> ContentHash {
        self.manifest_identity
    }

    /// Logical facets present in this complete generation.
    #[must_use]
    pub const fn resident_facets(&self) -> ResidencyMask {
        self.resident
    }

    /// Effective macro SoA after persistent deltas.
    #[must_use]
    pub fn macro_points(&self) -> &PlantPointColumns {
        &self.macro_points
    }

    /// Quantized micro tiles when a facet requiring them is resident.
    pub fn micro_fields(&self) -> Option<&[MicroFieldTile]> {
        match self.facets.get(&VegetationCellSectionKind::MicroFields) {
            Some(VegetationCellFacet::MicroFields(tiles)) => Some(tiles),
            _ => None,
        }
    }

    /// Persistent disturbance masks overlaid on quantized micro fields.
    #[must_use]
    pub fn disturbance_masks(&self) -> &BTreeMap<DisturbanceTileKey, Vec<i16>> {
        &self.disturbance_masks
    }

    /// Editor provenance when the editing facet is resident.
    pub fn provenance(&self) -> Option<&ProvenanceTable> {
        match self.facets.get(&VegetationCellSectionKind::Provenance) {
            Some(VegetationCellFacet::Provenance(table)) => Some(table),
            _ => None,
        }
    }

    /// Rejection diagnostics when the editing facet is resident.
    pub fn rejection_diagnostics(&self) -> Option<&VegetationRejectionDiagnosticsFacet> {
        match self
            .facets
            .get(&VegetationCellSectionKind::RejectionDiagnostics)
        {
            Some(VegetationCellFacet::RejectionDiagnostics(value)) => Some(value),
            _ => None,
        }
    }

    /// Requires one validated typed artifact facet for renderer, physics, nav, or tooling adapters.
    pub fn require_facet(&self, kind: VegetationCellSectionKind) -> Result<&VegetationCellFacet> {
        self.facets.get(&kind).ok_or(Error::FacetNotResident {
            cell: self.id.cell,
            facet: section_name(kind),
        })
    }

    fn point(&self, slot: PlantSlot) -> Result<PlantPoint> {
        if slot.generation != self.id.generation {
            return Err(Error::StaleGeneration {
                cell: self.id.cell,
                expected: slot.generation,
                current: self.id.generation,
            });
        }
        self.macro_points.point(slot.row as usize)
    }

    fn snapshot(&self, slot: PlantSlot) -> Result<VegetationPlantSnapshot> {
        let point = self.point(slot)?;
        let tags = self
            .family_tags
            .get(&point.family.value())
            .cloned()
            .unwrap_or_default();
        let provenance = self
            .provenance()
            .and_then(|table| table.get(ProvenanceHandle(point.provenance)))
            .cloned();
        Ok(VegetationPlantSnapshot {
            plant: point.id,
            handle: VegetationPlantHandle {
                plant: point.id,
                generation: self.id,
            },
            position: point.position,
            bounds: point.bounds,
            family: point.family,
            tags,
            lifecycle: point.lifecycle,
            phenotype: point.phenotype,
            interaction_policy: point.interaction_policy,
            health: point.health,
            moisture: point.moisture,
            fuel: point.fuel,
            provenance,
        })
    }

    fn matching_snapshot(
        &self,
        row: u32,
        filter: &VegetationQueryFilter,
    ) -> Result<Option<VegetationPlantSnapshot>> {
        let point = self.macro_points.point(row as usize)?;
        let tags = self
            .family_tags
            .get(&point.family.value())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if !filter.matches(&point, tags) {
            return Ok(None);
        }
        self.snapshot(PlantSlot {
            row,
            generation: self.id.generation,
        })
        .map(Some)
    }
}

struct RuntimeCell {
    slot: Arc<GenerationSlot<VegetationCellGeneration>>,
}

/// The sole authoritative owner of runtime vegetation cell generations and persistent deltas.
pub struct VegetationWorld {
    manifest: VegetationBaseManifest,
    manifest_identity: ContentHash,
    manifest_cells: BTreeMap<WorldCellKey, VegetationManifestCell>,
    family_tags: Arc<BTreeMap<u64, Vec<PlantTagId>>>,
    persistent: VegetationState,
    effective: VegetationState,
    predictions: BTreeMap<u128, Vec<VegetationMutationRecord>>,
    residency: ResidencyManager,
    budgets: VegetationResidencyBudgets,
    residency_revision: u64,
    cells: BTreeMap<WorldCellKey, RuntimeCell>,
}

impl VegetationWorld {
    /// Binds a new runtime world to one exact, validated immutable manifest.
    pub fn new(
        manifest: VegetationBaseManifest,
        budgets: VegetationResidencyBudgets,
    ) -> Result<Self> {
        let canonical = manifest.canonical_bytes()?;
        let manifest_identity = ContentHash::of(&canonical);
        let manifest_cells = manifest
            .cells
            .iter()
            .cloned()
            .map(|cell| (cell.cell, cell))
            .collect();
        let family_tags = Arc::new(
            manifest
                .plants
                .iter()
                .map(|plant| (plant.family.value(), plant.tags.clone()))
                .collect(),
        );
        let persistent = VegetationState::new(manifest_identity.bytes());
        Ok(Self {
            effective: persistent.clone(),
            persistent,
            predictions: BTreeMap::new(),
            manifest,
            manifest_identity,
            manifest_cells,
            family_tags,
            residency: ResidencyManager::new(),
            budgets,
            residency_revision: 0,
            cells: BTreeMap::new(),
        })
    }

    /// Bound immutable manifest.
    #[must_use]
    pub fn manifest(&self) -> &VegetationBaseManifest {
        &self.manifest
    }

    /// Exact bound manifest identity.
    #[must_use]
    pub const fn manifest_identity(&self) -> ContentHash {
        self.manifest_identity
    }

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
        self.publish_state_rebuilds(staged)
    }

    /// Adds or replaces one predictive source and reschedules within the per-facet budgets.
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

    /// Returns the current complete generation while keeping it alive for the reader.
    pub fn cell_snapshot(&self, cell: WorldCellKey) -> Option<Arc<VegetationCellGeneration>> {
        self.cells.get(&cell).map(|entry| entry.slot.read())
    }

    /// Begins one coalesced load and returns a worker-owned immutable staging packet.
    pub fn begin_load(
        &mut self,
        cell: WorldCellKey,
        facets: ResidencyMask,
    ) -> Result<VegetationCellLoad> {
        if facets == ResidencyMask::NONE {
            return Err(Error::ArtifactFormat {
                format: "vegetation runtime load",
                field: "facets".to_owned(),
            });
        }
        let manifest_cell = self.require_manifest_cell(cell)?.clone();
        self.ensure_runtime_cell(cell)?;
        let token = self.cells[&cell]
            .slot
            .begin(self.residency_revision)
            .map_err(Error::from)?;
        Ok(VegetationCellLoad {
            token,
            facets,
            manifest_cell,
            platform_profile: self.manifest.platform.identity()?,
            manifest_identity: self.manifest_identity,
            current: self.cells[&cell].slot.read(),
            persistent: self.effective.cells().get(&cell).cloned(),
            family_tags: Arc::clone(&self.family_tags),
        })
    }

    /// Cancels an in-flight cell load when its token remains current.
    pub fn cancel_load(&self, token: GenerationToken) -> Result<bool> {
        let entry = self
            .cells
            .get(&token.cell)
            .ok_or(Error::UnknownRuntimeCell { cell: token.cell })?;
        entry.slot.cancel(token).map_err(Into::into)
    }

    /// Atomically publishes a complete staged generation; late work is discarded.
    pub fn publish_staged(&self, staged: StagedVegetationCellGeneration) -> Result<bool> {
        if staged.generation.manifest_identity != self.manifest_identity {
            return Err(Error::ManifestMismatch);
        }
        let entry = self
            .cells
            .get(&staged.token.cell)
            .ok_or(Error::UnknownRuntimeCell {
                cell: staged.token.cell,
            })?;
        entry
            .slot
            .try_publish(staged.token, staged.generation)
            .map_err(Into::into)
    }

    /// Applies confirmed mutations through the one reducer and republishes changed resident cells.
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
        let staged = self.stage_state_rebuilds(&effective, &changed_cells)?;
        self.persistent = candidate;
        self.effective = effective;
        self.predictions = predictions;
        self.publish_state_rebuilds(staged)?;
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

    /// Resolves a generation-tagged handle and rejects stale generations deterministically.
    pub fn resolve_handle(&self, handle: VegetationPlantHandle) -> Result<VegetationPlantSnapshot> {
        let generation =
            self.cell_snapshot(handle.generation.cell)
                .ok_or(Error::PlantNotResident {
                    plant: handle.plant.to_string(),
                })?;
        if generation.id.generation != handle.generation.generation {
            return Err(Error::StaleGeneration {
                cell: handle.generation.cell,
                expected: handle.generation.generation,
                current: generation.id.generation,
            });
        }
        let slot = generation
            .slots
            .get(&handle.plant)
            .copied()
            .ok_or_else(|| Error::PlantNotResident {
                plant: handle.plant.to_string(),
            })?;
        generation.snapshot(slot)
    }

    /// Looks up one stable identity across CPU-resident macro generations.
    pub fn find_plant(&self, plant: PlantId) -> Result<Option<VegetationPlantSnapshot>> {
        for generation in self.resident_macro_generations() {
            if let Some(slot) = generation.slots.get(&plant).copied() {
                return generation.snapshot(slot).map(Some);
            }
        }
        Ok(None)
    }

    /// Exact-bounds query over every CPU-resident macro cell.
    pub fn query_bounds(
        &self,
        bounds: WorldBounds,
        filter: &VegetationQueryFilter,
    ) -> Result<Vec<VegetationPlantSnapshot>> {
        let mut results = Vec::new();
        for generation in self.resident_macro_generations() {
            for row in generation.bvh.query_bounds(bounds) {
                if let Some(snapshot) = generation.matching_snapshot(row, filter)? {
                    results.push(snapshot);
                }
            }
        }
        results.sort_unstable_by_key(|snapshot| snapshot.plant);
        Ok(results)
    }

    /// Radius query over conservative bounds, independent of render or collision visibility.
    pub fn query_radius(
        &self,
        center: WorldPosition,
        radius_m: f64,
        filter: &VegetationQueryFilter,
    ) -> Result<Vec<VegetationPlantSnapshot>> {
        if !radius_m.is_finite() || radius_m < 0.0 {
            return Err(Error::ArtifactFormat {
                format: "vegetation runtime query",
                field: "radius".to_owned(),
            });
        }
        let center_m = center.world_meters();
        let extent = DVec3::splat(radius_m);
        let candidate_bounds =
            WorldBounds::from_world_meters(center_m - extent, center_m + extent)?;
        let radius_squared = radius_m * radius_m;
        let mut results = Vec::new();
        for generation in self.resident_macro_generations() {
            for row in generation.bvh.query_bounds(candidate_bounds) {
                let point_bounds = generation.macro_points.bounds[row as usize];
                if distance_squared_to_bounds(center_m, point_bounds) <= radius_squared
                    && let Some(snapshot) = generation.matching_snapshot(row, filter)?
                {
                    results.push(snapshot);
                }
            }
        }
        results.sort_unstable_by_key(|snapshot| snapshot.plant);
        Ok(results)
    }

    /// Bounds ray query sorted by distance and stable identity.
    pub fn query_ray(
        &self,
        ray: VegetationQueryRay,
        filter: &VegetationQueryFilter,
    ) -> Result<Vec<VegetationRayHit>> {
        let origin = ray.origin.world_meters();
        let mut results = Vec::new();
        for generation in self.resident_macro_generations() {
            for (row, distance_m) in
                generation
                    .bvh
                    .query_ray(origin, ray.direction, ray.max_distance_m)
            {
                if let Some(plant) = generation.matching_snapshot(row, filter)? {
                    results.push(VegetationRayHit { plant, distance_m });
                }
            }
        }
        results.sort_by(|left, right| {
            left.distance_m
                .total_cmp(&right.distance_m)
                .then_with(|| left.plant.plant.cmp(&right.plant.plant))
        });
        Ok(results)
    }

    /// Nearest matching plant within an optional finite maximum distance.
    pub fn query_nearest(
        &self,
        position: WorldPosition,
        max_distance_m: Option<f64>,
        filter: &VegetationQueryFilter,
    ) -> Result<Option<VegetationNearestHit>> {
        if max_distance_m.is_some_and(|distance| !distance.is_finite() || distance < 0.0) {
            return Err(Error::ArtifactFormat {
                format: "vegetation runtime query",
                field: "nearest.maxDistance".to_owned(),
            });
        }
        let point = position.world_meters();
        let mut best: Option<VegetationNearestHit> = None;
        for generation in self.resident_macro_generations() {
            for row in generation.bvh.rows_by_nearness(point) {
                let distance_m =
                    distance_squared_to_bounds(point, generation.macro_points.bounds[row as usize])
                        .sqrt();
                if max_distance_m.is_some_and(|maximum| distance_m > maximum) {
                    continue;
                }
                let Some(plant) = generation.matching_snapshot(row, filter)? else {
                    continue;
                };
                let replace = best.as_ref().is_none_or(|current| {
                    distance_m < current.distance_m
                        || (distance_m == current.distance_m && plant.plant < current.plant.plant)
                });
                if replace {
                    best = Some(VegetationNearestHit { plant, distance_m });
                }
            }
        }
        Ok(best)
    }

    /// Complete source, byte, residency, and pending-load status.
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

    fn resident_macro_generations(
        &self,
    ) -> impl Iterator<Item = Arc<VegetationCellGeneration>> + '_ {
        self.cells.values().filter_map(|entry| {
            let generation = entry.slot.read();
            (!generation.macro_points.ids.is_empty()).then_some(generation)
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

    fn ensure_runtime_cell(&mut self, cell: WorldCellKey) -> Result<()> {
        if self.cells.contains_key(&cell) {
            return Ok(());
        }
        let empty = Arc::new(VegetationCellGeneration::empty(
            cell,
            self.manifest_identity,
            self.family_tags.clone(),
        )?);
        self.cells.insert(
            cell,
            RuntimeCell {
                slot: Arc::new(GenerationSlot::new(cell, empty)),
            },
        );
        Ok(())
    }

    fn require_manifest_cell(&self, cell: WorldCellKey) -> Result<&VegetationManifestCell> {
        self.manifest_cells
            .get(&cell)
            .ok_or(Error::UnknownRuntimeCell { cell })
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
        ordered.sort_unstable_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.cell.cmp(&right.cell))
        });
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
        }
        Ok(())
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

    fn publish_state_rebuilds(&self, staged: Vec<StagedVegetationCellGeneration>) -> Result<()> {
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

fn required_sections(facets: ResidencyMask) -> BTreeSet<VegetationCellSectionKind> {
    let mut result = BTreeSet::new();
    for facet in facets.iter() {
        result.extend(match facet {
            ResidencyFacet::Render => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::MicroFields,
                VegetationCellSectionKind::RenderReferences,
                VegetationCellSectionKind::RenderBounds,
            ][..],
            ResidencyFacet::Physics => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::CollisionInputs,
            ],
            ResidencyFacet::Simulation => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::MicroFields,
                VegetationCellSectionKind::EcologyBoundary,
                VegetationCellSectionKind::EcologyCheckpoint,
            ],
            ResidencyFacet::Editing => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::Provenance,
                VegetationCellSectionKind::RejectionDiagnostics,
                VegetationCellSectionKind::SurfaceAttachments,
                VegetationCellSectionKind::SurfaceDependencies,
            ],
            ResidencyFacet::Navigation => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::NavigationContributions,
            ],
            ResidencyFacet::Network => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::EcologyBoundary,
                VegetationCellSectionKind::EcologyCheckpoint,
            ],
        });
    }
    result
}

fn section_name(kind: VegetationCellSectionKind) -> &'static str {
    match kind {
        VegetationCellSectionKind::MacroPoints => "macro-points",
        VegetationCellSectionKind::MicroFields => "micro-fields",
        VegetationCellSectionKind::Provenance => "provenance",
        VegetationCellSectionKind::RejectionDiagnostics => "rejection-diagnostics",
        VegetationCellSectionKind::SurfaceAttachments => "surface-attachments",
        VegetationCellSectionKind::SurfaceDependencies => "surface-dependencies",
        VegetationCellSectionKind::RenderReferences => "render-references",
        VegetationCellSectionKind::RenderBounds => "render-bounds",
        VegetationCellSectionKind::CollisionInputs => "collision-inputs",
        VegetationCellSectionKind::NavigationContributions => "navigation-contributions",
        VegetationCellSectionKind::EcologyBoundary => "ecology-boundary",
        VegetationCellSectionKind::EcologyCheckpoint => "ecology-checkpoint",
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

fn validate_artifact_against_manifest(
    platform_profile: ContentHash,
    cell: &VegetationManifestCell,
    bytes: &[u8],
) -> Result<()> {
    if ContentHash::of(bytes) != cell.artifact_hash {
        return Err(Error::ArtifactHashMismatch {
            format: ".svegcell",
            subject: "manifest artifact".to_owned(),
        });
    }
    let index = VegetationCellArtifactIndex::open(bytes)?;
    if index.cell != cell.cell || index.payload_hash != cell.payload_hash {
        return Err(Error::ArtifactFormat {
            format: ".svegcell",
            field: "manifest cell identity".to_owned(),
        });
    }
    if index.platform_profile != platform_profile {
        return Err(Error::ArtifactHashMismatch {
            format: ".svegcell",
            subject: "platform profile".to_owned(),
        });
    }
    if index.sections.len() != cell.sections.len() {
        return Err(Error::ArtifactFormat {
            format: ".svegcell",
            field: "manifest sections".to_owned(),
        });
    }
    for (actual, expected) in index.sections.iter().zip(&cell.sections) {
        if actual.kind != expected.kind
            || actual.version != expected.version
            || actual.codec != expected.codec
            || actual.alignment != expected.alignment
            || actual.stored_size != expected.stored_size
            || actual.decoded_size != expected.decoded_size
            || actual.content_hash != expected.content_hash
        {
            return Err(Error::ArtifactFormat {
                format: ".svegcell",
                field: format!("manifest section {}", expected.kind as u16),
            });
        }
    }
    Ok(())
}

fn macro_columns(
    facets: &BTreeMap<VegetationCellSectionKind, VegetationCellFacet>,
) -> Result<&PlantPointColumns> {
    macro_columns_optional(facets).ok_or_else(|| Error::ArtifactFormat {
        format: ".svegcell",
        field: "macro points are required by every logical runtime facet".to_owned(),
    })
}

fn macro_columns_optional(
    facets: &BTreeMap<VegetationCellSectionKind, VegetationCellFacet>,
) -> Option<&PlantPointColumns> {
    match facets.get(&VegetationCellSectionKind::MacroPoints) {
        Some(VegetationCellFacet::MacroPoints(columns)) => Some(columns),
        _ => None,
    }
}

fn effective_macro_points(
    base: &PlantPointColumns,
    state: Option<&VegetationCellState>,
) -> Result<PlantPointColumns> {
    let mut points = BTreeMap::new();
    for row in 0..base.row_count()? {
        let point = base.point(row)?;
        points.insert(point.id, point);
    }
    if let Some(state) = state {
        for (plant, delta) in &state.plants {
            if let Some(addition) = &delta.addition
                && points.insert(*plant, addition.clone()).is_some()
            {
                return Err(Error::DuplicatePlantId(plant.to_string()));
            }
            if delta.tombstoned {
                points.remove(plant);
                continue;
            }
            let Some(point) = points.get_mut(plant) else {
                continue;
            };
            if let Some((position, orientation, scale)) = delta.transform {
                let previous = point.position.global_ticks();
                let next = position.global_ticks();
                let offset = std::array::from_fn(|axis| next[axis] - previous[axis]);
                point.position = position;
                point.orientation = orientation;
                point.scale = scale;
                point.bounds = translate_bounds(point.bounds, offset)?;
                point.flags = point.flags.union(PlantFlags::TRANSFORM_OVERRIDE);
            }
            if let Some(lifecycle) = delta.lifecycle {
                point.lifecycle = lifecycle;
            }
            if let Some(phenotype) = delta.phenotype {
                point.phenotype = phenotype;
            }
            if let Some(ecology_tick) = delta.ecology_tick {
                point.ecology_tick = ecology_tick;
            }
            if let Some(health) = delta.health {
                point.health = health;
            }
            if let Some(moisture) = delta.moisture {
                point.moisture = moisture;
            }
            if let Some(fuel) = delta.fuel {
                point.fuel = fuel;
            }
            if let Some(interaction_policy) = delta.interaction_policy {
                point.interaction_policy = interaction_policy;
            }
            if delta.lifecycle.is_some()
                || delta.phenotype.is_some()
                || delta.ecology_tick.is_some()
                || delta.health.is_some()
                || delta.moisture.is_some()
                || delta.fuel.is_some()
                || delta.interaction_policy.is_some()
            {
                point.flags = point.flags.union(PlantFlags::STATE_OVERRIDE);
            }
        }
    }
    PlantPointColumns::from_points(points.into_values().collect())
}

fn translate_bounds(bounds: WorldBounds, offset: [i128; 3]) -> Result<WorldBounds> {
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    WorldBounds::new(
        [
            minimum[0]
                .checked_add(offset[0])
                .ok_or(Error::NumericOverflow)?,
            minimum[1]
                .checked_add(offset[1])
                .ok_or(Error::NumericOverflow)?,
            minimum[2]
                .checked_add(offset[2])
                .ok_or(Error::NumericOverflow)?,
        ],
        [
            maximum[0]
                .checked_add(offset[0])
                .ok_or(Error::NumericOverflow)?,
            maximum[1]
                .checked_add(offset[1])
                .ok_or(Error::NumericOverflow)?,
            maximum[2]
                .checked_add(offset[2])
                .ok_or(Error::NumericOverflow)?,
        ],
    )
    .map_err(Into::into)
}

fn union_masks(left: ResidencyMask, right: ResidencyMask) -> ResidencyMask {
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

#[derive(Clone, Debug, Default)]
struct MacroBvh {
    nodes: Vec<MacroBvhNode>,
    row_bounds: Vec<WorldBounds>,
    root: Option<u32>,
}

#[derive(Clone, Debug)]
enum MacroBvhNode {
    Leaf {
        bounds: WorldBounds,
        rows: Vec<u32>,
    },
    Branch {
        bounds: WorldBounds,
        left: u32,
        right: u32,
    },
}

impl MacroBvhNode {
    const fn bounds(&self) -> WorldBounds {
        match self {
            Self::Leaf { bounds, .. } | Self::Branch { bounds, .. } => *bounds,
        }
    }
}

impl MacroBvh {
    fn build(bounds: &[WorldBounds]) -> Result<Self> {
        if bounds.is_empty() {
            return Ok(Self::default());
        }
        let mut value = Self {
            nodes: Vec::new(),
            row_bounds: bounds.to_vec(),
            root: None,
        };
        let rows = (0..bounds.len())
            .map(|row| u32::try_from(row).map_err(|_| Error::NumericOverflow))
            .collect::<Result<Vec<_>>>()?;
        value.root = Some(value.build_node(bounds, rows)?);
        Ok(value)
    }

    fn build_node(&mut self, source: &[WorldBounds], mut rows: Vec<u32>) -> Result<u32> {
        let bounds = rows
            .iter()
            .map(|row| source[*row as usize])
            .reduce(WorldBounds::union)
            .ok_or(Error::NumericOverflow)?;
        if rows.len() <= 8 {
            rows.sort_unstable();
            return self.push(MacroBvhNode::Leaf { bounds, rows });
        }
        let min = bounds.min_ticks();
        let max = bounds.max_ticks_exclusive();
        let axis = (0..3)
            .max_by_key(|axis| max[*axis] - min[*axis])
            .unwrap_or(0);
        rows.sort_unstable_by_key(|row| {
            let row_bounds = source[*row as usize];
            row_bounds.min_ticks()[axis] + row_bounds.max_ticks_exclusive()[axis]
        });
        let right = rows.split_off(rows.len() / 2);
        let left = self.build_node(source, rows)?;
        let right = self.build_node(source, right)?;
        self.push(MacroBvhNode::Branch {
            bounds,
            left,
            right,
        })
    }

    fn push(&mut self, node: MacroBvhNode) -> Result<u32> {
        let index = u32::try_from(self.nodes.len()).map_err(|_| Error::NumericOverflow)?;
        self.nodes.push(node);
        Ok(index)
    }

    fn query_bounds(&self, bounds: WorldBounds) -> Vec<u32> {
        let mut result = Vec::new();
        let Some(root) = self.root else {
            return result;
        };
        let mut pending = vec![root];
        while let Some(index) = pending.pop() {
            match &self.nodes[index as usize] {
                MacroBvhNode::Leaf {
                    bounds: node_bounds,
                    rows,
                } => {
                    if bounds_intersect(*node_bounds, bounds) {
                        result.extend(rows.iter().copied());
                    }
                }
                MacroBvhNode::Branch {
                    bounds: node_bounds,
                    left,
                    right,
                } => {
                    if bounds_intersect(*node_bounds, bounds) {
                        pending.push(*right);
                        pending.push(*left);
                    }
                }
            }
        }
        result.retain(|row| bounds_intersect(self.row_bounds[*row as usize], bounds));
        result.sort_unstable();
        result
    }

    fn rows_by_nearness(&self, point: DVec3) -> Vec<u32> {
        let mut rows = self.query_all();
        rows.sort_by(|left, right| {
            let left_distance = self.row_bounds(*left).map_or(f64::INFINITY, |bounds| {
                distance_squared_to_bounds(point, bounds)
            });
            let right_distance = self.row_bounds(*right).map_or(f64::INFINITY, |bounds| {
                distance_squared_to_bounds(point, bounds)
            });
            left_distance
                .total_cmp(&right_distance)
                .then_with(|| left.cmp(right))
        });
        rows
    }

    fn query_ray(&self, origin: DVec3, direction: DVec3, maximum: f64) -> Vec<(u32, f64)> {
        let mut result = Vec::new();
        let Some(root) = self.root else {
            return result;
        };
        let mut pending = vec![root];
        while let Some(index) = pending.pop() {
            let node = &self.nodes[index as usize];
            if ray_bounds_distance(origin, direction, node.bounds(), maximum).is_none() {
                continue;
            }
            match node {
                MacroBvhNode::Leaf { rows, .. } => {
                    result.extend(rows.iter().filter_map(|row| {
                        self.row_bounds(*row)
                            .and_then(|bounds| {
                                ray_bounds_distance(origin, direction, bounds, maximum)
                            })
                            .map(|distance| (*row, distance))
                    }));
                }
                MacroBvhNode::Branch { left, right, .. } => {
                    pending.push(*right);
                    pending.push(*left);
                }
            }
        }
        result.sort_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        result
    }

    fn query_all(&self) -> Vec<u32> {
        let Some(root) = self.root else {
            return Vec::new();
        };
        let mut pending = vec![root];
        let mut rows = Vec::new();
        while let Some(index) = pending.pop() {
            match &self.nodes[index as usize] {
                MacroBvhNode::Leaf { rows: leaf, .. } => rows.extend(leaf),
                MacroBvhNode::Branch { left, right, .. } => {
                    pending.push(*right);
                    pending.push(*left);
                }
            }
        }
        rows
    }

    fn row_bounds(&self, row: u32) -> Option<WorldBounds> {
        self.row_bounds.get(row as usize).copied()
    }
}

fn bounds_intersect(left: WorldBounds, right: WorldBounds) -> bool {
    let left_min = left.min_ticks();
    let left_max = left.max_ticks_exclusive();
    let right_min = right.min_ticks();
    let right_max = right.max_ticks_exclusive();
    (0..3).all(|axis| left_min[axis] < right_max[axis] && right_min[axis] < left_max[axis])
}

fn distance_squared_to_bounds(point: DVec3, bounds: WorldBounds) -> f64 {
    let minimum = ticks_to_meters(bounds.min_ticks());
    let maximum = ticks_to_meters(bounds.max_ticks_exclusive());
    (0..3)
        .map(|axis| {
            let delta = if point[axis] < minimum[axis] {
                minimum[axis] - point[axis]
            } else if point[axis] > maximum[axis] {
                point[axis] - maximum[axis]
            } else {
                0.0
            };
            delta * delta
        })
        .sum()
}

fn ray_bounds_distance(
    origin: DVec3,
    direction: DVec3,
    bounds: WorldBounds,
    maximum: f64,
) -> Option<f64> {
    let minimum = ticks_to_meters(bounds.min_ticks());
    let maximum_bounds = ticks_to_meters(bounds.max_ticks_exclusive());
    let mut enter = 0.0_f64;
    let mut exit = maximum;
    for axis in 0..3 {
        if direction[axis] == 0.0 {
            if origin[axis] < minimum[axis] || origin[axis] > maximum_bounds[axis] {
                return None;
            }
            continue;
        }
        let inverse = direction[axis].recip();
        let first = (minimum[axis] - origin[axis]) * inverse;
        let second = (maximum_bounds[axis] - origin[axis]) * inverse;
        enter = enter.max(first.min(second));
        exit = exit.min(first.max(second));
        if exit < enter {
            return None;
        }
    }
    (enter <= maximum).then_some(enter)
}

fn ticks_to_meters(ticks: [i128; 3]) -> DVec3 {
    DVec3::new(ticks[0] as f64, ticks[1] as f64, ticks[2] as f64)
        / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER)
}

#[cfg(test)]
mod tests {
    use saffron_spatial::{DecisionScalar, QuantizedLocalPosition, SourceLevel, UnitInterval};

    use super::*;
    use crate::{
        ArtifactSectionCodec, CookPlatformProfile, CookVersionSet, CookWorkActual,
        CookWorkEstimate, ManifestCellSection, ManifestSpeciesCount, PlantPoint,
        QuantizedOrientation, VegetationCellArtifactHeader, VegetationCellSection,
        VegetationManifestPlant, write_vegetation_cell_artifact,
    };

    fn point() -> PlantPoint {
        let position = WorldPosition::new(
            WorldCellKey::base(0, 0, 0),
            QuantizedLocalPosition::new([10, 20, 30]).unwrap(),
        )
        .unwrap();
        PlantPoint {
            id: PlantId::explicit([1; 16]).unwrap(),
            owner: WorldCellKey::base(0, 0, 0),
            position,
            orientation: QuantizedOrientation::identity(),
            scale: [DecisionScalar::from_bits(65_536); 3],
            bounds: WorldBounds::new([0, 0, 0], [100, 100, 100]).unwrap(),
            family: Uuid(7),
            variation: 0,
            lifecycle: PlantLifecycle::Mature,
            phenotype: 2,
            representation_class: 1,
            deterministic_key: 3,
            candidate: 4,
            parent: None,
            colony: None,
            ecology_tick: 5,
            health: UnitInterval::ONE,
            moisture: UnitInterval::ONE,
            fuel: UnitInterval::ONE,
            phenology: UnitInterval::ZERO,
            flags: PlantFlags::AUTHORED,
            interaction_policy: InteractionPolicy::Interactive,
            provenance: 0,
            attachment: None,
            surface_projection: [DecisionScalar::from_bits(0); 3],
        }
    }

    fn fixture() -> (VegetationWorld, Vec<u8>, PlantId) {
        let platform = CookPlatformProfile {
            target: "aarch64-apple-darwin".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "rust-1.96".to_owned(),
            features: vec!["canonical-fixed".to_owned()],
        };
        let point = point();
        let columns = PlantPointColumns::from_points(vec![point.clone()]).unwrap();
        let sections = vec![
            VegetationCellSection::raw(
                VegetationCellSectionKind::MacroPoints,
                columns.canonical_bytes().unwrap(),
            ),
            VegetationCellSection::raw(
                VegetationCellSectionKind::CollisionInputs,
                [b"SVEGCOL1".as_slice(), &0_u64.to_be_bytes()].concat(),
            ),
        ];
        let bytes = write_vegetation_cell_artifact(
            VegetationCellArtifactHeader {
                cell: point.owner,
                cook_key: ContentHash::new([8; 32]),
                platform_profile: platform.identity().unwrap(),
            },
            &sections,
        )
        .unwrap();
        let index = VegetationCellArtifactIndex::open(&bytes).unwrap();
        let mut manifest = VegetationBaseManifest::current(
            Uuid(1),
            Uuid(2),
            ContentHash::new([3; 32]),
            CookVersionSet::current(),
            platform,
            ContentHash::new([4; 32]),
        );
        manifest.plants.push(VegetationManifestPlant {
            family: point.family,
            tags: vec![PlantTagId::new(11).unwrap(), PlantTagId::new(13).unwrap()],
            source_hash: ContentHash::new([5; 32]),
            artifact_hash: ContentHash::new([6; 32]),
            local_bounds_min: [DecisionScalar::from_bits(-65_536); 3],
            local_bounds_max: [DecisionScalar::from_bits(65_536); 3],
            variation_count: 1,
            phenotype_count: 3,
        });
        manifest.cells.push(VegetationManifestCell {
            cell: point.owner,
            bounds: point.owner.bounds(),
            artifact_hash: ContentHash::of(&bytes),
            payload_hash: index.payload_hash,
            dependencies: Vec::new(),
            species_counts: vec![ManifestSpeciesCount {
                family: point.family,
                macro_count: 1,
                micro_count: 0,
            }],
            macro_count: 1,
            micro_count: 0,
            resident_memory_bytes: index.sections.iter().map(|value| value.decoded_size).sum(),
            stored_bytes: bytes.len() as u64,
            estimate: CookWorkEstimate::default(),
            actual: CookWorkActual::default(),
            sections: index
                .sections
                .iter()
                .map(|section| ManifestCellSection {
                    kind: section.kind,
                    version: section.version,
                    codec: ArtifactSectionCodec::Raw,
                    alignment: section.alignment,
                    stored_size: section.stored_size,
                    decoded_size: section.decoded_size,
                    content_hash: section.content_hash,
                })
                .collect(),
        });
        let world = VegetationWorld::new(manifest, VegetationResidencyBudgets::UNLIMITED).unwrap();
        (world, bytes, point.id)
    }

    fn source() -> SpatialSource {
        SpatialSource {
            id: SpatialSourceId(1),
            revision: 1,
            position: WorldPosition::origin(),
            velocity_mps: DVec3::ZERO,
            prediction_seconds: 0.0,
            levels: vec![SourceLevel {
                level: 0,
                load_radius_cells: 0,
                cleanup_radius_cells: 1,
            }],
            facets: ResidencyMask::one(ResidencyFacet::Physics),
            priority: 10,
        }
    }

    #[test]
    fn load_query_unload_and_reload_preserve_persistent_tombstones() {
        let (mut world, artifact, plant) = fixture();
        world.update_source(source()).unwrap();
        let report = world.residency_report().unwrap();
        assert_eq!(report.pending.len(), 1);
        assert_eq!(
            report.pending[0].facets,
            ResidencyMask::one(ResidencyFacet::Physics)
        );

        let staged = world
            .begin_load(
                WorldCellKey::base(0, 0, 0),
                ResidencyMask::one(ResidencyFacet::Physics),
            )
            .unwrap()
            .stage(&artifact)
            .unwrap();
        assert!(world.publish_staged(staged).unwrap());
        let found = world.find_plant(plant).unwrap().unwrap();
        assert_eq!(
            found.tags,
            vec![PlantTagId::new(11).unwrap(), PlantTagId::new(13).unwrap()]
        );

        world
            .apply_confirmed_mutations(&[VegetationMutationRecord {
                header: crate::MutationHeader {
                    cell: WorldCellKey::base(0, 0, 0),
                    transaction: 1,
                    authority: 2,
                    logical_tick: 3,
                    idempotency_key: 4,
                    base_revision: None,
                },
                mutation: crate::VegetationMutation::Tombstone { plant },
            }])
            .unwrap();
        assert!(world.find_plant(plant).unwrap().is_none());
        assert!(matches!(
            world.resolve_handle(found.handle),
            Err(Error::StaleGeneration { .. })
        ));

        let snapshot = world.export_state_snapshot().unwrap();
        let (mut restored, restored_artifact, restored_plant) = fixture();
        restored.import_state_snapshot(&snapshot).unwrap();
        restored.update_source(source()).unwrap();
        let staged = restored
            .begin_load(
                WorldCellKey::base(0, 0, 0),
                ResidencyMask::one(ResidencyFacet::Physics),
            )
            .unwrap()
            .stage(&restored_artifact)
            .unwrap();
        restored.publish_staged(staged).unwrap();
        assert!(restored.find_plant(restored_plant).unwrap().is_none());

        assert!(world.remove_source(SpatialSourceId(1)).unwrap());
        assert_eq!(
            world
                .cell_snapshot(WorldCellKey::base(0, 0, 0))
                .unwrap()
                .resident_facets(),
            ResidencyMask::NONE
        );
        world.update_source(source()).unwrap();
        let staged = world
            .begin_load(
                WorldCellKey::base(0, 0, 0),
                ResidencyMask::one(ResidencyFacet::Physics),
            )
            .unwrap()
            .stage(&artifact)
            .unwrap();
        assert!(world.publish_staged(staged).unwrap());
        assert!(world.find_plant(plant).unwrap().is_none());
    }

    #[test]
    fn prediction_overlay_is_transient_until_authority_confirmation() {
        let (mut world, artifact, plant) = fixture();
        world.update_source(source()).unwrap();
        let staged = world
            .begin_load(
                WorldCellKey::base(0, 0, 0),
                ResidencyMask::one(ResidencyFacet::Physics),
            )
            .unwrap()
            .stage(&artifact)
            .unwrap();
        world.publish_staged(staged).unwrap();
        let record = VegetationMutationRecord {
            header: crate::MutationHeader {
                cell: WorldCellKey::base(0, 0, 0),
                transaction: 10,
                authority: 20,
                logical_tick: 30,
                idempotency_key: 40,
                base_revision: None,
            },
            mutation: crate::VegetationMutation::Tombstone { plant },
        };

        world
            .apply_prediction(std::slice::from_ref(&record))
            .unwrap();
        assert_eq!(world.prediction_count(), 1);
        assert!(world.find_plant(plant).unwrap().is_none());
        assert!(world.persistent_state().cells().is_empty());

        let snapshot = world.export_state_snapshot().unwrap();
        let (mut restored, restored_artifact, _) = fixture();
        restored.import_state_snapshot(&snapshot).unwrap();
        restored.update_source(source()).unwrap();
        let staged = restored
            .begin_load(
                WorldCellKey::base(0, 0, 0),
                ResidencyMask::one(ResidencyFacet::Physics),
            )
            .unwrap()
            .stage(&restored_artifact)
            .unwrap();
        restored.publish_staged(staged).unwrap();
        assert!(restored.find_plant(plant).unwrap().is_some());

        assert!(world.reject_prediction(10).unwrap());
        assert_eq!(world.prediction_count(), 0);
        assert!(world.find_plant(plant).unwrap().is_some());

        world
            .apply_prediction(std::slice::from_ref(&record))
            .unwrap();
        world.confirm_prediction(10).unwrap();
        assert_eq!(world.prediction_count(), 0);
        assert!(world.find_plant(plant).unwrap().is_none());
        assert!(!world.persistent_state().cells().is_empty());
    }

    #[test]
    fn spatial_queries_filter_tags_and_return_only_stable_identity() {
        let (mut world, artifact, plant) = fixture();
        world.update_source(source()).unwrap();
        let staged = world
            .begin_load(
                WorldCellKey::base(0, 0, 0),
                ResidencyMask::one(ResidencyFacet::Physics),
            )
            .unwrap()
            .stage(&artifact)
            .unwrap();
        world.publish_staged(staged).unwrap();
        let filter = VegetationQueryFilter {
            required_tags: BTreeSet::from([PlantTagId::new(13).unwrap()]),
            ..VegetationQueryFilter::default()
        };
        let bounds = WorldBounds::new([0, 0, 0], [50, 50, 50]).unwrap();
        assert_eq!(world.query_bounds(bounds, &filter).unwrap()[0].plant, plant);
        assert_eq!(
            world
                .query_radius(WorldPosition::origin(), 1.0, &filter)
                .unwrap()[0]
                .plant,
            plant
        );
        let ray = VegetationQueryRay::new(WorldPosition::origin(), DVec3::X, 1.0).unwrap();
        assert_eq!(world.query_ray(ray, &filter).unwrap()[0].plant.plant, plant);
        assert_eq!(
            world
                .query_nearest(WorldPosition::origin(), Some(1.0), &filter)
                .unwrap()
                .unwrap()
                .plant
                .plant,
            plant
        );
    }

    #[test]
    fn source_budget_limits_admission_without_losing_demand() {
        let (mut world, _, _) = fixture();
        world.budgets.physics = 1;
        world.update_source(source()).unwrap();
        let report = world.residency_report().unwrap();
        assert_eq!(report.source_count, 1);
        assert!(report.requested_bytes[ResidencyFacet::Physics as usize] > 1);
        assert_eq!(report.resident_bytes[ResidencyFacet::Physics as usize], 0);
        assert!(report.pending.is_empty());
    }

    #[test]
    fn corrupt_artifact_never_reaches_publication() {
        let (mut world, mut artifact, _) = fixture();
        world.update_source(source()).unwrap();
        let load = world
            .begin_load(
                WorldCellKey::base(0, 0, 0),
                ResidencyMask::one(ResidencyFacet::Physics),
            )
            .unwrap();
        let last = artifact.len() - 1;
        artifact[last] ^= 1;
        assert!(matches!(
            load.stage(&artifact),
            Err(Error::ArtifactHashMismatch { .. })
        ));
        assert_eq!(
            world
                .cell_snapshot(WorldCellKey::base(0, 0, 0))
                .unwrap()
                .resident_facets(),
            ResidencyMask::NONE
        );
    }

    #[test]
    fn residency_revision_discards_late_staged_work() {
        let (mut world, artifact, _) = fixture();
        world.update_source(source()).unwrap();
        let staged = world
            .begin_load(
                WorldCellKey::base(0, 0, 0),
                ResidencyMask::one(ResidencyFacet::Physics),
            )
            .unwrap()
            .stage(&artifact)
            .unwrap();
        let mut moved = source();
        moved.revision = 2;
        moved.position = WorldPosition::from_world_meters(DVec3::new(64.0, 0.0, 0.0)).unwrap();
        world.update_source(moved).unwrap();
        assert!(!world.publish_staged(staged).unwrap());
        assert_eq!(
            world
                .cell_snapshot(WorldCellKey::base(0, 0, 0))
                .unwrap()
                .resident_facets(),
            ResidencyMask::NONE
        );
    }
}
