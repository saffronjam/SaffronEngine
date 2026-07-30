//! Measured work, estimates, and the observational statistics one cook reports.

use std::collections::BTreeMap;
use std::time::Instant;

use saffron_core::Uuid;
use saffron_spatial::WorldCellKey;
use saffron_vegetation::{
    CandidateRejectionReason, CookGraph, CookNodeAddress, CookWorkActual, CookWorkEstimate,
    GraphEvaluationDiagnostics, GraphEvaluationResult, ManifestCellSection, ManifestSpeciesCount,
    PlantPointColumns, ProvenanceTable, VegetationCellArtifactIndex,
};

use crate::{Error, Result};

use super::VegetationCookStatistics;

pub(super) fn cook_statistics(
    graph: &CookGraph,
    elapsed_micros: u64,
    rejection_totals: [u64; 7],
) -> VegetationCookStatistics {
    let reasons = [
        CandidateRejectionReason::SurfaceMiss,
        CandidateRejectionReason::Threshold,
        CandidateRejectionReason::WeightedElimination,
        CandidateRejectionReason::PriorityExclusion,
        CandidateRejectionReason::Competition,
        CandidateRejectionReason::ForeignOwner,
        CandidateRejectionReason::NoSpecies,
    ];
    VegetationCookStatistics {
        nodes: u64::try_from(graph.nodes.len()).unwrap_or(u64::MAX),
        elapsed_micros,
        peak_memory_bytes: graph
            .nodes
            .iter()
            .map(|node| node.actual.peak_memory_bytes)
            .max()
            .unwrap_or(0),
        input_bytes: graph.nodes.iter().fold(0_u64, |total, node| {
            total.saturating_add(node.actual.input_bytes)
        }),
        output_bytes: graph.nodes.iter().fold(0_u64, |total, node| {
            total.saturating_add(node.actual.output_bytes)
        }),
        cache_hits: u64::try_from(
            graph
                .nodes
                .iter()
                .filter(|node| node.actual.cache_hit)
                .count(),
        )
        .unwrap_or(u64::MAX),
        cache_misses: u64::try_from(
            graph
                .nodes
                .iter()
                .filter(|node| !node.actual.cache_hit)
                .count(),
        )
        .unwrap_or(u64::MAX),
        published_cells: u64::try_from(
            graph
                .nodes
                .iter()
                .filter(|node| matches!(&node.address, CookNodeAddress::Cell { .. }))
                .count(),
        )
        .unwrap_or(u64::MAX),
        rejections: reasons
            .into_iter()
            .enumerate()
            .map(|(index, reason)| (reason, rejection_totals[index]))
            .collect(),
    }
}

pub(super) const fn rejection_reason_index(reason: CandidateRejectionReason) -> usize {
    match reason {
        CandidateRejectionReason::SurfaceMiss => 0,
        CandidateRejectionReason::Threshold => 1,
        CandidateRejectionReason::WeightedElimination => 2,
        CandidateRejectionReason::PriorityExclusion => 3,
        CandidateRejectionReason::Competition => 4,
        CandidateRejectionReason::ForeignOwner => 5,
        CandidateRejectionReason::NoSpecies => 6,
    }
}

pub(super) fn empty_result(cell: WorldCellKey) -> GraphEvaluationResult {
    GraphEvaluationResult {
        cell,
        macro_points: PlantPointColumns::default(),
        micro_fields: Vec::new(),
        surface_projection_tiles: Vec::new(),
        surface_field_query_tiles: Vec::new(),
        ancestor_references: Vec::new(),
        provenance: ProvenanceTable::default(),
        diagnostics: GraphEvaluationDiagnostics::default(),
    }
}

pub(super) fn species_counts(result: &GraphEvaluationResult) -> Result<Vec<ManifestSpeciesCount>> {
    let mut counts = BTreeMap::<u64, (Uuid, u64, u64)>::new();
    for family in &result.macro_points.families {
        let count = counts.entry(family.value()).or_insert((*family, 0, 0));
        count.1 = count.1.checked_add(1).ok_or(Error::Vegetation(
            saffron_vegetation::Error::NumericOverflow,
        ))?;
    }
    for tile in &result.micro_fields {
        let samples = u64::try_from(tile.density.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
        let count = counts
            .entry(tile.family.value())
            .or_insert((tile.family, 0, 0));
        count.2 = count.2.checked_add(samples).ok_or(Error::Vegetation(
            saffron_vegetation::Error::NumericOverflow,
        ))?;
    }
    Ok(counts
        .into_iter()
        .map(
            |(_, (family, macro_count, micro_count))| ManifestSpeciesCount {
                family,
                macro_count,
                micro_count,
            },
        )
        .collect())
}

pub(super) fn manifest_sections(index: &VegetationCellArtifactIndex) -> Vec<ManifestCellSection> {
    index
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
        .collect()
}

pub(super) fn result_estimate(result: &GraphEvaluationResult) -> Result<CookWorkEstimate> {
    let output_bytes = u64::try_from(result.canonical_byte_len()?)
        .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
    let micro_samples = result.micro_fields.iter().try_fold(0_u64, |total, tile| {
        total
            .checked_add(
                u64::try_from(tile.density.len())
                    .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
            )
            .ok_or(Error::Vegetation(
                saffron_vegetation::Error::NumericOverflow,
            ))
    })?;
    Ok(CookWorkEstimate {
        work_units: result
            .diagnostics
            .candidate_count
            .saturating_add(result.diagnostics.accepted_count)
            .saturating_add(micro_samples),
        peak_memory_bytes: output_bytes,
        input_bytes: 0,
        output_bytes,
    })
}

pub(super) fn result_actual(
    result: &GraphEvaluationResult,
    output_bytes: u64,
    cache_hit: bool,
) -> CookWorkActual {
    CookWorkActual {
        elapsed_micros: result.diagnostics.nodes.iter().fold(0_u64, |total, node| {
            total.saturating_add(node.elapsed_micros)
        }),
        peak_memory_bytes: result
            .canonical_byte_len()
            .ok()
            .and_then(|bytes| u64::try_from(bytes).ok())
            .unwrap_or(u64::MAX),
        input_bytes: 0,
        output_bytes,
        rejection_count: u64::try_from(result.diagnostics.rejected.len()).unwrap_or(u64::MAX),
        cache_hit,
    }
}

pub(super) fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}
