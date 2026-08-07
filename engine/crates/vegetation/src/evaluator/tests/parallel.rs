use super::*;

#[test]
fn reference_is_stable_under_input_order_origin_and_repetition() {
    let graph = compile_fixture(0);
    let halo = graph.required_halo(0);
    let mut original = input(WorldCellKey::base(0, 0, 0), halo);
    let baseline_result =
        evaluate_cell_reference(&graph, &original, &GraphCancellationToken::default()).unwrap();
    let plant = baseline_result.macro_points.ids[0];
    let explanation = baseline_result.explain_plant(plant).unwrap();
    assert_eq!(explanation.record.plant, Some(plant));
    assert_eq!(explanation.record.family, Some(Uuid(702)));
    assert_eq!(
        explanation
            .decisions
            .last()
            .map(|(_, decision)| decision.outcome),
        Some(ProvenanceDecisionOutcome::Accepted)
    );
    let baseline = baseline_result.canonical_bytes().unwrap();

    original.regions.reverse();
    original.render_origin =
        WorldPosition::from_global_ticks([9_000_000_000, -2_000_000_000, 4_000_000_000]).unwrap();
    let shuffled = evaluate_cell_reference(&graph, &original, &GraphCancellationToken::default())
        .unwrap()
        .canonical_bytes()
        .unwrap();
    let repeated = evaluate_cell_reference(&graph, &original, &GraphCancellationToken::default())
        .unwrap()
        .canonical_bytes()
        .unwrap();
    assert_eq!(baseline, shuffled);
    assert_eq!(baseline, repeated);
}

#[test]
fn worker_counts_and_cell_request_order_are_byte_identical() {
    let graph = Arc::new(compile_fixture(0));
    let halo = graph.required_halo(0);
    let cells = [
        WorldCellKey::base(0, 0, 0),
        WorldCellKey::base(1, 0, 0),
        WorldCellKey::base(-1, 0, 1),
        WorldCellKey::base(2, 0, -1),
    ];
    let serial = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .evaluate(
            job(cells.iter().map(|cell| input(*cell, halo)).collect()),
            &GraphCancellationToken::default(),
        )
        .unwrap();
    let mut reversed = cells
        .iter()
        .rev()
        .map(|cell| input(*cell, halo))
        .collect::<Vec<_>>();
    reversed[0].regions.reverse();
    let parallel = BiomeGraphEvaluator::new(graph, 4)
        .unwrap()
        .evaluate(job(reversed), &GraphCancellationToken::default())
        .unwrap();
    let serial = serial
        .cells
        .into_iter()
        .map(|result| (result.cell, result.canonical_bytes().unwrap()))
        .collect::<BTreeMap<_, _>>();
    let parallel = parallel
        .cells
        .into_iter()
        .map(|result| (result.cell, result.canonical_bytes().unwrap()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(serial, parallel);
}

#[test]
fn global_stages_publish_once_and_cells_only_replay_immutable_tiles() {
    let graph = Arc::new(compile_global_fixture());
    let halo = graph.required_halo(0);
    let cells = vec![
        input(WorldCellKey::base(0, 0, 0), halo),
        input(WorldCellKey::base(1, 0, 0), halo),
    ];
    let reference_bounds = cells
        .iter()
        .map(|inputs| {
            (
                inputs.output_cell,
                ancestor_reference_upper_bound(&graph, inputs, true).unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let expected_tiles = expected_global_stage_tiles(&graph, &cells).unwrap();
    let result = BiomeGraphEvaluator::new(Arc::clone(&graph), 2)
        .unwrap()
        .evaluate(
            global_job(&graph, cells),
            &GraphCancellationToken::default(),
        )
        .unwrap();

    assert_eq!(result.global_stages.len(), expected_tiles.len());
    assert!(result.global_stages.iter().all(|tile| {
        tile.resident_bytes > 0
            && tile
                .result
                .diagnostics
                .nodes
                .iter()
                .any(|node| node.operator == GraphOperator::BlueNoisePoisson)
    }));
    assert!(
        result
            .global_stages
            .iter()
            .any(|tile| { tile.result.macro_points.row_count().unwrap() > 0 })
    );
    for cell in &result.cells {
        assert!(
            !cell
                .diagnostics
                .nodes
                .iter()
                .any(|node| node.operator == GraphOperator::BlueNoisePoisson)
        );
        assert_eq!(cell.macro_points.row_count().unwrap(), 0);
        assert!(!cell.ancestor_references.is_empty());
        assert!(cell.ancestor_references.capacity() as u64 <= reference_bounds[&cell.cell]);
    }
    let mut plant_ids = BTreeSet::new();
    for tile in &result.global_stages {
        for plant in &tile.result.macro_points.ids {
            assert!(plant_ids.insert(*plant));
            tile.result.explain_plant(*plant).unwrap();
        }
    }
}

#[test]
fn global_stage_set_must_be_complete_before_any_result_is_published() {
    let graph = Arc::new(compile_global_fixture());
    let halo = graph.required_halo(0);
    let mut inputs = global_job(&graph, vec![input(WorldCellKey::base(0, 0, 0), halo)]);
    inputs.global_stages.pop().unwrap();
    let error = BiomeGraphEvaluator::new(graph, 1)
        .unwrap()
        .evaluate(inputs, &GraphCancellationToken::default())
        .unwrap_err();
    assert!(
        matches!(error, Error::GraphDocument { path, .. } if path == "evaluation.globalStages")
    );
}

#[test]
fn qualified_compute_nodes_match_reference_bytes_through_the_single_facade() {
    let graph = Arc::new(compile_resident_branch_fixture());
    let halo = graph.required_halo(0);
    let inputs = input(WorldCellKey::base(0, 0, 0), halo);
    let reference = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .evaluate(
            job(vec![inputs.clone()]),
            &GraphCancellationToken::default(),
        )
        .unwrap()
        .cells
        .pop()
        .unwrap();
    let compute = BiomeGraphEvaluator::new(graph, 1)
        .unwrap()
        .with_compute_executor(reference_compute())
        .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
        .unwrap()
        .cells
        .pop()
        .unwrap();
    assert_eq!(
        reference.canonical_bytes().unwrap(),
        compute.canonical_bytes().unwrap()
    );
    assert!(compute.diagnostics.nodes.iter().any(|node| {
        node.symbol.starts_with("noise:")
            && node.execution_domain == GraphExecutionDomain::SlangCompute
            && node.transfer_bytes == 0
            && node.elapsed_micros == 0
    }));
    assert!(
        compute
            .diagnostics
            .gpu_groups
            .iter()
            .any(|group| { group.transfer_bytes > 0 && group.invocation_count > 0 })
    );
}

#[test]
fn branched_resident_subgraph_dispatches_once_and_matches_reference() {
    let graph = Arc::new(compile_resident_branch_fixture());
    let qualification = reference_compute();
    let plan = build_execution_plan(
        &graph,
        false,
        Some(GraphGpuScheduling {
            profile: qualification.profile(),
            qualifications: qualification.qualifications(),
        }),
    )
    .unwrap();
    assert_eq!(plan.domain_for(&[], 15), None);
    let halo = graph.required_halo(0);
    let inputs = input(WorldCellKey::base(0, 0, 0), halo);
    let reference = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .evaluate(
            job(vec![inputs.clone()]),
            &GraphCancellationToken::default(),
        )
        .unwrap()
        .cells
        .pop()
        .unwrap();
    let dispatches = Arc::new(AtomicUsize::new(0));
    let compute = BiomeGraphEvaluator::new(graph, 1)
        .unwrap()
        .with_compute_executor(counting_compute(Arc::clone(&dispatches)))
        .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
        .unwrap()
        .cells
        .pop()
        .unwrap();
    assert_eq!(
        reference.canonical_bytes().unwrap(),
        compute.canonical_bytes().unwrap()
    );
    assert_eq!(
        dispatches.load(Ordering::SeqCst),
        compute.diagnostics.gpu_groups.len()
    );
    assert_eq!(
        compute.diagnostics.gpu_groups.len(),
        1,
        "{:?}",
        compute.diagnostics.gpu_groups
    );
    assert_eq!(
        compute.diagnostics.gpu_groups[0]
            .nodes
            .iter()
            .map(|node| node.node)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([6, 10, 11, 12, 13, 14])
    );
}

/// Uniqueness and spacing both get *easier* to satisfy as points disappear, so a seam needs an
/// oracle for the missing direction too: every candidate a cell rejects as `ForeignOwner` has to
/// come back published by the cell that owns it, and the pinned per-cell counts fail on any drop
/// the halo swallows on both sides.
#[test]
fn halo_faces_and_corners_publish_every_owned_point_exactly_once() {
    let graph = compile_fixture(0);
    let halo = graph.required_halo(0);
    assert!(
        halo > DecisionScalar::from_bits(0),
        "a graph without a halo cannot exercise a seam"
    );
    let seam_counts = BTreeMap::from([
        (WorldCellKey::base(0, 0, 0), (65_u64, 14_u64)),
        (WorldCellKey::base(1, 0, 0), (60, 13)),
        (WorldCellKey::base(0, 0, 1), (69, 11)),
        (WorldCellKey::base(1, 0, 1), (63, 16)),
    ]);
    let cells = seam_counts.keys().copied().collect::<Vec<_>>();
    let block = seam_counts.keys().copied().collect::<BTreeSet<_>>();
    let results = evaluate_job(
        &graph,
        job(cells.iter().map(|cell| input(*cell, halo)).collect()),
        4,
        &GraphCancellationToken::default(),
        None,
    )
    .unwrap()
    .cells;
    assert_eq!(
        results
            .iter()
            .map(|result| result.cell)
            .collect::<BTreeSet<_>>(),
        block
    );

    let mut ids = BTreeSet::new();
    let mut positions = Vec::new();
    let mut published = BTreeSet::new();
    for result in &results {
        for row in 0..result.macro_points.row_count().unwrap() {
            assert_eq!(result.macro_points.owner_cells[row], result.cell);
            assert!(ids.insert(result.macro_points.ids[row]));
            positions.push(position(&result.macro_points, row));
            assert!(published.insert((result.cell, result.macro_points.candidates[row])));
        }
    }
    assert_eq!(
        results
            .iter()
            .map(|result| (
                result.cell,
                (
                    result.diagnostics.candidate_count,
                    result.diagnostics.accepted_count,
                    result.macro_points.row_count().unwrap() as u64,
                )
            ))
            .collect::<BTreeMap<_, _>>(),
        seam_counts
            .iter()
            .map(|(cell, (candidates, accepted))| (*cell, (*candidates, *accepted, *accepted)))
            .collect::<BTreeMap<_, _>>()
    );

    let mut foreign = 0_usize;
    for result in &results {
        for rejected in &result.diagnostics.rejected {
            if rejected.reason != CandidateRejectionReason::ForeignOwner {
                continue;
            }
            let owner = rejected.position.cell();
            if !block.contains(&owner) {
                continue;
            }
            foreign += 1;
            assert!(
                published.contains(&(owner, rejected.candidate.ordinal)),
                "{owner:?} candidate {} vanished at a seam: {:?} rejected it as foreign and its \
                 owner never published it",
                rejected.candidate.ordinal,
                result.cell
            );
        }
    }
    assert!(
        foreign > 0,
        "the seam oracle needs at least one cross-cell halo candidate"
    );

    // Partitioning may not move a decision: the same cells evaluated one at a time have to agree
    // byte for byte with the four-worker block.
    for result in &results {
        let reference = evaluate_cell_reference(
            &graph,
            &input(result.cell, halo),
            &GraphCancellationToken::default(),
        )
        .unwrap();
        assert_eq!(
            result.canonical_bytes().unwrap(),
            reference.canonical_bytes().unwrap(),
            "{:?} differs between the partitioned block and a single-cell evaluation",
            result.cell
        );
    }

    let required = i128::from(2 * LOCAL_TICKS_PER_METER);
    for left in 0..positions.len() {
        for right in left + 1..positions.len() {
            assert!(
                distance_squared_xz(positions[left], positions[right]).unwrap()
                    >= required * required
            );
        }
    }
}

#[test]
fn coarse_outputs_publish_once_and_fine_cells_reference_the_ancestor() {
    let graph = compile_fixture(1);
    let coarse_cell = WorldCellKey::new(0, 0, 0, 1).unwrap();
    let coarse = evaluate_cell_reference(
        &graph,
        &input(coarse_cell, graph.required_halo(1)),
        &GraphCancellationToken::default(),
    )
    .unwrap();
    assert!(coarse.macro_points.row_count().unwrap() > 0);
    assert!(
        coarse
            .macro_points
            .owner_cells
            .iter()
            .all(|owner| *owner == coarse_cell)
    );

    let fine = evaluate_cell_reference(
        &graph,
        &input(WorldCellKey::base(0, 0, 0), graph.required_halo(0)),
        &GraphCancellationToken::default(),
    )
    .unwrap();
    assert_eq!(fine.macro_points.row_count().unwrap(), 0);
    assert_eq!(fine.ancestor_references, vec![coarse_cell]);
}

#[test]
fn unrelated_node_edits_preserve_accepted_ids_and_preview_reports_destructive_edits() {
    let graph = compile_fixture(0);
    let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    let previous =
        evaluate_cell_reference(&graph, &inputs, &GraphCancellationToken::default()).unwrap();

    let mut asset = fixture_asset(0);
    let mut document = BiomeGraphDocument::from_json(&asset.graph).unwrap();
    document
        .nodes
        .iter_mut()
        .find(|node| node.guid == 5)
        .unwrap()
        .semantic_revision = 2;
    asset.graph = document.to_json();
    let unrelated = compile_biome_graph(
        &asset,
        &[],
        &NoDependencies,
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    let unchanged =
        evaluate_cell_reference(&unrelated, &inputs, &GraphCancellationToken::default()).unwrap();
    assert_eq!(previous.macro_points.ids, unchanged.macro_points.ids);

    let mut changed_asset = fixture_asset(0);
    let mut changed = BiomeGraphDocument::from_json(&changed_asset.graph).unwrap();
    changed
        .nodes
        .iter_mut()
        .find(|node| node.guid == 2)
        .unwrap()
        .semantic_revision = 2;
    changed_asset.graph = changed.to_json();
    let changed_graph = compile_biome_graph(
        &changed_asset,
        &[],
        &NoDependencies,
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    let proposed =
        evaluate_cell_reference(&changed_graph, &inputs, &GraphCancellationToken::default())
            .unwrap();
    let pin = previous.macro_points.ids[0];
    let preview = preview_graph_identity_edit(&previous, &proposed, &[pin], &[pin]).unwrap();
    assert!(!preview.accepted.is_empty());
    assert!(!preview.removed.is_empty());
    assert_eq!(preview.conflicts.invalidated_pins, vec![pin]);
    assert_eq!(preview.conflicts.invalidated_overrides, vec![pin]);
}

#[test]
fn cancellation_and_missing_halo_abort_without_a_result() {
    let graph = compile_fixture(0);
    let cancellation = GraphCancellationToken::default();
    cancellation.cancel();
    assert!(matches!(
        evaluate_cell_reference(
            &graph,
            &input(WorldCellKey::base(0, 0, 0), graph.required_halo(0)),
            &cancellation,
        ),
        Err(Error::GraphCancelled)
    ));
    assert!(matches!(
        evaluate_cell_reference(
            &graph,
            &input(WorldCellKey::base(0, 0, 0), DecisionScalar::from_bits(0),),
            &GraphCancellationToken::default(),
        ),
        Err(Error::GraphAuthoritativeInput { .. })
    ));
}
