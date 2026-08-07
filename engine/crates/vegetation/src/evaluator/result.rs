//! The canonical evaluation result and the identity-edit preview.

use super::*;

use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::WorldCellKey;

use crate::canonical::{ByteSink, CanonicalSink, CountSink};
use crate::hash::VegetationContentHasher;
use crate::{
    Error, IdentityConflictReport, PlantId, PlantPointColumns, ProvenanceDecision,
    ProvenanceDecisionHandle, ProvenanceHandle, ProvenanceRecord, ProvenanceTable, Result,
    VegetationCellSection, VegetationCellSectionKind, identity_conflicts,
};

/// Complete result of one reference evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphEvaluationResult {
    pub cell: WorldCellKey,
    /// Schema-hashed canonical macro columns.
    pub macro_points: PlantPointColumns,
    pub micro_fields: Vec<MicroFieldTile>,
    /// Exact canonical surface projection queries produced during this evaluation.
    pub surface_projection_tiles: Vec<QuantizedSurfaceProjectionTile>,
    /// Exact canonical surface-field queries produced during preparation or replay.
    pub surface_field_query_tiles: Vec<QuantizedSurfaceFieldQueryTile>,
    /// Coarse cells whose macro products are referenced by this finer cell.
    pub ancestor_references: Vec<WorldCellKey>,
    /// Expanded provenance table used by accepted points.
    pub provenance: ProvenanceTable,
    /// Typed counts, timings, and rejection explanations.
    pub diagnostics: GraphEvaluationDiagnostics,
}

/// One complete, parent-before-child explanation of an accepted or rejected candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenanceExplanation {
    /// Record handle referenced by the point or rejection.
    pub provenance: ProvenanceHandle,
    /// Map, layer, biome, candidate, family, and optional accepted plant identity.
    pub record: ProvenanceRecord,
    /// Reachable decision DAG nodes in canonical handle order.
    pub decisions: Vec<(ProvenanceDecisionHandle, ProvenanceDecision)>,
    /// Terminal rejection reason, absent for an accepted plant.
    pub rejection_reason: Option<CandidateRejectionReason>,
}

impl GraphEvaluationResult {
    /// Explains one accepted plant through the shared provenance decision DAG.
    pub fn explain_plant(&self, plant: PlantId) -> Result<ProvenanceExplanation> {
        let index =
            self.macro_points
                .ids
                .binary_search(&plant)
                .map_err(|_| Error::GraphDocument {
                    path: format!("evaluation.plants.{plant}"),
                    reason: "accepted plant is not present in this result".to_owned(),
                })?;
        let handle = self
            .macro_points
            .provenance
            .get(index)
            .copied()
            .map(ProvenanceHandle)
            .ok_or_else(|| {
                Error::PointSchema("plant provenance column is incomplete".to_owned())
            })?;
        self.explain_provenance(handle, None)
    }

    /// Explains one rejected candidate through the shared provenance decision DAG.
    pub fn explain_rejection(&self, candidate: CandidateIdentity) -> Result<ProvenanceExplanation> {
        let rejected = self
            .diagnostics
            .rejected
            .iter()
            .find(|rejected| rejected.candidate == candidate)
            .ok_or_else(|| Error::GraphDocument {
                path: "evaluation.rejected".to_owned(),
                reason: "candidate is not present in the rejection diagnostics".to_owned(),
            })?;
        self.explain_provenance(rejected.provenance, Some(rejected.reason))
    }

    fn explain_provenance(
        &self,
        handle: ProvenanceHandle,
        rejection_reason: Option<CandidateRejectionReason>,
    ) -> Result<ProvenanceExplanation> {
        let record = self
            .provenance
            .get(handle)
            .cloned()
            .ok_or_else(|| Error::GraphDocument {
                path: format!("evaluation.provenance.{}", handle.0),
                reason: "provenance record is missing".to_owned(),
            })?;
        let mut reachable = BTreeSet::new();
        let mut pending = vec![record.decision];
        while let Some(decision) = pending.pop() {
            if !reachable.insert(decision) {
                continue;
            }
            let value = self
                .provenance
                .decision(decision)
                .ok_or_else(|| Error::GraphDocument {
                    path: format!("evaluation.provenance.decisions.{}", decision.0),
                    reason: "provenance decision is missing".to_owned(),
                })?;
            pending.extend(value.parents.iter().copied());
        }
        let decisions = reachable
            .into_iter()
            .map(|decision| {
                self.provenance
                    .decision(decision)
                    .cloned()
                    .map(|value| (decision, value))
                    .ok_or_else(|| Error::GraphDocument {
                        path: format!("evaluation.provenance.decisions.{}", decision.0),
                        reason: "provenance decision is missing".to_owned(),
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ProvenanceExplanation {
            provenance: handle,
            record,
            decisions,
            rejection_reason,
        })
    }

    /// Exact canonical encoding length without allocating the encoded result.
    pub fn canonical_byte_len(&self) -> Result<usize> {
        let mut sink = CountSink::new();
        self.encode_canonical(&mut sink)?;
        Ok(sink.finish())
    }

    /// Stable result bytes used by determinism and scheduling tests.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let mut sink = ByteSink::new();
        self.encode_canonical(&mut sink)?;
        Ok(sink.finish())
    }

    /// Streams the exact canonical encoding into a vegetation content digest.
    pub fn update_content_hasher(&self, hasher: &mut VegetationContentHasher) -> Result<()> {
        self.encode_canonical(hasher)
    }

    /// Produces independently resident `.svegcell` facets from the canonical evaluator result.
    pub fn cell_artifact_sections(&self) -> Result<Vec<VegetationCellSection>> {
        self.validate_canonical_encoding()?;
        let macro_points = self.macro_points.canonical_bytes()?;

        let mut micro_fields = ByteSink::new();
        micro_fields.write(b"SVEGMIC2")?;
        self.encode_micro_fields(&mut micro_fields)?;

        let mut provenance = ByteSink::new();
        provenance.write(b"SVEGPRV1")?;
        self.encode_provenance(&mut provenance)?;

        let mut diagnostics = ByteSink::new();
        diagnostics.write(b"SVEGREJ2")?;
        self.encode_rejection_diagnostics(&mut diagnostics)?;

        let mut attachments = ByteSink::new();
        attachments.write(b"SVEGSAT1")?;
        self.encode_surface_attachments(&mut attachments)?;

        let mut surface_dependencies = ByteSink::new();
        surface_dependencies.write(b"SVEGSDE1")?;
        self.encode_surface_dependencies(&mut surface_dependencies)?;

        let mut render_references = ByteSink::new();
        render_references.write(b"SVEGRRF1")?;
        self.encode_render_references(&mut render_references)?;

        let mut render_bounds = ByteSink::new();
        render_bounds.write(b"SVEGRBD1")?;
        self.encode_render_bounds(&mut render_bounds)?;

        let mut collision_inputs = ByteSink::new();
        collision_inputs.write(b"SVEGCOL1")?;
        self.encode_collision_inputs(&mut collision_inputs)?;

        let mut navigation = ByteSink::new();
        navigation.write(b"SVEGNAV1")?;
        self.encode_navigation_contributions(&mut navigation)?;

        let mut ecology_boundary = ByteSink::new();
        ecology_boundary.write(b"SVEGEBD1")?;
        self.encode_ecology_boundary(&mut ecology_boundary)?;

        let mut ecology_checkpoint = ByteSink::new();
        ecology_checkpoint.write(b"SVEGECP1")?;
        self.encode_ecology_checkpoint(&mut ecology_checkpoint)?;

        Ok(vec![
            VegetationCellSection::new(VegetationCellSectionKind::MacroPoints, macro_points),
            VegetationCellSection::new(
                VegetationCellSectionKind::MicroFields,
                micro_fields.finish(),
            ),
            VegetationCellSection::new(VegetationCellSectionKind::Provenance, provenance.finish()),
            VegetationCellSection::new(
                VegetationCellSectionKind::RejectionDiagnostics,
                diagnostics.finish(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::SurfaceAttachments,
                attachments.finish(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::SurfaceDependencies,
                surface_dependencies.finish(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::RenderReferences,
                render_references.finish(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::RenderBounds,
                render_bounds.finish(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::CollisionInputs,
                collision_inputs.finish(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::NavigationContributions,
                navigation.finish(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::EcologyBoundary,
                ecology_boundary.finish(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::EcologyCheckpoint,
                ecology_checkpoint.finish(),
            ),
        ])
    }

    fn encode_canonical<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        self.validate_canonical_encoding()?;
        sink.write(b"SVEGEVAL05")?;
        sink.write(&self.cell.canonical_bytes())?;
        let macro_length = self.macro_points.canonical_byte_len()?;
        push_len(sink, macro_length)?;
        self.macro_points.encode_canonical(sink)?;
        self.encode_micro_fields(sink)?;
        self.encode_surface_attachments(sink)?;
        self.encode_surface_dependencies(sink)?;
        encode_unique_references(sink, &self.ancestor_references)?;
        self.encode_provenance(sink)?;
        self.encode_rejection_diagnostics(sink)
    }

    fn encode_micro_fields<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        encode_micro_fields_body(sink, &self.micro_fields)
    }

    fn encode_surface_attachments<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.surface_projection_tiles.len())?;
        encode_ordered(sink, &self.surface_projection_tiles, encode_projection_tile)
    }

    fn encode_surface_dependencies<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.surface_field_query_tiles.len())?;
        encode_ordered(
            sink,
            &self.surface_field_query_tiles,
            encode_field_query_tile,
        )
    }

    fn encode_render_references<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.macro_points.ids.len())?;
        for row in 0..self.macro_points.ids.len() {
            sink.write(&self.macro_points.ids[row].bytes())?;
            sink.write(&self.macro_points.families[row].value().to_be_bytes())?;
            sink.write(&self.macro_points.variations[row].to_be_bytes())?;
            sink.write(&self.macro_points.phenotypes[row].to_be_bytes())?;
            sink.write(&self.macro_points.representation_classes[row].to_be_bytes())?;
            sink.write(&(self.macro_points.lifecycles[row] as u32).to_be_bytes())?;
        }
        Ok(())
    }

    fn encode_render_bounds<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.macro_points.ids.len())?;
        for row in 0..self.macro_points.ids.len() {
            sink.write(&self.macro_points.ids[row].bytes())?;
            encode_world_bounds(sink, self.macro_points.bounds[row])?;
        }
        Ok(())
    }

    fn encode_collision_inputs<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.macro_points.ids.len())?;
        for row in 0..self.macro_points.ids.len() {
            sink.write(&self.macro_points.ids[row].bytes())?;
            sink.write(&self.macro_points.families[row].value().to_be_bytes())?;
            encode_world_position(sink, self.macro_points.positions[row])?;
            for lane in self.macro_points.orientations[row].bits() {
                sink.write(&lane.to_be_bytes())?;
            }
            for scale in self.macro_points.scales[row] {
                sink.write(&scale.canonical_bytes())?;
            }
            encode_world_bounds(sink, self.macro_points.bounds[row])?;
            sink.write(&(self.macro_points.interaction_policies[row] as u32).to_be_bytes())?;
            sink.write(&(self.macro_points.lifecycles[row] as u32).to_be_bytes())?;
        }
        Ok(())
    }

    fn encode_navigation_contributions<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.macro_points.ids.len())?;
        for row in 0..self.macro_points.ids.len() {
            sink.write(&self.macro_points.ids[row].bytes())?;
            sink.write(&self.macro_points.families[row].value().to_be_bytes())?;
            encode_world_bounds(sink, self.macro_points.bounds[row])?;
            sink.write(&(self.macro_points.interaction_policies[row] as u32).to_be_bytes())?;
            sink.write(&(self.macro_points.lifecycles[row] as u32).to_be_bytes())?;
        }
        Ok(())
    }

    fn encode_ecology_boundary<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        let cell_bounds = self.cell.bounds();
        let boundary_rows = self
            .macro_points
            .bounds
            .iter()
            .enumerate()
            .filter(|(_, bounds)| touches_boundary(**bounds, cell_bounds))
            .map(|(row, _)| row)
            .collect::<Vec<_>>();
        push_len(sink, boundary_rows.len())?;
        for row in boundary_rows {
            sink.write(&self.macro_points.ids[row].bytes())?;
            sink.write(&self.macro_points.families[row].value().to_be_bytes())?;
            encode_world_bounds(sink, self.macro_points.bounds[row])?;
            sink.write(&self.macro_points.ecology_ticks[row].to_be_bytes())?;
            sink.write(&self.macro_points.health[row].canonical_bytes())?;
            sink.write(&self.macro_points.moisture[row].canonical_bytes())?;
            sink.write(&self.macro_points.fuel[row].canonical_bytes())?;
            sink.write(&self.macro_points.phenology[row].canonical_bytes())?;
        }
        Ok(())
    }

    fn encode_ecology_checkpoint<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.macro_points.ids.len())?;
        for row in 0..self.macro_points.ids.len() {
            sink.write(&self.macro_points.ids[row].bytes())?;
            sink.write(&self.macro_points.families[row].value().to_be_bytes())?;
            sink.write(&(self.macro_points.lifecycles[row] as u32).to_be_bytes())?;
            sink.write(&self.macro_points.phenotypes[row].to_be_bytes())?;
            sink.write(&self.macro_points.ecology_ticks[row].to_be_bytes())?;
            sink.write(&self.macro_points.health[row].canonical_bytes())?;
            sink.write(&self.macro_points.moisture[row].canonical_bytes())?;
            sink.write(&self.macro_points.fuel[row].canonical_bytes())?;
            sink.write(&self.macro_points.phenology[row].canonical_bytes())?;
            sink.write(&self.macro_points.flags[row].bits().to_be_bytes())?;
        }
        Ok(())
    }

    pub(super) fn encode_provenance<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        encode_provenance(sink, &self.provenance)
    }

    fn encode_rejection_diagnostics<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        sink.write(&self.diagnostics.candidate_count.to_be_bytes())?;
        sink.write(&self.diagnostics.accepted_count.to_be_bytes())?;
        push_len(sink, self.diagnostics.rejected.len())?;
        encode_ordered(sink, &self.diagnostics.rejected, encode_rejected_candidate)?;
        push_len(sink, self.diagnostics.streams.len())?;
        encode_ordered(sink, &self.diagnostics.streams, encode_diagnostic_stream)
    }

    fn validate_canonical_encoding(&self) -> Result<()> {
        self.validate_canonical_encoding_with_guard(None)
    }

    pub(super) fn validate_canonical_encoding_guarded(
        &self,
        guard: PreflightGuard<'_>,
    ) -> Result<()> {
        self.validate_canonical_encoding_with_guard(Some(guard))
    }

    fn validate_canonical_encoding_with_guard(
        &self,
        guard: Option<PreflightGuard<'_>>,
    ) -> Result<()> {
        self.macro_points
            .validate_guarded(|| guard.map_or(Ok(()), PreflightGuard::check))?;
        for tile in &self.surface_projection_tiles {
            guard.map_or(Ok(()), PreflightGuard::check)?;
            if let Some(guard) = guard {
                validate_projection_tile_guarded(tile, guard)?;
            } else {
                tile.validate()?;
            }
        }
        for tile in &self.surface_field_query_tiles {
            validate_field_query_tile_with_guard(tile, guard)?;
        }
        let ordered =
            validate_strict_order(&self.micro_fields, guard, |left, right| {
                (left.cell, left.family.value()) < (right.cell, right.family.value())
            })? && validate_strict_order(&self.surface_projection_tiles, guard, |left, right| {
                projection_tile_order_key(left) < projection_tile_order_key(right)
            })? && validate_strict_order(&self.surface_field_query_tiles, guard, |left, right| {
                field_query_tile_order_key(left) < field_query_tile_order_key(right)
            })? && validate_strict_order(&self.ancestor_references, guard, |left, right| {
                left < right
            })? && validate_strict_order(&self.diagnostics.rejected, guard, |left, right| {
                rejected_order_key(left) < rejected_order_key(right)
            })? && validate_strict_order(&self.diagnostics.streams, guard, |left, right| {
                diagnostic_stream_order_key(left) < diagnostic_stream_order_key(right)
            })?;
        if !ordered {
            return Err(Error::GraphDocument {
                path: "evaluation.canonicalEncoding".to_owned(),
                reason: "result collections are not in canonical order".to_owned(),
            });
        }
        for stream in &self.diagnostics.streams {
            guard.map_or(Ok(()), PreflightGuard::check)?;
            let candidates_ordered = match &stream.candidates {
                Some(values) => validate_strict_order(values, guard, |left, right| {
                    left.identity < right.identity
                })?,
                None => true,
            };
            let field_ordered = match &stream.field {
                Some(values) => validate_strict_order(values, guard, |left, right| {
                    left.candidate < right.candidate
                })?,
                None => true,
            };
            let rejected_ordered =
                validate_strict_order(&stream.rejected, guard, |left, right| {
                    rejected_order_key(left) < rejected_order_key(right)
                })?;
            let stream_ordered = candidates_ordered && field_ordered && rejected_ordered;
            if !stream_ordered {
                return Err(Error::GraphDocument {
                    path: "evaluation.canonicalEncoding.streams".to_owned(),
                    reason: "diagnostic stream samples are not in canonical order".to_owned(),
                });
            }
        }
        guard.map_or(Ok(()), PreflightGuard::check)
    }
}

/// Accepted/removed/moved identities and override conflicts for a destructive graph edit preview.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphIdentityEditPreview {
    /// Newly accepted identities.
    pub accepted: Vec<PlantId>,
    /// Identities removed by the edit.
    pub removed: Vec<PlantId>,
    /// Retained identities whose exact positions moved.
    pub moved: Vec<PlantId>,
    /// Pins and authored overrides invalidated by removed identities.
    pub conflicts: IdentityConflictReport,
}

/// Compares two complete evaluator results before committing an identity-affecting edit.
pub fn preview_graph_identity_edit(
    previous: &GraphEvaluationResult,
    proposed: &GraphEvaluationResult,
    pins: &[PlantId],
    overrides: &[PlantId],
) -> Result<GraphIdentityEditPreview> {
    previous.macro_points.validate()?;
    proposed.macro_points.validate()?;
    let old: BTreeMap<_, _> = previous
        .macro_points
        .ids
        .iter()
        .copied()
        .zip(previous.macro_points.positions.iter().copied())
        .collect();
    let new: BTreeMap<_, _> = proposed
        .macro_points
        .ids
        .iter()
        .copied()
        .zip(proposed.macro_points.positions.iter().copied())
        .collect();
    let old_ids = old.keys().copied().collect::<BTreeSet<_>>();
    let new_ids = new.keys().copied().collect::<BTreeSet<_>>();
    let accepted = new_ids.difference(&old_ids).copied().collect();
    let removed = old_ids.difference(&new_ids).copied().collect();
    let moved = old_ids
        .intersection(&new_ids)
        .copied()
        .filter(|id| old[id] != new[id])
        .collect();
    Ok(GraphIdentityEditPreview {
        accepted,
        removed,
        moved,
        conflicts: identity_conflicts(
            &old_ids.into_iter().collect::<Vec<_>>(),
            &new_ids.into_iter().collect::<Vec<_>>(),
            pins,
            overrides,
        ),
    })
}
