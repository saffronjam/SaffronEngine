//! Strict typed decoders for independently resident `.svegcell` facets.

use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_spatial::{
    DecisionScalar, FieldChannel, FieldDerivative, QuantizedLocalPosition, SignedUnit,
    SurfaceAttachment, SurfacePrimitiveId, SurfaceProviderId, SurfaceRevision, SurfaceTagId,
    UnitInterval, WeightedSurfaceTag, WorldBounds, WorldPosition,
};

use crate::binary::BinaryReader;
use crate::memory::reserve_exact;
use crate::{
    CandidateIdentity, CandidateLineage, CandidateRejectionReason, DiagnosticCandidateSample,
    DiagnosticScalarSample, DiagnosticStreamScope, Error, GraphNodeAddress, GraphOperator,
    InteractionPolicy, MicroFieldTile, NamedDiagnosticStream, PlantFlags, PlantId, PlantLifecycle,
    PlantPointColumns, ProvenanceDecision, ProvenanceDecisionHandle, ProvenanceDecisionOutcome,
    ProvenanceHandle, ProvenanceRecord, ProvenanceTable, QuantizedOrientation,
    QuantizedSurfaceFieldQueryEntry, QuantizedSurfaceFieldQueryTile, QuantizedSurfaceFieldValue,
    QuantizedSurfaceProjectionEntry, QuantizedSurfaceProjectionSample,
    QuantizedSurfaceProjectionTile, RejectedCandidate, Result, VegetationCellSectionKind,
};

const MICRO_FORMAT: &str = "vegetation micro fields";
const PROVENANCE_FORMAT: &str = "vegetation provenance";
const REJECTION_FORMAT: &str = "vegetation rejection diagnostics";
const ATTACHMENT_FORMAT: &str = "vegetation surface attachments";
const DEPENDENCY_FORMAT: &str = "vegetation surface dependencies";
const RENDER_REFERENCE_FORMAT: &str = "vegetation render references";
const RENDER_BOUNDS_FORMAT: &str = "vegetation render bounds";
const COLLISION_FORMAT: &str = "vegetation collision inputs";
const NAVIGATION_FORMAT: &str = "vegetation navigation contributions";
const ECOLOGY_BOUNDARY_FORMAT: &str = "vegetation ecology boundary";
const ECOLOGY_CHECKPOINT_FORMAT: &str = "vegetation ecology checkpoint";

/// Complete typed result of decoding one independently resident cell facet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VegetationCellFacet {
    /// Canonical macro-point columns.
    MacroPoints(Box<PlantPointColumns>),
    /// Quantized cosmetic micro-field tiles.
    MicroFields(Vec<MicroFieldTile>),
    /// Shared accepted/rejected lineage table.
    Provenance(ProvenanceTable),
    /// Rejected candidates and named diagnostic streams.
    RejectionDiagnostics(VegetationRejectionDiagnosticsFacet),
    /// Exact surface projection query results.
    SurfaceAttachments(Vec<QuantizedSurfaceProjectionTile>),
    /// Exact surface-field query inputs.
    SurfaceDependencies(Vec<QuantizedSurfaceFieldQueryTile>),
    /// Renderer-independent representation selections.
    RenderReferences(Vec<VegetationRenderReference>),
    /// Conservative per-plant render bounds.
    RenderBounds(Vec<VegetationRenderBounds>),
    /// Inputs used to derive collision proxies.
    CollisionInputs(Vec<VegetationCollisionInput>),
    /// Inputs used to derive navigation contributions.
    NavigationContributions(Vec<VegetationNavigationContribution>),
    /// Cross-cell ecology boundary state.
    EcologyBoundary(Vec<VegetationEcologyBoundary>),
    /// Deterministic per-plant ecology checkpoint state.
    EcologyCheckpoint(Vec<VegetationEcologyCheckpoint>),
}

/// One typed render-reference row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationRenderReference {
    /// Stable plant identity.
    pub plant: PlantId,
    /// Compiled plant-family identity.
    pub family: Uuid,
    /// Family variation.
    pub variation: u32,
    /// Ecological phenotype.
    pub phenotype: u32,
    /// Renderer-independent representation class.
    pub representation_class: u32,
    /// Biological lifecycle state.
    pub lifecycle: PlantLifecycle,
}

/// One typed render-bounds row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationRenderBounds {
    /// Stable plant identity.
    pub plant: PlantId,
    /// Conservative world bounds.
    pub bounds: WorldBounds,
}

/// One typed collision-input row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationCollisionInput {
    /// Stable plant identity.
    pub plant: PlantId,
    /// Compiled plant-family identity.
    pub family: Uuid,
    /// Exact world position.
    pub position: WorldPosition,
    /// Quantized orientation.
    pub orientation: QuantizedOrientation,
    /// Q15.16 scale.
    pub scale: [DecisionScalar; 3],
    /// Conservative world bounds.
    pub bounds: WorldBounds,
    /// Gameplay interaction policy.
    pub interaction_policy: InteractionPolicy,
    /// Biological lifecycle state.
    pub lifecycle: PlantLifecycle,
}

/// One typed navigation-contribution row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationNavigationContribution {
    /// Stable plant identity.
    pub plant: PlantId,
    /// Compiled plant-family identity.
    pub family: Uuid,
    /// Conservative obstacle bounds.
    pub bounds: WorldBounds,
    /// Gameplay interaction policy.
    pub interaction_policy: InteractionPolicy,
    /// Biological lifecycle state.
    pub lifecycle: PlantLifecycle,
}

/// One cross-cell ecology boundary row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationEcologyBoundary {
    /// Stable plant identity.
    pub plant: PlantId,
    /// Compiled plant-family identity.
    pub family: Uuid,
    /// Conservative influence bounds.
    pub bounds: WorldBounds,
    /// Biological simulation tick.
    pub ecology_tick: u64,
    /// Persistent health.
    pub health: UnitInterval,
    /// Persistent moisture.
    pub moisture: UnitInterval,
    /// Persistent fuel.
    pub fuel: UnitInterval,
    /// Current phenology phase.
    pub phenology: UnitInterval,
}

/// One deterministic ecology checkpoint row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationEcologyCheckpoint {
    /// Stable plant identity.
    pub plant: PlantId,
    /// Compiled plant-family identity.
    pub family: Uuid,
    /// Biological lifecycle state.
    pub lifecycle: PlantLifecycle,
    /// Ecological phenotype.
    pub phenotype: u32,
    /// Biological simulation tick.
    pub ecology_tick: u64,
    /// Persistent health.
    pub health: UnitInterval,
    /// Persistent moisture.
    pub moisture: UnitInterval,
    /// Persistent fuel.
    pub fuel: UnitInterval,
    /// Current phenology phase.
    pub phenology: UnitInterval,
    /// Authored/runtime state flags.
    pub flags: PlantFlags,
}

/// Complete typed rejection-diagnostics payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationRejectionDiagnosticsFacet {
    /// Total candidates seen by output stages.
    pub candidate_count: u64,
    /// Accepted macro-point count.
    pub accepted_count: u64,
    /// Canonically ordered rejected candidates.
    pub rejected: Vec<RejectedCandidate>,
    /// Canonically ordered named diagnostic streams.
    pub streams: Vec<NamedDiagnosticStream>,
}

/// Decodes a section payload according to its closed `.svegcell` facet kind.
pub fn decode_vegetation_cell_facet(
    kind: VegetationCellSectionKind,
    bytes: &[u8],
) -> Result<VegetationCellFacet> {
    Ok(match kind {
        VegetationCellSectionKind::MacroPoints => VegetationCellFacet::MacroPoints(Box::new(
            PlantPointColumns::from_canonical_bytes(bytes)?,
        )),
        VegetationCellSectionKind::MicroFields => {
            VegetationCellFacet::MicroFields(decode_vegetation_micro_fields(bytes)?)
        }
        VegetationCellSectionKind::Provenance => {
            VegetationCellFacet::Provenance(decode_vegetation_provenance(bytes)?)
        }
        VegetationCellSectionKind::RejectionDiagnostics => {
            VegetationCellFacet::RejectionDiagnostics(decode_vegetation_rejection_diagnostics(
                bytes,
            )?)
        }
        VegetationCellSectionKind::SurfaceAttachments => {
            VegetationCellFacet::SurfaceAttachments(decode_vegetation_surface_attachments(bytes)?)
        }
        VegetationCellSectionKind::SurfaceDependencies => {
            VegetationCellFacet::SurfaceDependencies(decode_vegetation_surface_dependencies(bytes)?)
        }
        VegetationCellSectionKind::RenderReferences => {
            VegetationCellFacet::RenderReferences(decode_vegetation_render_references(bytes)?)
        }
        VegetationCellSectionKind::RenderBounds => {
            VegetationCellFacet::RenderBounds(decode_vegetation_render_bounds(bytes)?)
        }
        VegetationCellSectionKind::CollisionInputs => {
            VegetationCellFacet::CollisionInputs(decode_vegetation_collision_inputs(bytes)?)
        }
        VegetationCellSectionKind::NavigationContributions => {
            VegetationCellFacet::NavigationContributions(
                decode_vegetation_navigation_contributions(bytes)?,
            )
        }
        VegetationCellSectionKind::EcologyBoundary => {
            VegetationCellFacet::EcologyBoundary(decode_vegetation_ecology_boundary(bytes)?)
        }
        VegetationCellSectionKind::EcologyCheckpoint => {
            VegetationCellFacet::EcologyCheckpoint(decode_vegetation_ecology_checkpoint(bytes)?)
        }
    })
}

/// Decodes the exact current micro-field facet.
pub fn decode_vegetation_micro_fields(bytes: &[u8]) -> Result<Vec<MicroFieldTile>> {
    let mut reader = facet_reader(bytes, MICRO_FORMAT, b"SVEGMIC2")?;
    let count = reader.count(77)?;
    let mut tiles = reserved(count, "decoded vegetation micro fields")?;
    for _ in 0..count {
        let cell = reader.cell()?;
        let family = reader.uuid()?;
        let dimensions = [reader.u32()?, reader.u32()?, reader.u32()?];
        let density_count = reader.count(2)?;
        let mut density = reserved(density_count, "decoded vegetation micro density")?;
        for _ in 0..density_count {
            density.push(reader.u16()?);
        }
        let attribute_count = reader.count(20)?;
        let mut attributes = BTreeMap::new();
        let mut previous_channel = None;
        for _ in 0..attribute_count {
            let channel = reader.u128()?;
            if previous_channel.is_some_and(|previous| previous >= channel) {
                return invalid(MICRO_FORMAT, "attributes.order");
            }
            previous_channel = Some(channel);
            let value_count = reader.count(4)?;
            let mut values = reserved(value_count, "decoded vegetation micro attributes")?;
            for _ in 0..value_count {
                values.push(reader.i32()?);
            }
            if value_count != density_count {
                return invalid(MICRO_FORMAT, "attributes.count");
            }
            attributes.insert(channel, values);
        }
        let reconstruction_seed = reader.u128()?;
        let expected_density = dimensions
            .into_iter()
            .try_fold(1_usize, |product, dimension| {
                let dimension = usize::try_from(dimension).map_err(|_| Error::NumericOverflow)?;
                product.checked_mul(dimension).ok_or(Error::NumericOverflow)
            })?;
        if dimensions.contains(&0) || expected_density != density_count {
            return invalid(MICRO_FORMAT, "dimensions");
        }
        tiles.push(MicroFieldTile {
            cell,
            family,
            dimensions,
            density,
            attributes,
            reconstruction_seed,
        });
    }
    reader.complete()?;
    require_strict_order(&tiles, MICRO_FORMAT, "tiles.order", |left, right| {
        (left.cell, left.family.value()) < (right.cell, right.family.value())
    })?;
    Ok(tiles)
}

/// Decodes the exact current provenance facet.
pub fn decode_vegetation_provenance(bytes: &[u8]) -> Result<ProvenanceTable> {
    let mut reader = facet_reader(bytes, PROVENANCE_FORMAT, b"SVEGPRV1")?;
    let decision_count = reader.count(49)?;
    let mut table = ProvenanceTable::default();
    for index in 0..decision_count {
        let parent_count = reader.count(4)?;
        let mut parents = reserved(parent_count, "decoded vegetation provenance parents")?;
        for _ in 0..parent_count {
            let parent = ProvenanceDecisionHandle(reader.u32()?);
            if usize::try_from(parent.0).map_err(|_| Error::NumericOverflow)? >= index {
                return invalid(PROVENANCE_FORMAT, "decisions.parents");
            }
            parents.push(parent);
        }
        require_strict_order(
            &parents,
            PROVENANCE_FORMAT,
            "decisions.parents.order",
            |a, b| a < b,
        )?;
        let path_count = reader.count(16)?;
        let mut subgraph_path = reserved(path_count, "decoded vegetation provenance path")?;
        for _ in 0..path_count {
            subgraph_path.push(reader.u128()?);
        }
        let node = reader.u128()?;
        let operator_text = reader.string()?;
        let operator =
            GraphOperator::from_wire(&operator_text).ok_or_else(|| Error::ArtifactFormat {
                format: PROVENANCE_FORMAT,
                field: "decisions.operator".to_owned(),
            })?;
        let candidate = reader.u64()?;
        let outcome = provenance_outcome(reader.u8()?)?;
        let handle = table.intern_decision(ProvenanceDecision {
            parents,
            subgraph_path,
            node,
            operator,
            candidate,
            outcome,
        });
        if usize::try_from(handle.0).map_err(|_| Error::NumericOverflow)? != index {
            return invalid(PROVENANCE_FORMAT, "decisions.duplicate");
        }
    }
    let record_count = reader.count(50)?;
    for index in 0..record_count {
        let record = ProvenanceRecord {
            map: reader.uuid()?,
            layer: reader.u128()?,
            biome: reader.uuid()?,
            decision: ProvenanceDecisionHandle(reader.u32()?),
            candidate: reader.u64()?,
            family: read_optional_uuid(&mut reader, PROVENANCE_FORMAT)?,
            plant: read_optional_plant(&mut reader, PROVENANCE_FORMAT)?,
            variation: reader.u32()?,
        };
        if usize::try_from(record.decision.0).map_err(|_| Error::NumericOverflow)? >= decision_count
        {
            return invalid(PROVENANCE_FORMAT, "records.decision");
        }
        let handle = table.intern(record);
        if usize::try_from(handle.0).map_err(|_| Error::NumericOverflow)? != index {
            return invalid(PROVENANCE_FORMAT, "records.duplicate");
        }
    }
    reader.complete()?;
    Ok(table)
}

/// Decodes the exact current rejection-diagnostics facet.
pub fn decode_vegetation_rejection_diagnostics(
    bytes: &[u8],
) -> Result<VegetationRejectionDiagnosticsFacet> {
    let mut reader = facet_reader(bytes, REJECTION_FORMAT, b"SVEGREJ1")?;
    let candidate_count = reader.u64()?;
    let accepted_count = reader.u64()?;
    if accepted_count > candidate_count {
        return invalid(REJECTION_FORMAT, "acceptedCount");
    }
    let rejected_count = reader.count(57)?;
    let mut rejected = reserved(rejected_count, "decoded vegetation rejections")?;
    for _ in 0..rejected_count {
        rejected.push(read_rejected_candidate(&mut reader)?);
    }
    require_strict_order(
        &rejected,
        REJECTION_FORMAT,
        "rejected.order",
        rejected_before,
    )?;
    let stream_count = reader.count(27)?;
    let mut streams = reserved(stream_count, "decoded vegetation diagnostic streams")?;
    for _ in 0..stream_count {
        streams.push(read_diagnostic_stream(&mut reader)?);
    }
    reader.complete()?;
    require_strict_order(
        &streams,
        REJECTION_FORMAT,
        "streams.order",
        |left, right| diagnostic_key(left) < diagnostic_key(right),
    )?;
    Ok(VegetationRejectionDiagnosticsFacet {
        candidate_count,
        accepted_count,
        rejected,
        streams,
    })
}

/// Decodes the exact current surface-attachment facet.
pub fn decode_vegetation_surface_attachments(
    bytes: &[u8],
) -> Result<Vec<QuantizedSurfaceProjectionTile>> {
    let mut reader = facet_reader(bytes, ATTACHMENT_FORMAT, b"SVEGSAT1")?;
    let count = reader.count(60)?;
    let mut tiles = reserved(count, "decoded vegetation surface attachments")?;
    for _ in 0..count {
        let node = reader.u128()?;
        let node_semantic_revision = reader.u32()?;
        let provider_set_hash = reader.array()?;
        if node == 0 || node_semantic_revision == 0 || provider_set_hash == [0; 32] {
            return invalid(ATTACHMENT_FORMAT, "tile.identity");
        }
        let sample_count = reader.count(49)?;
        let mut samples = reserved(sample_count, "decoded vegetation projection samples")?;
        for _ in 0..sample_count {
            let query = read_world_position(&mut reader)?;
            let sample = match reader.u8()? {
                0 => None,
                1 => {
                    let position = read_world_position(&mut reader)?;
                    let attachment = read_required_attachment(&mut reader)?;
                    let normal = [
                        SignedUnit::from_bits(reader.u16()? as i16)?,
                        SignedUnit::from_bits(reader.u16()? as i16)?,
                        SignedUnit::from_bits(reader.u16()? as i16)?,
                    ];
                    let projection = [
                        DecisionScalar::from_bits(reader.i32()?),
                        DecisionScalar::from_bits(reader.i32()?),
                        DecisionScalar::from_bits(reader.i32()?),
                    ];
                    let tag_count = reader.count(10)?;
                    let mut tags = reserved(tag_count, "decoded vegetation surface tags")?;
                    for _ in 0..tag_count {
                        tags.push(WeightedSurfaceTag {
                            tag: SurfaceTagId(reader.u64()?),
                            weight: UnitInterval::from_bits(reader.u16()?),
                        });
                    }
                    require_strict_order(
                        &tags,
                        ATTACHMENT_FORMAT,
                        "samples.tags.order",
                        |a, b| a.tag < b.tag,
                    )?;
                    Some(QuantizedSurfaceProjectionSample {
                        position,
                        attachment,
                        normal,
                        projection,
                        tags,
                    })
                }
                _ => return invalid(ATTACHMENT_FORMAT, "samples.presence"),
            };
            samples.push(QuantizedSurfaceProjectionEntry { query, sample });
        }
        require_strict_order(&samples, ATTACHMENT_FORMAT, "samples.order", |a, b| {
            a.query < b.query
        })?;
        tiles.push(QuantizedSurfaceProjectionTile {
            node,
            node_semantic_revision,
            samples,
            provider_set_hash,
        });
    }
    reader.complete()?;
    require_strict_order(&tiles, ATTACHMENT_FORMAT, "tiles.order", |left, right| {
        projection_tile_key(left) < projection_tile_key(right)
    })?;
    Ok(tiles)
}

/// Decodes the exact current surface-dependency facet.
pub fn decode_vegetation_surface_dependencies(
    bytes: &[u8],
) -> Result<Vec<QuantizedSurfaceFieldQueryTile>> {
    let mut reader = facet_reader(bytes, DEPENDENCY_FORMAT, b"SVEGSDE1")?;
    let count = reader.count(62)?;
    let mut tiles = reserved(count, "decoded vegetation surface dependencies")?;
    for _ in 0..count {
        let node = reader.u128()?;
        let node_semantic_revision = reader.u32()?;
        let channel = read_field_channel(&mut reader)?;
        let derivative = read_field_derivative(&mut reader)?;
        let provider_set_hash = reader.array()?;
        if node == 0 || node_semantic_revision == 0 || provider_set_hash == [0; 32] {
            return invalid(DEPENDENCY_FORMAT, "tile.identity");
        }
        let sample_count = reader.count(105)?;
        let mut samples = reserved(sample_count, "decoded vegetation field samples")?;
        for _ in 0..sample_count {
            let candidate = read_candidate_identity(&mut reader, DEPENDENCY_FORMAT)?;
            let query = read_world_position(&mut reader)?;
            let value = read_surface_field_value(&mut reader)?;
            if !matches!(
                (derivative, value),
                (
                    FieldDerivative::Value,
                    QuantizedSurfaceFieldValue::Scalar(_)
                ) | (
                    FieldDerivative::Gradient,
                    QuantizedSurfaceFieldValue::Gradient(_)
                ) | (
                    FieldDerivative::Hessian,
                    QuantizedSurfaceFieldValue::Hessian(_)
                )
            ) {
                return invalid(DEPENDENCY_FORMAT, "samples.valueType");
            }
            samples.push(QuantizedSurfaceFieldQueryEntry {
                candidate,
                query,
                value,
            });
        }
        require_strict_order(&samples, DEPENDENCY_FORMAT, "samples.order", |a, b| {
            (a.candidate, a.query) < (b.candidate, b.query)
        })?;
        tiles.push(QuantizedSurfaceFieldQueryTile {
            node,
            node_semantic_revision,
            channel,
            derivative,
            samples,
            provider_set_hash,
        });
    }
    reader.complete()?;
    require_strict_order(&tiles, DEPENDENCY_FORMAT, "tiles.order", |left, right| {
        field_tile_key(left) < field_tile_key(right)
    })?;
    Ok(tiles)
}

/// Decodes the exact current render-reference facet.
pub fn decode_vegetation_render_references(bytes: &[u8]) -> Result<Vec<VegetationRenderReference>> {
    let mut reader = facet_reader(bytes, RENDER_REFERENCE_FORMAT, b"SVEGRRF1")?;
    let count = reader.count(40)?;
    let mut rows = reserved(count, "decoded vegetation render references")?;
    for _ in 0..count {
        rows.push(VegetationRenderReference {
            plant: read_plant(&mut reader)?,
            family: reader.uuid()?,
            variation: reader.u32()?,
            phenotype: reader.u32()?,
            representation_class: reader.u32()?,
            lifecycle: PlantLifecycle::try_from(reader.u32()?)?,
        });
    }
    reader.complete()?;
    require_plant_order(&rows, RENDER_REFERENCE_FORMAT, |row| row.plant)?;
    Ok(rows)
}

/// Decodes the exact current render-bounds facet.
pub fn decode_vegetation_render_bounds(bytes: &[u8]) -> Result<Vec<VegetationRenderBounds>> {
    let mut reader = facet_reader(bytes, RENDER_BOUNDS_FORMAT, b"SVEGRBD1")?;
    let count = reader.count(112)?;
    let mut rows = reserved(count, "decoded vegetation render bounds")?;
    for _ in 0..count {
        rows.push(VegetationRenderBounds {
            plant: read_plant(&mut reader)?,
            bounds: reader.bounds()?,
        });
    }
    reader.complete()?;
    require_plant_order(&rows, RENDER_BOUNDS_FORMAT, |row| row.plant)?;
    Ok(rows)
}

/// Decodes the exact current collision-input facet.
pub fn decode_vegetation_collision_inputs(bytes: &[u8]) -> Result<Vec<VegetationCollisionInput>> {
    let mut reader = facet_reader(bytes, COLLISION_FORMAT, b"SVEGCOL1")?;
    let count = reader.count(185)?;
    let mut rows = reserved(count, "decoded vegetation collision inputs")?;
    for _ in 0..count {
        let plant = read_plant(&mut reader)?;
        let family = reader.uuid()?;
        let position = read_packed_world_position(&mut reader)?;
        let orientation = read_orientation(&mut reader)?;
        let scale = read_scale(&mut reader)?;
        let bounds = reader.bounds()?;
        let interaction_policy = InteractionPolicy::try_from(reader.u32()?)?;
        let lifecycle = PlantLifecycle::try_from(reader.u32()?)?;
        if scale.iter().any(|scale| scale.bits() <= 0) || !bounds.contains(position) {
            return invalid(COLLISION_FORMAT, "rows.transformBounds");
        }
        rows.push(VegetationCollisionInput {
            plant,
            family,
            position,
            orientation,
            scale,
            bounds,
            interaction_policy,
            lifecycle,
        });
    }
    reader.complete()?;
    require_plant_order(&rows, COLLISION_FORMAT, |row| row.plant)?;
    Ok(rows)
}

/// Decodes the exact current navigation-contribution facet.
pub fn decode_vegetation_navigation_contributions(
    bytes: &[u8],
) -> Result<Vec<VegetationNavigationContribution>> {
    let mut reader = facet_reader(bytes, NAVIGATION_FORMAT, b"SVEGNAV1")?;
    let count = reader.count(128)?;
    let mut rows = reserved(count, "decoded vegetation navigation contributions")?;
    for _ in 0..count {
        rows.push(VegetationNavigationContribution {
            plant: read_plant(&mut reader)?,
            family: reader.uuid()?,
            bounds: reader.bounds()?,
            interaction_policy: InteractionPolicy::try_from(reader.u32()?)?,
            lifecycle: PlantLifecycle::try_from(reader.u32()?)?,
        });
    }
    reader.complete()?;
    require_plant_order(&rows, NAVIGATION_FORMAT, |row| row.plant)?;
    Ok(rows)
}

/// Decodes the exact current cross-cell ecology boundary facet.
pub fn decode_vegetation_ecology_boundary(bytes: &[u8]) -> Result<Vec<VegetationEcologyBoundary>> {
    let mut reader = facet_reader(bytes, ECOLOGY_BOUNDARY_FORMAT, b"SVEGEBD1")?;
    let count = reader.count(136)?;
    let mut rows = reserved(count, "decoded vegetation ecology boundary")?;
    for _ in 0..count {
        rows.push(VegetationEcologyBoundary {
            plant: read_plant(&mut reader)?,
            family: reader.uuid()?,
            bounds: reader.bounds()?,
            ecology_tick: reader.u64()?,
            health: UnitInterval::from_bits(reader.u16()?),
            moisture: UnitInterval::from_bits(reader.u16()?),
            fuel: UnitInterval::from_bits(reader.u16()?),
            phenology: UnitInterval::from_bits(reader.u16()?),
        });
    }
    reader.complete()?;
    require_plant_order(&rows, ECOLOGY_BOUNDARY_FORMAT, |row| row.plant)?;
    Ok(rows)
}

/// Decodes the exact current ecology-checkpoint facet.
pub fn decode_vegetation_ecology_checkpoint(
    bytes: &[u8],
) -> Result<Vec<VegetationEcologyCheckpoint>> {
    let mut reader = facet_reader(bytes, ECOLOGY_CHECKPOINT_FORMAT, b"SVEGECP1")?;
    let count = reader.count(52)?;
    let mut rows = reserved(count, "decoded vegetation ecology checkpoints")?;
    for _ in 0..count {
        rows.push(VegetationEcologyCheckpoint {
            plant: read_plant(&mut reader)?,
            family: reader.uuid()?,
            lifecycle: PlantLifecycle::try_from(reader.u32()?)?,
            phenotype: reader.u32()?,
            ecology_tick: reader.u64()?,
            health: UnitInterval::from_bits(reader.u16()?),
            moisture: UnitInterval::from_bits(reader.u16()?),
            fuel: UnitInterval::from_bits(reader.u16()?),
            phenology: UnitInterval::from_bits(reader.u16()?),
            flags: PlantFlags::from_bits(reader.u32()?)?,
        });
    }
    reader.complete()?;
    require_plant_order(&rows, ECOLOGY_CHECKPOINT_FORMAT, |row| row.plant)?;
    Ok(rows)
}

fn facet_reader<'a>(
    bytes: &'a [u8],
    format: &'static str,
    magic: &[u8; 8],
) -> Result<BinaryReader<'a>> {
    let mut reader = BinaryReader::new(bytes, format);
    reader.expect(magic, "magic")?;
    Ok(reader)
}

fn reserved<T>(count: usize, resource: &'static str) -> Result<Vec<T>> {
    let mut values = Vec::new();
    reserve_exact(&mut values, count, resource)?;
    Ok(values)
}

fn invalid<T>(format: &'static str, field: &str) -> Result<T> {
    Err(Error::ArtifactFormat {
        format,
        field: field.to_owned(),
    })
}

fn require_strict_order<T>(
    values: &[T],
    format: &'static str,
    field: &str,
    before: impl Fn(&T, &T) -> bool,
) -> Result<()> {
    if values.windows(2).any(|pair| !before(&pair[0], &pair[1])) {
        return invalid(format, field);
    }
    Ok(())
}

fn require_plant_order<T>(
    rows: &[T],
    format: &'static str,
    plant: impl Fn(&T) -> PlantId,
) -> Result<()> {
    require_strict_order(rows, format, "rows.plantOrder", |left, right| {
        plant(left) < plant(right)
    })
}

fn read_plant(reader: &mut BinaryReader<'_>) -> Result<PlantId> {
    PlantId::from_bytes(reader.array()?)
}

fn read_optional_plant(
    reader: &mut BinaryReader<'_>,
    format: &'static str,
) -> Result<Option<PlantId>> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(read_plant(reader)?)),
        _ => invalid(format, "optionalPlant"),
    }
}

fn read_optional_uuid(reader: &mut BinaryReader<'_>, format: &'static str) -> Result<Option<Uuid>> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.uuid()?)),
        _ => invalid(format, "optionalUuid"),
    }
}

fn read_world_position(reader: &mut BinaryReader<'_>) -> Result<WorldPosition> {
    Ok(WorldPosition::from_global_ticks([
        reader.i128()?,
        reader.i128()?,
        reader.i128()?,
    ])?)
}

fn read_packed_world_position(reader: &mut BinaryReader<'_>) -> Result<WorldPosition> {
    WorldPosition::new(
        reader.cell()?,
        QuantizedLocalPosition::new([reader.u32()?, reader.u32()?, reader.u32()?])?,
    )
    .map_err(Into::into)
}

fn read_orientation(reader: &mut BinaryReader<'_>) -> Result<QuantizedOrientation> {
    QuantizedOrientation::new([
        reader.u16()? as i16,
        reader.u16()? as i16,
        reader.u16()? as i16,
        reader.u16()? as i16,
    ])
}

fn read_scale(reader: &mut BinaryReader<'_>) -> Result<[DecisionScalar; 3]> {
    Ok([
        DecisionScalar::from_bits(reader.i32()?),
        DecisionScalar::from_bits(reader.i32()?),
        DecisionScalar::from_bits(reader.i32()?),
    ])
}

fn read_required_attachment(reader: &mut BinaryReader<'_>) -> Result<SurfaceAttachment> {
    Ok(SurfaceAttachment::new(
        SurfaceProviderId(reader.u64()?),
        SurfacePrimitiveId(reader.u64()?),
        [
            UnitInterval::from_bits(reader.u16()?),
            UnitInterval::from_bits(reader.u16()?),
            UnitInterval::from_bits(reader.u16()?),
        ],
        SurfaceRevision(reader.u64()?),
    )?)
}

fn read_field_channel(reader: &mut BinaryReader<'_>) -> Result<FieldChannel> {
    Ok(match reader.u8()? {
        0 => FieldChannel::Altitude,
        1 => FieldChannel::Slope,
        2 => FieldChannel::Curvature,
        3 => FieldChannel::Concavity,
        4 => FieldChannel::Drainage,
        5 => FieldChannel::Moisture,
        6 => FieldChannel::Temperature,
        7 => FieldChannel::Precipitation,
        8 => FieldChannel::Sunlight,
        9 => FieldChannel::Exposure,
        10 => FieldChannel::WaterDistance,
        11 => FieldChannel::WaterDepth,
        12 => FieldChannel::SignedBlocker,
        13 => FieldChannel::SplineDistance,
        14 => FieldChannel::User(reader.u64()?),
        _ => return invalid(DEPENDENCY_FORMAT, "tile.channel"),
    })
}

fn read_field_derivative(reader: &mut BinaryReader<'_>) -> Result<FieldDerivative> {
    match reader.u8()? {
        0 => Ok(FieldDerivative::Value),
        1 => Ok(FieldDerivative::Gradient),
        2 => Ok(FieldDerivative::Hessian),
        _ => invalid(DEPENDENCY_FORMAT, "tile.derivative"),
    }
}

fn read_surface_field_value(reader: &mut BinaryReader<'_>) -> Result<QuantizedSurfaceFieldValue> {
    match reader.u8()? {
        0 => Ok(QuantizedSurfaceFieldValue::Scalar(reader.i32()?)),
        1 => Ok(QuantizedSurfaceFieldValue::Gradient([
            reader.i32()?,
            reader.i32()?,
            reader.i32()?,
        ])),
        2 => Ok(QuantizedSurfaceFieldValue::Hessian([
            reader.i32()?,
            reader.i32()?,
            reader.i32()?,
            reader.i32()?,
            reader.i32()?,
            reader.i32()?,
        ])),
        _ => invalid(DEPENDENCY_FORMAT, "samples.valueType"),
    }
}

fn read_candidate_identity(
    reader: &mut BinaryReader<'_>,
    format: &'static str,
) -> Result<CandidateIdentity> {
    let identity = CandidateIdentity {
        node: reader.u128()?,
        node_address: reader.u128()?,
        node_semantic_revision: reader.u32()?,
        ordinal: reader.u64()?,
        ancestor: reader.u64()?,
    };
    if identity.node == 0 || identity.node_address == 0 || identity.node_semantic_revision == 0 {
        return invalid(format, "candidate.identity");
    }
    Ok(identity)
}

fn provenance_outcome(value: u8) -> Result<ProvenanceDecisionOutcome> {
    match value {
        0 => Ok(ProvenanceDecisionOutcome::Produced),
        1 => Ok(ProvenanceDecisionOutcome::Retained),
        2 => Ok(ProvenanceDecisionOutcome::Accepted),
        3 => Ok(ProvenanceDecisionOutcome::Rejected),
        _ => invalid(PROVENANCE_FORMAT, "decisions.outcome"),
    }
}

fn rejection_reason(value: u8) -> Result<CandidateRejectionReason> {
    match value {
        0 => Ok(CandidateRejectionReason::SurfaceMiss),
        1 => Ok(CandidateRejectionReason::Threshold),
        2 => Ok(CandidateRejectionReason::WeightedElimination),
        3 => Ok(CandidateRejectionReason::PriorityExclusion),
        4 => Ok(CandidateRejectionReason::Competition),
        5 => Ok(CandidateRejectionReason::ForeignOwner),
        6 => Ok(CandidateRejectionReason::NoSpecies),
        _ => invalid(REJECTION_FORMAT, "rejectionReason"),
    }
}

const fn rejection_code(value: CandidateRejectionReason) -> u8 {
    match value {
        CandidateRejectionReason::SurfaceMiss => 0,
        CandidateRejectionReason::Threshold => 1,
        CandidateRejectionReason::WeightedElimination => 2,
        CandidateRejectionReason::PriorityExclusion => 3,
        CandidateRejectionReason::Competition => 4,
        CandidateRejectionReason::ForeignOwner => 5,
        CandidateRejectionReason::NoSpecies => 6,
    }
}

fn read_rejected_candidate(reader: &mut BinaryReader<'_>) -> Result<RejectedCandidate> {
    Ok(RejectedCandidate {
        candidate: read_candidate_identity(reader, REJECTION_FORMAT)?,
        reason: rejection_reason(reader.u8()?)?,
        provenance: ProvenanceHandle(reader.u32()?),
    })
}

fn rejected_before(left: &RejectedCandidate, right: &RejectedCandidate) -> bool {
    (left.candidate, rejection_code(left.reason), left.provenance)
        < (
            right.candidate,
            rejection_code(right.reason),
            right.provenance,
        )
}

fn read_diagnostic_stream(reader: &mut BinaryReader<'_>) -> Result<NamedDiagnosticStream> {
    let node_length = reader.length()?;
    let mut node_reader = BinaryReader::new(reader.take(node_length)?, REJECTION_FORMAT);
    let module_count = node_reader.count(16)?;
    let mut module_path = reserved(module_count, "decoded diagnostic module path")?;
    for _ in 0..module_count {
        module_path.push(node_reader.u128()?);
    }
    let node_id = node_reader.u128()?;
    node_reader.complete()?;
    if node_id == 0 {
        return invalid(REJECTION_FORMAT, "streams.node");
    }
    let node = GraphNodeAddress {
        module_path,
        node: node_id,
    };
    let label = reader.string()?;
    let scope = match reader.u8()? {
        0 => DiagnosticStreamScope::GlobalSnapshot,
        1 => DiagnosticStreamScope::CandidateLineage(CandidateLineage(reader.u128()?)),
        _ => return invalid(REJECTION_FORMAT, "streams.scope"),
    };
    let candidates = match reader.u8()? {
        0 => None,
        1 => {
            let count = reader.count(142)?;
            let mut values = reserved(count, "decoded diagnostic candidate samples")?;
            for _ in 0..count {
                values.push(DiagnosticCandidateSample {
                    identity: read_candidate_identity(reader, REJECTION_FORMAT)?,
                    owner: reader.cell()?,
                    position: read_world_position(reader)?,
                    family: read_optional_uuid(reader, REJECTION_FORMAT)?,
                    variation: reader.u32()?,
                    priority: DecisionScalar::from_bits(reader.i32()?),
                    ecology_tick: reader.u64()?,
                });
            }
            require_strict_order(
                &values,
                REJECTION_FORMAT,
                "streams.candidates.order",
                |a, b| a.identity < b.identity,
            )?;
            Some(values)
        }
        _ => return invalid(REJECTION_FORMAT, "streams.candidates.presence"),
    };
    let field = match reader.u8()? {
        0 => None,
        1 => {
            let count = reader.count(56)?;
            let mut values = reserved(count, "decoded diagnostic scalar samples")?;
            for _ in 0..count {
                values.push(DiagnosticScalarSample {
                    candidate: read_candidate_identity(reader, REJECTION_FORMAT)?,
                    value: DecisionScalar::from_bits(reader.i32()?),
                });
            }
            require_strict_order(&values, REJECTION_FORMAT, "streams.field.order", |a, b| {
                a.candidate < b.candidate
            })?;
            Some(values)
        }
        _ => return invalid(REJECTION_FORMAT, "streams.field.presence"),
    };
    let rejected_count = reader.count(57)?;
    let mut rejected = reserved(rejected_count, "decoded diagnostic stream rejections")?;
    for _ in 0..rejected_count {
        rejected.push(read_rejected_candidate(reader)?);
    }
    require_strict_order(
        &rejected,
        REJECTION_FORMAT,
        "streams.rejected.order",
        rejected_before,
    )?;
    Ok(NamedDiagnosticStream {
        node,
        label,
        scope,
        candidates,
        field,
        rejected,
    })
}

fn diagnostic_key(
    stream: &NamedDiagnosticStream,
) -> (&GraphNodeAddress, &str, DiagnosticStreamScope) {
    (&stream.node, stream.label.as_str(), stream.scope)
}

fn projection_tile_key(tile: &QuantizedSurfaceProjectionTile) -> (u128, u32, [u8; 32]) {
    (
        tile.node,
        tile.node_semantic_revision,
        tile.provider_set_hash,
    )
}

fn field_tile_key(
    tile: &QuantizedSurfaceFieldQueryTile,
) -> (u128, u32, FieldChannel, FieldDerivative, [u8; 32]) {
    (
        tile.node,
        tile.node_semantic_revision,
        tile.channel,
        tile.derivative,
        tile.provider_set_hash,
    )
}
