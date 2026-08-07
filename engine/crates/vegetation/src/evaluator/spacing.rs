//! Weighted elimination, variable spacing, and competition claims.

use super::*;

use std::collections::BTreeMap;

use saffron_spatial::{DecisionScalar, UnitInterval};

use crate::{CompiledGraphNode, Error, Result};

pub(super) fn weighted_elimination(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    weights: &ScalarFieldSamples,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    ensure_lineage(node, "weights", candidates.lineage, weights.lineage)?;
    let target = usize::try_from(u32_parameter(node, "targetCount", 0)?).unwrap_or(usize::MAX);
    let maximum_neighbours = usize::try_from(u32_parameter(node, "maximumNeighbours", 0)?)
        .map_err(|_| Error::NumericOverflow)?;
    if maximum_neighbours == 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "weighted-elimination maximum neighbours must be positive".to_owned(),
        });
    }
    if target >= candidates.candidates.len() {
        return Ok(candidates.clone());
    }
    let radius = fixed_meters_to_ticks(fixed_parameter(
        node,
        "eliminationRadius",
        DecisionScalar::from_bits(0),
    )?)?
    .checked_abs()
    .ok_or(Error::NumericOverflow)?;
    if radius == 0 {
        return Err(Error::GraphUnboundedInfluence {
            node: node.definition.guid,
        });
    }
    ensure_support_ticks(node, radius)?;
    let candidate_count = candidates.candidates.len();
    state.check_transient_memory(weighted_elimination_scratch_bytes(
        candidate_count as u64,
        target as u64,
        maximum_neighbours as u64,
    )?)?;
    let mut candidate_weights = Vec::new();
    crate::memory::reserve_exact(
        &mut candidate_weights,
        candidate_count,
        "weighted elimination weights",
    )?;
    for candidate in &candidates.candidates {
        let weight = weights
            .values
            .get(&candidate.identity)
            .copied()
            .ok_or_else(|| Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: "weighted-elimination sample".to_owned(),
            })?;
        let weight = u32::try_from(weight.bits()).map_err(|_| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "weighted-elimination weights must be positive".to_owned(),
        })?;
        if weight == 0 {
            return Err(Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "weighted-elimination weights must be positive".to_owned(),
            });
        }
        candidate_weights.push(weight);
    }
    let radius_squared = radius.checked_mul(radius).ok_or(Error::NumericOverflow)?;
    let mut bucket_entries = Vec::new();
    crate::memory::reserve_exact(
        &mut bucket_entries,
        candidate_count,
        "weighted elimination spatial index",
    )?;
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        let ticks = candidate.position.global_ticks();
        bucket_entries.push((
            (ticks[0].div_euclid(radius), ticks[2].div_euclid(radius)),
            index,
        ));
    }
    bucket_entries.sort_unstable();
    let mut adjacency = Vec::new();
    crate::memory::reserve_exact(
        &mut adjacency,
        candidate_count,
        "weighted elimination adjacency headers",
    )?;
    for _ in 0..candidate_count {
        let mut neighbours = Vec::new();
        crate::memory::reserve_exact(
            &mut neighbours,
            maximum_neighbours,
            "weighted elimination adjacency",
        )?;
        adjacency.push(neighbours);
    }
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        let ticks = candidate.position.global_ticks();
        let bucket = (ticks[0].div_euclid(radius), ticks[2].div_euclid(radius));
        for x in -1..=1 {
            for z in -1..=1 {
                let neighbour_bucket = (bucket.0 + x, bucket.1 + z);
                let start = bucket_entries.partition_point(|(key, _)| *key < neighbour_bucket);
                let end = bucket_entries.partition_point(|(key, _)| *key <= neighbour_bucket);
                for &(_, other_index) in bucket_entries[start..end]
                    .iter()
                    .filter(|(_, other)| *other > index)
                {
                    let other = &candidates.candidates[other_index];
                    let distance_squared = distance_squared_xz(candidate.position, other.position)?;
                    if distance_squared >= radius_squared {
                        continue;
                    }
                    let distance = integer_sqrt(distance_squared);
                    let contribution =
                        u64::try_from(radius.checked_sub(distance).ok_or(Error::NumericOverflow)?)
                            .map_err(|_| Error::NumericOverflow)?;
                    if adjacency[index].len() == maximum_neighbours
                        || adjacency[other_index].len() == maximum_neighbours
                    {
                        return Err(Error::GraphLimit {
                            resource: "weighted-elimination neighbours",
                            requested: u64::try_from(maximum_neighbours)
                                .unwrap_or(u64::MAX)
                                .saturating_add(1),
                            limit: u64::try_from(maximum_neighbours).unwrap_or(u64::MAX),
                        });
                    }
                    adjacency[index].push((other_index, contribution));
                    adjacency[other_index].push((index, contribution));
                }
            }
        }
    }
    let baseline = u64::try_from(radius).map_err(|_| Error::NumericOverflow)?;
    let mut crowding = Vec::new();
    crate::memory::reserve_exact(
        &mut crowding,
        candidate_count,
        "weighted elimination crowding",
    )?;
    for neighbours in &adjacency {
        crowding.push(
            neighbours
                .iter()
                .try_fold(baseline, |sum, (_, contribution)| {
                    sum.checked_add(*contribution).ok_or(Error::NumericOverflow)
                })?,
        );
    }
    let mut active = Vec::new();
    crate::memory::reserve_exact(
        &mut active,
        candidate_count,
        "weighted elimination active mask",
    )?;
    active.resize(candidate_count, true);
    let mut queue = IndexedEliminationQueue::with_capacity(candidate_count)?;
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        queue.push(EliminationScore {
            crowding: crowding[index],
            weight: candidate_weights[index],
            identity: candidate.identity,
            index,
        })?;
    }
    let mut active_count = candidates.candidates.len();
    while active_count > target {
        state.check_abort()?;
        let score = queue.pop_max().ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "weighted-elimination queue exhausted".to_owned(),
        })?;
        active[score.index] = false;
        active_count -= 1;
        for &(neighbour, contribution) in &adjacency[score.index] {
            if !active[neighbour] {
                continue;
            }
            let next_crowding = crowding[neighbour]
                .checked_sub(contribution)
                .ok_or(Error::NumericOverflow)?;
            crowding[neighbour] = next_crowding;
            queue.update(EliminationScore {
                crowding: next_crowding,
                weight: candidate_weights[neighbour],
                identity: candidates.candidates[neighbour].identity,
                index: neighbour,
            })?;
        }
    }
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        if !active[index] {
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::WeightedElimination,
                candidate.family,
                candidate.variation,
                state,
            )?;
        }
    }
    let mut retained = Vec::new();
    crate::memory::reserve_exact(&mut retained, target, "weighted elimination result")?;
    retained.extend(
        candidates
            .candidates
            .iter()
            .enumerate()
            .filter(|(index, _)| active[*index])
            .map(|(_, candidate)| candidate.clone()),
    );
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: retained,
    };
    result.canonicalize()?;
    Ok(result)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EliminationScore {
    crowding: u64,
    pub(super) weight: u32,
    pub(super) identity: CandidateIdentity,
    pub(super) index: usize,
}

impl Ord for EliminationScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (u128::from(self.crowding) * u128::from(other.weight))
            .cmp(&(u128::from(other.crowding) * u128::from(self.weight)))
            .then_with(|| self.identity.cmp(&other.identity))
            .then_with(|| self.index.cmp(&other.index))
    }
}

struct IndexedEliminationQueue {
    heap: Vec<EliminationScore>,
    pub(super) positions: Vec<usize>,
}

impl IndexedEliminationQueue {
    pub(super) fn with_capacity(capacity: usize) -> Result<Self> {
        let mut heap = Vec::new();
        crate::memory::reserve_exact(&mut heap, capacity, "weighted elimination queue")?;
        let mut positions = Vec::new();
        crate::memory::reserve_exact(&mut positions, capacity, "weighted elimination queue index")?;
        positions.resize(capacity, usize::MAX);
        Ok(Self { heap, positions })
    }

    pub(super) fn push(&mut self, score: EliminationScore) -> Result<()> {
        if score.index >= self.positions.len() || self.positions[score.index] != usize::MAX {
            return Err(elimination_queue_error());
        }
        let position = self.heap.len();
        self.heap.push(score);
        self.positions[score.index] = position;
        self.sift_up(position);
        Ok(())
    }

    fn pop_max(&mut self) -> Option<EliminationScore> {
        let last = self.heap.len().checked_sub(1)?;
        self.swap_nodes(0, last);
        let removed = self.heap.pop()?;
        self.positions[removed.index] = usize::MAX;
        if !self.heap.is_empty() {
            self.sift_down(0);
        }
        Some(removed)
    }

    pub(super) fn update(&mut self, score: EliminationScore) -> Result<()> {
        let position = *self
            .positions
            .get(score.index)
            .ok_or_else(elimination_queue_error)?;
        let previous = *self
            .heap
            .get(position)
            .ok_or_else(elimination_queue_error)?;
        self.heap[position] = score;
        if score > previous {
            self.sift_up(position);
        } else {
            self.sift_down(position);
        }
        Ok(())
    }

    fn sift_up(&mut self, mut position: usize) {
        while position > 0 {
            let parent = (position - 1) / 2;
            if self.heap[parent] >= self.heap[position] {
                break;
            }
            self.swap_nodes(parent, position);
            position = parent;
        }
    }

    fn sift_down(&mut self, mut position: usize) {
        loop {
            let left = position * 2 + 1;
            if left >= self.heap.len() {
                break;
            }
            let right = left + 1;
            let child = if right < self.heap.len() && self.heap[right] > self.heap[left] {
                right
            } else {
                left
            };
            if self.heap[position] >= self.heap[child] {
                break;
            }
            self.swap_nodes(position, child);
            position = child;
        }
    }

    fn swap_nodes(&mut self, left: usize, right: usize) {
        self.heap.swap(left, right);
        self.positions[self.heap[left].index] = left;
        self.positions[self.heap[right].index] = right;
    }
}

fn elimination_queue_error() -> Error {
    Error::GraphDocument {
        path: "weightedElimination.queue".to_owned(),
        reason: "indexed elimination queue is inconsistent".to_owned(),
    }
}

impl PartialOrd for EliminationScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

pub(super) fn variable_spacing(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    radii: &ScalarFieldSamples,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    ensure_lineage(node, "radius", candidates.lineage, radii.lineage)?;
    state.check_transient_memory(xz_filter_scratch_bytes(candidates.candidates.len() as u64)?)?;
    let prototype_aware = bool_parameter(node, "prototypeAware", false)?;
    let mut effective_radii = Vec::new();
    crate::memory::reserve_exact(
        &mut effective_radii,
        candidates.candidates.len(),
        "variable-spacing radii",
    )?;
    for candidate in &candidates.candidates {
        let authored = radii
            .values
            .get(&candidate.identity)
            .copied()
            .unwrap_or(candidate.crown_radius.max(candidate.root_radius));
        let radius = if prototype_aware {
            let family = candidate
                .family
                .ok_or_else(|| Error::GraphAuthoritativeInput {
                    node: node.definition.guid,
                    input: "plant family before prototype-aware spacing".to_owned(),
                })?;
            let prototype = prototype_for_family(state, family)?;
            authored.max(
                prototype
                    .crown_radius
                    .into_iter()
                    .chain(prototype.root_radius)
                    .max()
                    .unwrap(),
            )
        } else {
            authored
        };
        effective_radii.push((
            candidate.identity,
            nonnegative_radius_ticks(node, "variable-spacing radius sample", radius)?,
        ));
    }
    effective_radii.sort_unstable_by_key(|(identity, _)| *identity);
    let maximum_radius = effective_radii
        .iter()
        .map(|(_, radius)| *radius)
        .max()
        .unwrap_or(0);
    let maximum_support = if prototype_aware {
        maximum_radius
            .checked_mul(2)
            .ok_or(Error::NumericOverflow)?
    } else {
        maximum_radius
    };
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    let mut immutable = Vec::new();
    crate::memory::reserve_exact(
        &mut immutable,
        candidates.candidates.len(),
        "variable-spacing candidates",
    )?;
    immutable.extend(candidates.candidates.iter().cloned());
    immutable.sort_unstable_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.identity.cmp(&right.identity))
    });
    let mut accepted: Vec<GraphCandidate> = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "variable-spacing accepted candidates",
    )?;
    let mut bucket_heads = BTreeMap::<(i128, i128), usize>::new();
    let mut bucket_links = Vec::<Option<usize>>::new();
    crate::memory::reserve_exact(
        &mut bucket_links,
        candidates.candidates.len(),
        "variable-spacing bucket links",
    )?;
    for candidate in immutable {
        let radius_ticks = lookup_candidate_radius(
            &effective_radii,
            candidate.identity,
            node,
            "variable-spacing radius sample",
        )?;
        ensure_support_ticks(node, radius_ticks)?;
        let mut wins = true;
        let bucket = xz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for z in -1..=1 {
                let key = offset_xz_bucket(bucket, x, z)?;
                let mut index = bucket_heads.get(&key).copied();
                while let Some(current) = index {
                    let other = accepted.get(current).ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "variable-spacing spatial index is invalid".to_owned(),
                    })?;
                    let other_radius_ticks = lookup_candidate_radius(
                        &effective_radii,
                        other.identity,
                        node,
                        "variable-spacing radius sample",
                    )?;
                    let required = if prototype_aware {
                        radius_ticks
                            .checked_add(other_radius_ticks)
                            .ok_or(Error::NumericOverflow)?
                    } else {
                        radius_ticks.max(other_radius_ticks)
                    };
                    ensure_support_ticks(node, required)?;
                    let required_squared = required
                        .checked_mul(required)
                        .ok_or(Error::NumericOverflow)?;
                    if distance_squared_xz(candidate.position, other.position)? < required_squared {
                        wins = false;
                        break 'neighbours;
                    }
                    index = *bucket_links
                        .get(current)
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "variable-spacing spatial index is invalid".to_owned(),
                        })?;
                }
            }
        }
        if wins {
            let index = accepted.len();
            bucket_links.push(bucket_heads.insert(bucket, index));
            accepted.push(candidate);
        } else {
            reject_candidate(
                node,
                &candidate,
                candidates.lineage,
                CandidateRejectionReason::Competition,
                candidate.family,
                candidate.variation,
                state,
            )?;
        }
    }
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted,
    };
    result.canonicalize()?;
    Ok(result)
}

pub(super) fn lookup_candidate_radius(
    radii: &[(CandidateIdentity, i128)],
    identity: CandidateIdentity,
    node: &CompiledGraphNode,
    input: &'static str,
) -> Result<i128> {
    radii
        .binary_search_by_key(&identity, |(candidate, _)| *candidate)
        .ok()
        .map(|index| radii[index].1)
        .ok_or_else(|| Error::GraphAuthoritativeInput {
            node: node.definition.guid,
            input: input.to_owned(),
        })
}

pub(super) fn competition_claims(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    communities: &CommunityTables,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    state.check_transient_memory(competition_scratch_bytes(
        candidates.candidates.len() as u64
    )?)?;
    let crown_weight = unit_parameter(node, "crownWeight", UnitInterval::ZERO)?;
    let root_weight = unit_parameter(node, "rootWeight", UnitInterval::ZERO)?;
    if crown_weight == UnitInterval::ZERO && root_weight == UnitInterval::ZERO {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "competition crown and root weights cannot both be zero".to_owned(),
        });
    }
    let mut radii = Vec::new();
    crate::memory::reserve_exact(&mut radii, candidates.candidates.len(), "competition radii")?;
    for candidate in &candidates.candidates {
        let family = candidate
            .family
            .ok_or_else(|| Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: "plant family before competition".to_owned(),
            })?;
        let prototype = prototype_for_family(state, family)?;
        let crown = prototype.crown_radius[0].max(prototype.crown_radius[1]);
        let root = prototype.root_radius[0].max(prototype.root_radius[1]);
        let crown = crown.checked_mul(DecisionScalar::from_bits(i32::from(crown_weight.bits())))?;
        let root = root.checked_mul(DecisionScalar::from_bits(i32::from(root_weight.bits())))?;
        radii.push((
            candidate.identity,
            fixed_meters_to_ticks(crown.checked_add(root)?)?.unsigned_abs() as i128,
        ));
    }
    radii.sort_unstable_by_key(|(identity, _)| *identity);
    let maximum_radius = radii.iter().map(|(_, radius)| *radius).max().unwrap_or(0);
    let maximum_pair_spacing =
        communities
            .competition
            .iter()
            .try_fold(0_i128, |maximum, rule| {
                Ok::<_, Error>(
                    maximum.max(fixed_meters_to_ticks(rule.spacing)?.unsigned_abs() as i128),
                )
            })?;
    let maximum_support = maximum_radius
        .checked_mul(2)
        .ok_or(Error::NumericOverflow)?
        .max(maximum_pair_spacing);
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    let mut buckets = Vec::<((i128, i128), usize)>::new();
    crate::memory::reserve_exact(
        &mut buckets,
        candidates.candidates.len(),
        "competition spatial index",
    )?;
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        buckets.push((xz_bucket(candidate.position, bucket_size), index));
    }
    buckets.sort_unstable();
    let mut accepted = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "competition accepted candidates",
    )?;
    for candidate in &candidates.candidates {
        let candidate_radius =
            lookup_candidate_radius(&radii, candidate.identity, node, "competition radius")?;
        let mut loses = false;
        let bucket = xz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for z in -1..=1 {
                let key = offset_xz_bucket(bucket, x, z)?;
                let start = buckets.partition_point(|(bucket, _)| *bucket < key);
                let end = buckets.partition_point(|(bucket, _)| *bucket <= key);
                for (_, index) in &buckets[start..end] {
                    let other =
                        candidates
                            .candidates
                            .get(*index)
                            .ok_or_else(|| Error::GraphDocument {
                                path: node.debug_symbol.label.clone(),
                                reason: "competition spatial index is invalid".to_owned(),
                            })?;
                    if candidate.identity == other.identity {
                        continue;
                    }
                    let other_radius = lookup_candidate_radius(
                        &radii,
                        other.identity,
                        node,
                        "competition radius",
                    )?;
                    let mut required = candidate_radius
                        .checked_add(other_radius)
                        .ok_or(Error::NumericOverflow)?;
                    let pair_rule = match (candidate.family, other.family) {
                        (Some(candidate_family), Some(other_family)) => communities
                            .competition
                            .iter()
                            .filter_map(|rule| {
                                if rule.first == candidate_family && rule.second == other_family {
                                    Some((rule.spacing, rule.priority))
                                } else if rule.first == other_family
                                    && rule.second == candidate_family
                                {
                                    Some((rule.spacing, rule.priority.checked_neg()?))
                                } else {
                                    None
                                }
                            })
                            .max_by_key(|(spacing, priority)| (*spacing, *priority)),
                        _ => None,
                    };
                    if let Some((spacing, _)) = pair_rule {
                        required =
                            required.max(fixed_meters_to_ticks(spacing)?.unsigned_abs() as i128);
                    }
                    ensure_support_ticks(node, required)?;
                    if distance_squared_xz(candidate.position, other.position)?
                        >= required
                            .checked_mul(required)
                            .ok_or(Error::NumericOverflow)?
                    {
                        continue;
                    }
                    let oriented_priority = pair_rule.map_or(0, |(_, priority)| priority);
                    let other_wins = oriented_priority < 0
                        || (oriented_priority == 0
                            && (other.priority > candidate.priority
                                || (other.priority == candidate.priority
                                    && other.identity < candidate.identity)));
                    if other_wins {
                        loses = true;
                        break 'neighbours;
                    }
                }
            }
        }
        if loses {
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::Competition,
                candidate.family,
                candidate.variation,
                state,
            )?;
        } else {
            accepted.push(candidate.clone());
        }
    }
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted,
    };
    result.canonicalize()?;
    Ok(result)
}
