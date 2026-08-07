//! Execution of one published work plan: claimant workers race the on-disk claims a remote
//! fleet would, and each cooks the cells it wins.

use std::collections::BTreeMap;

use saffron_spatial::{DecisionScalar, WorldCellKey};
use saffron_vegetation::{
    ContentHash, CookNodeAddress, CookNodeRecord, CookWorkActual, GraphCancellationToken,
    GraphEvaluationResult, ManifestCellDependency, ManifestCellDependencyRole,
    VegetationCellArtifactHeader, VegetationCellArtifactIndex, VegetationManifestCell,
    write_vegetation_cell_artifact,
};

use crate::{Error, Result, VegetationArtifactStore};

use super::dependencies::{cell_ancestor_dependencies, push_dependency};
use super::measure::{manifest_sections, result_actual, species_counts};
use super::{VegetationCookEvent, cancellation_checkpoint};

/// The lease after which an unfinished work claim is presumed dead and swept at plan start.
/// Sweeping is safe because publication is idempotent by content address.
pub(super) const WORK_CLAIM_LEASE: std::time::Duration = std::time::Duration::from_secs(30);

/// Executes one published work plan with an in-process claimant pool. Every worker races the
/// same on-disk claims a remote claimant would, cooks the cells it wins through
/// [`cook_one_cell`], and records completions; item order and the claim protocol — not the
/// pool — carry the correctness.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_work_items(
    store: &VegetationArtifactStore,
    work_identity: ContentHash,
    work_manifest: &saffron_vegetation::CookWorkManifest,
    merged_results: Vec<(WorldCellKey, GraphEvaluationResult)>,
    platform_profile: ContentHash,
    workers: usize,
    cancellation: &GraphCancellationToken,
    emit: &mut impl FnMut(VegetationCookEvent),
) -> Result<()> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    let results: Vec<Mutex<Option<GraphEvaluationResult>>> = merged_results
        .into_iter()
        .map(|(_, result)| Mutex::new(Some(result)))
        .collect();
    let done: Vec<AtomicBool> = (0..work_manifest.items.len())
        .map(|_| AtomicBool::new(false))
        .collect();
    let cell_outputs = Mutex::new(BTreeMap::<WorldCellKey, ContentHash>::new());
    let failure = Mutex::new(None::<Error>);
    let (events, event_sink) = std::sync::mpsc::channel::<VegetationCookEvent>();

    // Two workers can discover the same pre-existing record concurrently; the done flag's
    // compare-exchange picks one winner, and only the winner emits the node's events
    // (`announce` adds the Started half for a discovery, whose claimant never emitted one
    // in this run).
    let record_completion = |index: usize,
                             completion: &saffron_vegetation::CookWorkCompletion,
                             events: &std::sync::mpsc::Sender<VegetationCookEvent>,
                             announce: bool| {
        let CookNodeAddress::Cell { cell, .. } = completion.node.address else {
            return;
        };
        cell_outputs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(cell, completion.node.output_hash);
        if done[index]
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        if announce {
            let _ = events.send(VegetationCookEvent::Started {
                node: completion.node.address.clone(),
            });
        }
        let _ = events.send(VegetationCookEvent::Completed {
            node: completion.node.address.clone(),
            cache_hit: completion.actual.cache_hit,
            published_cell: true,
        });
    };
    let fail = |error: Error| {
        let mut slot = failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        slot.get_or_insert(error);
    };

    std::thread::scope(|scope| {
        for worker in 0..workers.max(1) {
            let claimant = format!("{}/{worker}", std::process::id());
            let events = events.clone();
            let record_completion = &record_completion;
            let fail = &fail;
            let results = &results;
            let done = &done;
            let cell_outputs = &cell_outputs;
            let failure = &failure;
            scope.spawn(move || {
                loop {
                    if failure
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .is_some()
                    {
                        return;
                    }
                    if let Err(error) = cancellation_checkpoint(cancellation) {
                        fail(error);
                        return;
                    }
                    let mut all_done = true;
                    let mut progressed = false;
                    for (index, item) in work_manifest.items.iter().enumerate() {
                        if done[index].load(Ordering::Acquire) {
                            continue;
                        }
                        all_done = false;
                        // A completion from another claimant — or an interrupted earlier run
                        // of this same plan — counts without cooking anything.
                        let recorded = match store
                            .read_work_completion_if_present(work_identity, index as u32)
                        {
                            Ok(recorded) => recorded,
                            Err(error) => {
                                fail(error);
                                return;
                            }
                        };
                        if let Some(completion) = recorded {
                            record_completion(index, &completion, &events, true);
                            progressed = true;
                            continue;
                        }
                        if item
                            .blocked_by
                            .iter()
                            .any(|&blocker| !done[blocker as usize].load(Ordering::Acquire))
                        {
                            continue;
                        }
                        match store.claim_work_item(work_identity, index as u32, &claimant) {
                            Ok(true) => {}
                            Ok(false) => continue,
                            Err(error) => {
                                fail(error);
                                return;
                            }
                        }
                        let Some(result) = results[index]
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .take()
                        else {
                            continue;
                        };
                        let _ = events.send(VegetationCookEvent::Started {
                            node: item.address.clone(),
                        });
                        let ancestors = cell_outputs
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .clone();
                        match cook_one_cell(
                            store,
                            work_identity,
                            work_manifest,
                            index as u32,
                            &result,
                            platform_profile,
                            &ancestors,
                        ) {
                            Ok(completion) => {
                                record_completion(index, &completion, &events, false);
                                progressed = true;
                            }
                            Err(error) => {
                                fail(error);
                                return;
                            }
                        }
                    }
                    if all_done {
                        return;
                    }
                    if !progressed {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            });
        }
        drop(events);
        for event in event_sink {
            emit(event);
        }
    });

    if let Some(error) = failure
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        return Err(error);
    }
    cancellation_checkpoint(cancellation)?;
    Ok(())
}

/// Cooks one claimed work item: reads and verifies its payload, composes the full dependency
/// set from the payload's own half plus the completed ancestors, publishes the cell artifact,
/// and records the completion the committer assembles.
fn cook_one_cell(
    store: &VegetationArtifactStore,
    work_identity: ContentHash,
    work_manifest: &saffron_vegetation::CookWorkManifest,
    item_index: u32,
    result: &GraphEvaluationResult,
    platform_profile: ContentHash,
    cell_outputs: &BTreeMap<WorldCellKey, ContentHash>,
) -> Result<saffron_vegetation::CookWorkCompletion> {
    let item = work_manifest
        .items
        .get(item_index as usize)
        .ok_or_else(|| Error::Io("vegetation work item index out of range".to_owned()))?;
    let payload_bytes = store.read_work_payload(item.payload)?;
    let payload = saffron_vegetation::CookWorkPayload::from_canonical_bytes(&payload_bytes)?;
    let recomputed = saffron_vegetation::cook_work_own_input_key(
        work_manifest.versions,
        &work_manifest.platform,
        &payload.address,
        &payload.own_dependencies,
    )?;
    if recomputed != item.own_input_key || payload.address != item.address {
        return Err(Error::Io(
            "vegetation work payload does not match its item's own-input key".to_owned(),
        ));
    }
    let CookNodeAddress::Cell { map, cell } = payload.address.clone() else {
        return Err(Error::Io(
            "vegetation work payload addresses a non-cell node".to_owned(),
        ));
    };
    let mut dependencies = payload.own_dependencies;
    for dependency in cell_ancestor_dependencies(map, cell, result, cell_outputs)? {
        push_dependency(&mut dependencies, dependency)?;
    }
    let estimate = item.estimate;
    let mut node = CookNodeRecord {
        address: payload.address,
        cook_key: ContentHash::default(),
        output_hash: ContentHash::default(),
        dependencies,
        estimate,
        actual: CookWorkActual::default(),
    };
    node.cook_key = node.calculate_cook_key(work_manifest.versions, &work_manifest.platform)?;
    let sections = result.cell_artifact_sections()?;
    let bytes = write_vegetation_cell_artifact(
        VegetationCellArtifactHeader {
            cell,
            cook_key: node.cook_key,
            platform_profile,
        },
        &sections,
    )?;
    let publication = store.publish_cell(&bytes)?;
    let index = VegetationCellArtifactIndex::open(
        &bytes,
        saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
    )?;
    node.output_hash = publication.content_hash;
    node.actual = result_actual(result, publication.bytes, publication.cache_hit);
    let species_counts = species_counts(result)?;
    let macro_count = u64::try_from(result.macro_points.ids.len())
        .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
    let micro_count = species_counts.iter().try_fold(0_u64, |total, species| {
        total
            .checked_add(species.micro_count)
            .ok_or(Error::Vegetation(
                saffron_vegetation::Error::NumericOverflow,
            ))
    })?;
    let cell_dependencies = result
        .ancestor_references
        .iter()
        .map(|ancestor| {
            let content_hash = cell_outputs.get(ancestor).copied().ok_or_else(|| {
                Error::Io(format!(
                    "vegetation cell {cell} references uncooked ancestor {ancestor}"
                ))
            })?;
            Ok(ManifestCellDependency {
                cell: *ancestor,
                content_hash,
                role: ManifestCellDependencyRole::Ancestor,
                halo: DecisionScalar::from_bits(0),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let manifest_cell = VegetationManifestCell {
        cell,
        bounds: cell.bounds(),
        artifact_hash: publication.content_hash,
        payload_hash: index.payload_hash,
        dependencies: cell_dependencies,
        species_counts,
        macro_count,
        micro_count,
        resident_memory_bytes: estimate.peak_memory_bytes,
        stored_bytes: publication.bytes,
        estimate,
        actual: node.actual,
        sections: manifest_sections(&index),
    };
    let completion = saffron_vegetation::CookWorkCompletion {
        item: item_index,
        actual: node.actual,
        node,
        manifest_cell,
    };
    store.complete_work_item(work_identity, item_index, &completion.canonical_bytes()?)?;
    Ok(completion)
}
