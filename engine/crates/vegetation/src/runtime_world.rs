//! Immutable runtime cell generations, facet residency, and vegetation queries.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use glam::DVec3;
use saffron_core::Uuid;
use saffron_spatial::{
    DecisionScalar, GenerationSlot, GenerationToken, ResidencyFacet, ResidencyManager,
    ResidencyMask, ResidencySnapshot, SpatialSource, SpatialSourceId, UnitInterval, WorldBounds,
    WorldCellKey, WorldPosition,
};

use crate::{
    ContentHash, DisturbanceTileKey, EcologyCatchUp, EcologyCatchUpReport, EcologyInfluence,
    EcologyPlantState, EcologyRegion, EcologyRegionState, EcologyRelation, EcologyRelations,
    EcologySpeciesRules, Error, InteractionPolicy, MicroFieldTile, MutationHeader, PlantFlags,
    PlantId, PlantLifecycle, PlantPoint, PlantPointColumns, PlantTagId, ProvenanceHandle,
    ProvenanceRecord, ProvenanceTable, QuantizedOrientation, Result, SaveStateEnvelope,
    VEGETATION_ARTIFACT_DECODE_LIMITS, VegetationBaseManifest, VegetationCellArtifactIndex,
    VegetationCellFacet, VegetationCellSectionKind, VegetationCellState, VegetationManifestCell,
    VegetationMutationRecord, VegetationRejectionDiagnosticsFacet, VegetationState,
    VegetationStateBinding, VegetationTransition, advance_region, decode_vegetation_cell_facet,
    dependency_regions, reduce_mutations,
};

/// Authority stamped on every mutation the ecology simulation commits.
const ECOLOGY_AUTHORITY: u128 = 0x5361_6666_726f_6e5f_4563_6f6c_6f67_7901;

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
    /// Monotonic biological tick.
    pub ecology_tick: u64,
    /// Generation-tagged lookup handle.
    pub handle: VegetationPlantHandle,
    /// Exact world position.
    pub position: WorldPosition,
    /// Quantized orientation.
    pub orientation: QuantizedOrientation,
    /// Q15.16 local scale.
    pub scale: [DecisionScalar; 3],
    /// Conservative world bounds.
    pub bounds: WorldBounds,
    /// Plant-family identity.
    pub family: Uuid,
    /// Canonically ordered family tags.
    pub tags: Vec<PlantTagId>,
    /// Biological lifecycle state.
    pub lifecycle: PlantLifecycle,
    /// Family variation, which selects the individual's geometry.
    pub variation: u32,
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
    /// Whether the plant is alight.
    pub ignited: bool,
    /// Compact accepted-point provenance when the editing facet is resident.
    pub provenance: Option<ProvenanceRecord>,
}

/// What a volume holds, for a system that needs to know whether it will burn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VegetationCombustionSample {
    /// Plants matched by the sample.
    pub plants: u32,
    /// How many of them are alight.
    pub ignited: u32,
    /// Mean combustible fuel.
    pub fuel: UnitInterval,
    /// Mean persistent moisture.
    pub moisture: UnitInterval,
    /// Mean health.
    pub health: UnitInterval,
    /// Ground covered by the matched plants, as a share of one cell.
    pub occupancy: UnitInterval,
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

/// One nonpersistent micro-field paint-feedback hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VegetationMicroHit {
    /// World-space hit position in metres.
    pub position: saffron_geometry::glam::DVec3,
    /// Metric distance from the ray origin.
    pub distance_m: f64,
    /// The field's plant family.
    pub family: Uuid,
    /// The owning cell.
    pub cell: WorldCellKey,
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
        let index =
            validate_artifact_against_manifest(self.platform_profile, &self.manifest_cell, bytes)?;
        let mut decoded = self.current.facets.clone();
        let resident = union_masks(self.current.resident, self.facets);
        for kind in required_sections(self.facets) {
            let section = index
                .section(bytes, kind)?
                .ok_or_else(|| Error::ArtifactFormat {
                    format: ".svegcell",
                    field: format!("missing runtime facet {}", kind as u16),
                })?;
            decoded.insert(kind, decode_vegetation_cell_facet(kind, section.as_ref())?);
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

    /// Per-plant navigation contribution rows when the navigation facet is resident.
    pub fn navigation_contributions(&self) -> Option<&[crate::VegetationNavigationContribution]> {
        match self
            .facets
            .get(&VegetationCellSectionKind::NavigationContributions)
        {
            Some(VegetationCellFacet::NavigationContributions(rows)) => Some(rows),
            _ => None,
        }
    }

    /// Per-plant collision derivation rows when the physics facet is resident.
    pub fn collision_inputs(&self) -> Option<&[crate::VegetationCollisionInput]> {
        match self.facets.get(&VegetationCellSectionKind::CollisionInputs) {
            Some(VegetationCellFacet::CollisionInputs(rows)) => Some(rows),
            _ => None,
        }
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
            ecology_tick: point.ecology_tick,
            handle: VegetationPlantHandle {
                plant: point.id,
                generation: self.id,
            },
            position: point.position,
            orientation: point.orientation,
            scale: point.scale,
            bounds: point.bounds,
            family: point.family,
            tags,
            lifecycle: point.lifecycle,
            variation: point.variation,
            phenotype: point.phenotype,
            interaction_policy: point.interaction_policy,
            health: point.health,
            moisture: point.moisture,
            fuel: point.fuel,
            ignited: point.flags.contains(PlantFlags::IGNITED),
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
    /// Monotonic counter bumped whenever a plant this cell owns enters or leaves bulk
    /// suppression, so render and collision adapters re-derive the cell.
    bulk_revision: u64,
}

/// How many committed transitions the event ring retains before evicting the oldest. A consumer
/// whose cursor falls behind that tail is told to resync rather than handed a gap.
pub const VEGETATION_EVENT_RING_CAP: usize = 4096;

/// One committed vegetation transition, sequence-stamped for cursor-based delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationEvent {
    /// Monotonic sequence number within this world.
    pub seq: u64,
    /// The transition itself.
    pub transition: VegetationTransition,
}

/// A cursor read of the event ring, plus the metadata a stale cursor needs to notice it missed
/// evicted events.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VegetationEventDrain {
    /// Events newer than the cursor, oldest first.
    pub events: Vec<VegetationEvent>,
    /// The newest sequence number the ring has stamped.
    pub high_water_seq: u64,
    /// The oldest sequence number still retained, or zero when the ring is empty.
    pub oldest_seq: u64,
    /// The cursor was older than the retained tail, so events were missed: resync.
    pub overflowed: bool,
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
    /// Typed transitions committed since the world came up, newest last.
    event_ring: VecDeque<VegetationEvent>,
    /// The newest sequence number the ring has stamped.
    event_seq: u64,
    /// Plants whose bulk representation is suppressed because a promoted entity owns them.
    /// Keyed by identity rather than held in an immutable published generation, so a cell
    /// unload, republication, or reload never loses or duplicates the suppression.
    bulk_suppressed: BTreeSet<PlantId>,
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
            event_ring: VecDeque::new(),
            event_seq: 0,
            bulk_suppressed: BTreeSet::new(),
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

    /// Iterates every resident cell's current published generation, keeping each alive
    /// for the reader — the render adapter's snapshot walk.
    pub fn resident_cells(
        &self,
    ) -> impl Iterator<Item = (WorldCellKey, Arc<VegetationCellGeneration>)> + '_ {
        self.cells
            .iter()
            .map(|(cell, entry)| (*cell, entry.slot.read()))
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

    /// Reads every committed transition with `seq > since`, oldest first, without consuming the
    /// ring — one cursor per consumer (scripts, VFX, audio, quests, navigation).
    #[must_use]
    pub fn drain_events(&self, since: u64) -> VegetationEventDrain {
        let events: Vec<VegetationEvent> = self
            .event_ring
            .iter()
            .filter(|event| event.seq > since)
            .copied()
            .collect();
        let oldest_seq = self.event_ring.front().map_or(0, |event| event.seq);
        VegetationEventDrain {
            events,
            high_water_seq: self.event_seq,
            oldest_seq,
            overflowed: oldest_seq > 0 && since + 1 < oldest_seq,
        }
    }

    fn record_transitions(&mut self, transitions: &[VegetationTransition]) {
        for transition in transitions {
            self.event_seq += 1;
            if self.event_ring.len() >= VEGETATION_EVENT_RING_CAP {
                self.event_ring.pop_front();
            }
            self.event_ring.push_back(VegetationEvent {
                seq: self.event_seq,
                transition: *transition,
            });
        }
    }

    /// Suppresses `plant`'s bulk representation: the render and collision adapters skip it from
    /// this point on, because a promoted entity owns it. Returns the plant's owner cell.
    ///
    /// # Errors
    ///
    /// [`Error::PlantNotResident`] when no resident macro facet carries the identity, or
    /// [`Error::Mutation`] when it is already suppressed (two owners is the one thing this
    /// authority exists to prevent).
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

    /// Nearest micro-field ground hit along a ray: the first resident cell whose
    /// floor-plane crossing lands on a texel with nonzero density. The hit is
    /// nonpersistent paint feedback — micro blades have no identity.
    pub fn query_micro_ray(&self, ray: VegetationQueryRay) -> Option<VegetationMicroHit> {
        let origin = ray.origin.world_meters();
        let mut nearest: Option<VegetationMicroHit> = None;
        for (cell, generation) in self.resident_cells() {
            let Some(tiles) = generation.micro_fields() else {
                continue;
            };
            let bounds = cell.bounds();
            let tick = 1.0 / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER);
            let min = bounds.min_ticks().map(|value| value as f64 * tick);
            let max = bounds
                .max_ticks_exclusive()
                .map(|value| value as f64 * tick);
            if ray.direction.y.abs() < 1e-9 {
                continue;
            }
            let t = (min[1] - origin.y) / ray.direction.y;
            if t < 0.0 || t > ray.max_distance_m {
                continue;
            }
            let point = origin + ray.direction * t;
            if point.x < min[0] || point.x >= max[0] || point.z < min[2] || point.z >= max[2] {
                continue;
            }
            for tile in tiles {
                let dims = tile.dimensions;
                let texel_x =
                    ((point.x - min[0]) / (max[0] - min[0]) * f64::from(dims[0])).floor() as u32;
                let texel_z =
                    ((point.z - min[2]) / (max[2] - min[2]) * f64::from(dims[2])).floor() as u32;
                let texel_x = texel_x.min(dims[0].saturating_sub(1));
                let texel_z = texel_z.min(dims[2].saturating_sub(1));
                let index = (texel_x + dims[0] * dims[1] * texel_z) as usize;
                if tile.density.get(index).is_none_or(|density| *density == 0) {
                    continue;
                }
                if nearest
                    .as_ref()
                    .is_none_or(|current| t < current.distance_m)
                {
                    nearest = Some(VegetationMicroHit {
                        position: point,
                        distance_m: t,
                        family: tile.family,
                        cell,
                    });
                }
            }
        }
        nearest
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

    /// Samples the combustible state of a volume: what is growing there, how much of it is alight,
    /// and how wet it is.
    ///
    /// This is the seam a fire system reads. Vegetation owns fuel, moisture, health, occupancy, and
    /// the persistent record of what is burning; heat propagation and smoke belong to the system
    /// that calls this and answers with [`VegetationMutation::Ignite`],
    /// [`VegetationMutation::Extinguish`], [`VegetationMutation::Burn`], and
    /// [`VegetationMutation::MoistureFuel`].
    ///
    /// # Errors
    ///
    /// Propagates [`Self::query_bounds`].
    pub fn combustion_sample(
        &self,
        bounds: WorldBounds,
        filter: &VegetationQueryFilter,
    ) -> Result<VegetationCombustionSample> {
        let plants = self.query_bounds(bounds, filter)?;
        if plants.is_empty() {
            return Ok(VegetationCombustionSample::default());
        }
        let mean = |total: u64| {
            UnitInterval::from_bits(u16::try_from(total / plants.len() as u64).unwrap_or(u16::MAX))
        };
        let sum = |select: fn(&VegetationPlantSnapshot) -> UnitInterval| {
            plants
                .iter()
                .map(|plant| u64::from(select(plant).bits()))
                .sum::<u64>()
        };
        let occupancy = plants
            .iter()
            .map(|plant| u64::from(canopy_share(plant.bounds, plant.position.cell()).bits()))
            .sum::<u64>();
        Ok(VegetationCombustionSample {
            plants: plants.len() as u32,
            ignited: plants.iter().filter(|plant| plant.ignited).count() as u32,
            fuel: mean(sum(|plant| plant.fuel)),
            moisture: mean(sum(|plant| plant.moisture)),
            health: mean(sum(|plant| plant.health)),
            occupancy: UnitInterval::from_bits(
                u16::try_from(occupancy).unwrap_or(UnitInterval::ONE.bits()),
            ),
        })
    }

    /// Advances biological time to `plan.target_tick` and catches the world's dependency regions
    /// up to it.
    ///
    /// World time moves first and unconditionally: biology has aged whether or not anything is
    /// loaded. Regions then execute the ticks they owe, one whole region at a time, and a region
    /// only runs while every cell it spans is resident — a region reading a neighbour that is not
    /// loaded would read stale ground and diverge from continuous simulation. Whatever the budget
    /// or residency leaves undone stays owed, in order, for the next call.
    ///
    /// # Errors
    ///
    /// Propagates the tick rules and the reducer, and fails when a region's cells disagree about
    /// which tick they have reached — that means state was assembled from mismatched checkpoints.
    pub fn advance_ecology(&mut self, plan: &EcologyCatchUp<'_>) -> Result<EcologyCatchUpReport> {
        self.persistent
            .ecology_mut()
            .advance_world_to(plan.target_tick)?;
        self.effective
            .ecology_mut()
            .advance_world_to(plan.target_tick)?;

        let regions = self.ecology_regions(plan.influence);

        let mut report = EcologyCatchUpReport {
            world_tick: plan.target_tick,
            regions: regions.len(),
            ..EcologyCatchUpReport::default()
        };
        let mut remaining = plan.budget.max_ticks;
        for region in &regions {
            if !self.region_is_resident(region) {
                report.regions_awaiting_residency += 1;
                continue;
            }
            let start = self.region_tick(region)?;
            let owed = plan.target_tick.saturating_sub(start);
            if owed == 0 {
                report.regions_caught_up += 1;
                continue;
            }
            let run = owed.min(u64::from(remaining));
            for tick in (start + 1)..=(start + run) {
                self.advance_region_one_tick(region, tick, plan)?;
                report.ticks_run += 1;
            }
            remaining -= u32::try_from(run).map_err(|_| Error::NumericOverflow)?;
            if run == owed {
                report.regions_caught_up += 1;
            } else {
                // Spent budget, or none left by the time this region came up. What it still owes
                // is owed, not lost: the next call resumes at the same tick.
                report.ticks_owed += owed - run;
            }
        }
        Ok(report)
    }

    /// Ecology rules per family, as the cook baked them into the manifest.
    ///
    /// A tick reads its species' rules from here rather than from the asset catalog: the manifest
    /// is the immutable thing the state is bound to, so the rules cannot drift from the state that
    /// was simulated under them.
    #[must_use]
    pub fn ecology_rules(&self) -> BTreeMap<u64, EcologySpeciesRules> {
        self.manifest
            .plants
            .iter()
            .map(|plant| (plant.family.value(), plant.ecology.rules))
            .collect()
    }

    /// Declared species relations, keyed by (subject family, other family).
    #[must_use]
    pub fn ecology_relations(&self) -> EcologyRelations {
        self.manifest
            .plants
            .iter()
            .flat_map(|plant| {
                plant.ecology.relations.iter().map(|relation| {
                    (
                        (plant.family.value(), relation.family.value()),
                        EcologyRelation {
                            kind: relation.kind,
                            strength: relation.strength,
                        },
                    )
                })
            })
            .collect()
    }

    /// The world's dependency regions under `influence`, in canonical order.
    ///
    /// A region spans every cell the cook planted plus every cell the simulation has planted into
    /// since: a seed that crossed a border into a cell the cook left empty is still a plant that
    /// ages.
    #[must_use]
    pub fn ecology_regions(&self, influence: EcologyInfluence) -> Vec<EcologyRegion> {
        let planted: BTreeSet<WorldCellKey> = self
            .manifest_cells
            .iter()
            .filter(|(_, cell)| cell.macro_count > 0)
            .map(|(key, _)| *key)
            .chain(self.persistent.cells().keys().copied())
            .collect();
        dependency_regions(&planted, influence.region_radius_cells())
    }

    /// Whether every cell `region` spans carries resident macro rows, which a tick requires.
    #[must_use]
    pub fn region_is_resident(&self, region: &EcologyRegion) -> bool {
        region
            .cells()
            .iter()
            .all(|cell| self.cell_has_resident_macro(*cell))
    }

    /// The tick a region has been simulated to.
    ///
    /// # Errors
    ///
    /// [`Error::Mutation`] when the region's cells disagree, which means state was assembled from
    /// mismatched checkpoints.
    pub fn ecology_region_tick(&self, region: &EcologyRegion) -> Result<u64> {
        self.region_tick(region)
    }

    /// Whether `cell` has a resident generation carrying macro rows to simulate.
    fn cell_has_resident_macro(&self, cell: WorldCellKey) -> bool {
        self.cells
            .get(&cell)
            .is_some_and(|entry| !entry.slot.read().macro_points.ids.is_empty())
    }

    /// The tick a region has been simulated to. Its cells advance together, so a disagreement is a
    /// corrupt checkpoint rather than something to paper over with a minimum.
    fn region_tick(&self, region: &EcologyRegion) -> Result<u64> {
        let ecology = self.persistent.ecology();
        let mut ticks = region.cells().iter().map(|cell| ecology.cell_tick(*cell));
        let first = ticks.next().unwrap_or_default();
        for tick in ticks {
            if tick != first {
                return Err(Error::Mutation(format!(
                    "dependency region spans cells at ticks {first} and {tick}; its cells must \
                     advance together"
                )));
            }
        }
        Ok(first)
    }

    /// Runs and commits one tick for one region: mutations through the one reducer, then the
    /// region's summaries published atomically.
    fn advance_region_one_tick(
        &mut self,
        region: &EcologyRegion,
        tick: u64,
        plan: &EcologyCatchUp<'_>,
    ) -> Result<()> {
        // Check the publication precondition before any mutation is committed, so the commit and
        // the tick generation cannot come apart: a refused tick leaves neither behind.
        let start = self.region_tick(region)?;
        if tick != start + 1 || tick > self.persistent.ecology().clock().tick() {
            return Err(Error::Mutation(format!(
                "ecology tick {tick} does not follow the region's tick {start} within world time \
                 {}",
                self.persistent.ecology().clock().tick()
            )));
        }
        let state = self.region_state(region)?;
        let result = advance_region(
            region,
            &state,
            tick,
            &crate::EcologyTickRules {
                map: leading_u128(self.manifest_identity.bytes()),
                influence: plan.influence,
                rules: plan.rules,
                relations: plan.relations,
                weather: plan.weather,
            },
        )?;

        let mut digest = b"saffron-anima/vegetation-ecology/transaction/v1".to_vec();
        digest.extend_from_slice(&tick.to_be_bytes());
        digest.extend_from_slice(&self.manifest_identity.bytes());
        for (cell, mutations) in &result.mutations {
            for coordinate in cell.coordinates() {
                digest.extend_from_slice(&coordinate.to_be_bytes());
            }
            digest.extend_from_slice(&(mutations.len() as u64).to_be_bytes());
        }
        let transaction = leading_u128(ContentHash::of(&digest).bytes());

        let mut records = Vec::new();
        let mut salt = 0_u128;
        for (cell, mutations) in result.mutations {
            for mutation in mutations {
                salt += 1;
                // A spread seed is owned by the cell it landed in, which is not always the cell
                // whose tick produced it.
                let cell = match &mutation {
                    crate::VegetationMutation::Planting(point)
                    | crate::VegetationMutation::AnchorAddition(point) => point.owner,
                    _ => cell,
                };
                records.push(VegetationMutationRecord {
                    header: MutationHeader {
                        cell,
                        transaction,
                        authority: ECOLOGY_AUTHORITY,
                        logical_tick: tick,
                        idempotency_key: transaction ^ (salt << 8),
                        base_revision: None,
                    },
                    mutation,
                });
            }
        }
        if !records.is_empty() {
            self.apply_confirmed_mutations(&records)?;
        }
        self.persistent
            .ecology_mut()
            .publish_region_tick(tick, &result.summaries)?;
        self.effective
            .ecology_mut()
            .publish_region_tick(tick, &result.summaries)
    }

    /// Reads a region's plants and boundary summaries out of the resident generations.
    fn region_state(&self, region: &EcologyRegion) -> Result<EcologyRegionState> {
        let mut state = EcologyRegionState {
            summaries: self.persistent.ecology().summaries().clone(),
            ..EcologyRegionState::default()
        };
        for &cell in region.cells() {
            let Some(entry) = self.cells.get(&cell) else {
                return Err(Error::UnknownRuntimeCell { cell });
            };
            let generation = entry.slot.read();
            let rows = generation.macro_points.row_count()?;
            let mut plants = Vec::with_capacity(rows);
            for row in 0..rows {
                let point = generation.macro_points.point(row)?;
                plants.push(EcologyPlantState {
                    plant: point.id,
                    family: point.family,
                    position: point.position,
                    lifecycle: point.lifecycle,
                    ecology_tick: point.ecology_tick,
                    health: point.health,
                    moisture: point.moisture,
                    fuel: point.fuel,
                    canopy: canopy_share(point.bounds, cell),
                });
            }
            plants.sort_unstable_by_key(|plant| plant.plant);
            state.plants.insert(cell, plants);
        }
        Ok(state)
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
                bulk_revision: 0,
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
) -> Result<VegetationCellArtifactIndex> {
    if ContentHash::of(bytes) != cell.artifact_hash {
        return Err(Error::ArtifactHashMismatch {
            format: ".svegcell",
            subject: "manifest artifact".to_owned(),
        });
    }
    let index = VegetationCellArtifactIndex::open(bytes, VEGETATION_ARTIFACT_DECODE_LIMITS)?;
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
    Ok(index)
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
            point.flags = if delta.ignited {
                point.flags.union(PlantFlags::IGNITED)
            } else {
                point.flags.difference(PlantFlags::IGNITED)
            };
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

/// The leading 16 bytes of a content hash, as the transaction and map identities want a `u128`.
fn leading_u128(bytes: [u8; 32]) -> u128 {
    let mut leading = [0_u8; 16];
    leading.copy_from_slice(&bytes[..16]);
    u128::from_be_bytes(leading)
}

/// A plant's share of its cell's ground, from the conservative bounds the cook produced.
///
/// Shade is an area effect, so the shade a cell casts is the sum of its plants' footprints against
/// the cell's own footprint. Integer ticks throughout: a canopy figure feeds simulated results, so
/// it may not vary with floating-point rounding.
fn canopy_share(plant: WorldBounds, cell: WorldCellKey) -> UnitInterval {
    let footprint = |bounds: WorldBounds| -> i128 {
        let minimum = bounds.min_ticks();
        let maximum = bounds.max_ticks_exclusive();
        (maximum[0] - minimum[0]).max(0) * (maximum[2] - minimum[2]).max(0)
    };
    let ground = footprint(cell.bounds());
    if ground <= 0 {
        return UnitInterval::ZERO;
    }
    let share = footprint(plant) * i128::from(UnitInterval::ONE.bits()) / ground;
    UnitInterval::from_bits(u16::try_from(share.clamp(0, i128::from(u16::MAX))).unwrap_or(u16::MAX))
}

#[cfg(test)]
mod tests {
    use saffron_spatial::{DecisionScalar, QuantizedLocalPosition, SourceLevel, UnitInterval};

    use super::*;
    use crate::{
        CookPlatformProfile, CookVersionSet, CookWorkActual, CookWorkEstimate, ManifestCellSection,
        ManifestSpeciesCount, PlantPoint, QuantizedOrientation, VegetationCellArtifactHeader,
        VegetationCellSection, VegetationManifestPlant, write_vegetation_cell_artifact,
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
            lifecycle: PlantLifecycle::Mature,
            variation: 0,
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
        fixture_with_micro(false)
    }

    fn fixture_with_micro(micro: bool) -> (VegetationWorld, Vec<u8>, PlantId) {
        let platform = CookPlatformProfile {
            target: "aarch64-apple-darwin".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "rust-1.96".to_owned(),
            features: vec!["canonical-fixed".to_owned()],
        };
        let point = point();
        let columns = PlantPointColumns::from_points(vec![point.clone()]).unwrap();
        let mut sections = vec![
            VegetationCellSection::new(
                VegetationCellSectionKind::MacroPoints,
                columns.canonical_bytes().unwrap(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::CollisionInputs,
                [b"SVEGCOL1".as_slice(), &0_u64.to_be_bytes()].concat(),
            ),
        ];
        if micro {
            let mut density = vec![32_768_u16; 16];
            density[0] = 0;
            let tile = crate::MicroFieldTile {
                cell: point.owner,
                family: point.family,
                dimensions: [4, 1, 4],
                density,
                attributes: BTreeMap::new(),
                reconstruction_seed: 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10,
            };
            sections.push(VegetationCellSection::new(
                VegetationCellSectionKind::MicroFields,
                crate::encode_vegetation_micro_fields(std::slice::from_ref(&tile)).unwrap(),
            ));
            sections.push(VegetationCellSection::new(
                VegetationCellSectionKind::RenderReferences,
                [b"SVEGRRF1".as_slice(), &0_u64.to_be_bytes()].concat(),
            ));
            sections.push(VegetationCellSection::new(
                VegetationCellSectionKind::RenderBounds,
                [b"SVEGRBD1".as_slice(), &0_u64.to_be_bytes()].concat(),
            ));
        }
        let bytes = write_vegetation_cell_artifact(
            VegetationCellArtifactHeader {
                cell: point.owner,
                cook_key: ContentHash::new([8; 32]),
                platform_profile: platform.identity().unwrap(),
            },
            &sections,
        )
        .unwrap();
        let index =
            VegetationCellArtifactIndex::open(&bytes, VEGETATION_ARTIFACT_DECODE_LIMITS).unwrap();
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
            ecology: crate::PlantEcologyDeclaration::default(),
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
                    codec: section.codec,
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
        source_with_facet(ResidencyFacet::Physics)
    }

    fn source_with_facet(facet: ResidencyFacet) -> SpatialSource {
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
            facets: ResidencyMask::one(facet),
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
    fn micro_ray_lands_on_the_nearest_dense_floor_texel() {
        let (mut world, artifact, _) = fixture_with_micro(true);
        world
            .update_source(source_with_facet(ResidencyFacet::Render))
            .unwrap();
        let staged = world
            .begin_load(
                WorldCellKey::base(0, 0, 0),
                ResidencyMask::one(ResidencyFacet::Render),
            )
            .unwrap()
            .stage(&artifact)
            .unwrap();
        world.publish_staged(staged).unwrap();

        let down = |x: f64, z: f64, maximum: f64| {
            VegetationQueryRay::new(
                WorldPosition::from_world_meters(DVec3::new(x, 10.0, z)).unwrap(),
                -DVec3::Y,
                maximum,
            )
            .unwrap()
        };
        let hit = world.query_micro_ray(down(40.0, 40.0, 100.0)).unwrap();
        assert_eq!(hit.position, DVec3::new(40.0, 0.0, 40.0));
        assert_eq!(hit.distance_m, 10.0);
        assert_eq!(hit.family, Uuid(7));
        assert_eq!(hit.cell, WorldCellKey::base(0, 0, 0));
        // The (0, 0) texel carries zero density; the crossing there reports no hit.
        assert!(world.query_micro_ray(down(8.0, 8.0, 100.0)).is_none());
        // The floor crossing past the ray's maximum reports no hit.
        assert!(world.query_micro_ray(down(40.0, 40.0, 5.0)).is_none());
        let level = VegetationQueryRay::new(
            WorldPosition::from_world_meters(DVec3::new(40.0, 10.0, 40.0)).unwrap(),
            DVec3::X,
            100.0,
        )
        .unwrap();
        assert!(world.query_micro_ray(level).is_none());
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

    fn loaded_world() -> VegetationWorld {
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
        assert!(world.publish_staged(staged).unwrap());
        world
    }

    fn catch_up_plan(target: u64, max_ticks: u32) -> EcologyCatchUp<'static> {
        static RULES: std::sync::OnceLock<BTreeMap<u64, crate::EcologySpeciesRules>> =
            std::sync::OnceLock::new();
        static RELATIONS: std::sync::OnceLock<EcologyRelations> = std::sync::OnceLock::new();
        EcologyCatchUp {
            target_tick: target,
            budget: crate::EcologyCatchUpBudget { max_ticks },
            influence: crate::EcologyInfluence::default(),
            rules: RULES
                .get_or_init(|| BTreeMap::from([(7, crate::EcologySpeciesRules::default())])),
            relations: RELATIONS.get_or_init(EcologyRelations::new),
            weather: crate::EcologyWeather {
                water: UnitInterval::from_bits(40_000),
                warmth: UnitInterval::from_bits(45_000),
            },
        }
    }

    /// The phase's central claim: biology reached by running every tick as time passes and biology
    /// reached by jumping time and catching up land on the same bytes.
    #[test]
    fn catch_up_equals_continuous_simulation() {
        let mut continuous = loaded_world();
        for tick in 1..=8_u64 {
            let plan = catch_up_plan(tick, 1);
            let report = continuous.advance_ecology(&plan).unwrap();
            assert_eq!(report.ticks_run, 1);
            assert_eq!(report.regions_caught_up, 1);
        }

        let mut caught_up = loaded_world();
        let plan = catch_up_plan(8, 8);
        let report = caught_up.advance_ecology(&plan).unwrap();
        assert_eq!(report.ticks_run, 8);
        assert_eq!(report.ticks_owed, 0);
        assert_eq!(report.regions, 1);

        assert!(
            !caught_up.persistent_state().cells().is_empty(),
            "the run committed real plant changes, so the comparison has something to compare",
        );
        assert_eq!(
            continuous
                .persistent_state()
                .ecology()
                .checkpoint_identity(),
            caught_up.persistent_state().ecology().checkpoint_identity(),
        );
        assert_eq!(
            continuous.persistent_state().canonical_bytes().unwrap(),
            caught_up.persistent_state().canonical_bytes().unwrap(),
            "both routes committed the same persistent state, byte for byte",
        );
    }

    /// A budget bounds the work per call. It delays when the region is readable; it never drops a
    /// tick or changes where the region ends up.
    #[test]
    fn a_catch_up_budget_delays_readiness_without_changing_results() {
        let mut world = loaded_world();
        let plan = catch_up_plan(8, 3);
        let first = world.advance_ecology(&plan).unwrap();
        assert_eq!(first.ticks_run, 3);
        assert_eq!(first.ticks_owed, 5);
        assert_eq!(first.regions_caught_up, 0);
        assert!(
            !world
                .persistent_state()
                .ecology()
                .is_caught_up(WorldCellKey::base(0, 0, 0)),
            "a region behind world time is not simulation-ready",
        );

        let second = world.advance_ecology(&plan).unwrap();
        assert_eq!(second.ticks_run, 3);
        let third = world.advance_ecology(&plan).unwrap();
        assert_eq!(third.ticks_run, 2, "the remainder, not a whole budget");
        assert_eq!(third.ticks_owed, 0);
        assert_eq!(third.regions_caught_up, 1);

        let mut unbudgeted = loaded_world();
        let whole = catch_up_plan(8, 64);
        unbudgeted.advance_ecology(&whole).unwrap();
        assert_eq!(
            world.persistent_state().canonical_bytes().unwrap(),
            unbudgeted.persistent_state().canonical_bytes().unwrap(),
            "three budgeted calls and one unbudgeted call agree",
        );
    }

    /// World time advances whether or not anything is loaded, but a region whose cells are not
    /// resident owes its ticks rather than running them against absent neighbours.
    #[test]
    fn a_region_awaiting_residency_owes_its_ticks() {
        let (mut world, artifact, _) = fixture();
        let plan = catch_up_plan(5, 16);
        let report = world.advance_ecology(&plan).unwrap();
        assert_eq!(report.regions_awaiting_residency, 1);
        assert_eq!(report.ticks_run, 0);
        assert_eq!(report.world_tick, 5);
        assert_eq!(world.persistent_state().ecology().clock().tick(), 5);

        // Loading the cell lets the same call finish the owed ticks, reaching the state a world
        // that never unloaded would hold.
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
        let after = world.advance_ecology(&plan).unwrap();
        assert_eq!(after.ticks_run, 5);
        assert_eq!(after.regions_caught_up, 1);

        let mut resident_throughout = loaded_world();
        let same = catch_up_plan(5, 16);
        resident_throughout.advance_ecology(&same).unwrap();
        assert_eq!(
            world.persistent_state().canonical_bytes().unwrap(),
            resident_throughout
                .persistent_state()
                .canonical_bytes()
                .unwrap(),
            "the residency path taken to a tick does not change the tick's result",
        );
    }
}
