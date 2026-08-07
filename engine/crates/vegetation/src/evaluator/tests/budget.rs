use super::*;

#[test]
fn public_preflight_global_work_is_aggregated_once_per_owner() {
    let graph = Arc::new(compile_global_fixture());
    let halo = graph.required_halo(0);
    let first = input(WorldCellKey::base(0, 0, 0), halo);
    let second = input(WorldCellKey::base(1, 0, 0), halo);
    let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 2).unwrap();
    let cancellation = GraphCancellationToken::default();
    let first_bound = evaluator
        .preflight(&global_job(&graph, vec![first.clone()]), &cancellation)
        .unwrap();
    let second_bound = evaluator
        .preflight(&global_job(&graph, vec![second.clone()]), &cancellation)
        .unwrap();
    let combined_job = global_job(&graph, vec![first, second]);
    let combined_bound = evaluator.preflight(&combined_job, &cancellation).unwrap();
    let actual = evaluator.evaluate(combined_job, &cancellation).unwrap();

    assert_eq!(
        combined_bound.global_stage_tiles as usize,
        actual.global_stages.len()
    );
    assert!(
        combined_bound.global_stage_tiles
            < first_bound.global_stage_tiles + second_bound.global_stage_tiles
    );
    assert!(
        combined_bound.candidate_count < first_bound.candidate_count + second_bound.candidate_count
    );
}

#[test]
fn public_preflight_bounds_complete_retained_job_results() {
    let graph = Arc::new(compile_global_fixture());
    let halo = graph.required_halo(0);
    let inputs = global_job(
        &graph,
        vec![
            input(WorldCellKey::base(0, 0, 0), halo),
            input(WorldCellKey::base(1, 0, 0), halo),
        ],
    );
    let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 2).unwrap();
    let cancellation = GraphCancellationToken::default();
    let bound = evaluator.preflight(&inputs, &cancellation).unwrap();
    let actual = evaluator.evaluate(inputs, &cancellation).unwrap();
    let result_bytes =
        actual
            .cells
            .iter()
            .map(|result| result.canonical_bytes().unwrap().len() as u64)
            .chain(actual.global_stages.iter().map(|tile| {
                tile.resident_bytes + tile.result.canonical_bytes().unwrap().len() as u64
            }))
            .sum::<u64>();
    let candidates = actual
        .cells
        .iter()
        .map(|result| result.diagnostics.candidate_count)
        .chain(
            actual
                .global_stages
                .iter()
                .map(|tile| tile.result.diagnostics.candidate_count),
        )
        .sum::<u64>();
    assert!(bound.memory_bytes >= bound.retained_input_bytes + result_bytes);
    assert!(bound.candidate_count >= candidates);
}

#[test]
fn public_preflight_enforces_every_job_resource_cap() {
    let cancellation = GraphCancellationToken::default();

    let mut graph = compile_fixture(0);
    graph.limits.max_workers = 1;
    let error = BiomeGraphEvaluator::new(Arc::new(graph), 2).err().unwrap();
    assert_graph_limit(error, "worker count");

    let mut graph = compile_fixture(0);
    let halo = graph.required_halo(0);
    let inputs = job(vec![
        input(WorldCellKey::base(0, 0, 0), halo),
        input(WorldCellKey::base(1, 0, 0), halo),
    ]);
    graph.limits.max_output_cells = 1;
    let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap_err();
    assert_graph_limit(error, "output cells");

    let mut graph = compile_document(recursive_document());
    let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    inputs
        .set_hierarchical_region(81, inputs.output_bounds)
        .unwrap();
    graph.limits.max_candidates = 13;
    let inputs = global_job(&graph, vec![inputs]);
    let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap_err();
    assert_graph_limit(error, "candidate count");

    let mut graph = compile_document(explicit_anchor_document());
    let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    inputs.anchors = vec![
        explicit_point(0, 42, WorldPosition::from_global_ticks([0, 0, 0]).unwrap()),
        explicit_point(
            1,
            42,
            WorldPosition::from_global_ticks([i128::from(LOCAL_TICKS_PER_METER), 0, 0]).unwrap(),
        ),
    ];
    graph.limits.max_macro_points = 1;
    let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
        .unwrap()
        .preflight(&job(vec![inputs]), &cancellation)
        .unwrap_err();
    assert_graph_limit(error, "accepted count");

    let mut graph = compile_document(micro_document());
    let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    graph.limits.max_micro_samples = 63;
    let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
        .unwrap()
        .preflight(&job(vec![inputs]), &cancellation)
        .unwrap_err();
    assert_graph_limit(error, "micro samples");

    let graph = Arc::new(compile_fixture(0));
    let inputs = job(vec![input(
        WorldCellKey::base(0, 0, 0),
        graph.required_halo(0),
    )]);
    let memory = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap()
        .memory_bytes;
    let mut limited = (*graph).clone();
    limited.limits.max_memory_bytes = memory - 1;
    let error = BiomeGraphEvaluator::new(Arc::new(limited), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap_err();
    assert_graph_limit(error, "memory bytes");

    let graph = Arc::new(compile_resident_branch_fixture());
    let inputs = job(vec![input(
        WorldCellKey::base(0, 0, 0),
        graph.required_halo(0),
    )]);
    let compute = reference_compute();
    let transfer = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .with_compute_executor(Arc::clone(&compute))
        .preflight(&inputs, &cancellation)
        .unwrap()
        .transfer_bytes;
    assert!(transfer > 0);
    let mut limited = (*graph).clone();
    limited.limits.max_transfer_bytes = transfer - 1;
    let error = BiomeGraphEvaluator::new(Arc::new(limited), 1)
        .unwrap()
        .with_compute_executor(compute)
        .preflight(&inputs, &cancellation)
        .unwrap_err();
    assert_graph_limit(error, "transfer bytes");

    let mut graph = compile_fixture(0);
    let inputs = job(vec![input(
        WorldCellKey::base(0, 0, 0),
        graph.required_halo(0),
    )]);
    graph.limits.max_time_ms = 0;
    let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap_err();
    assert_graph_limit(error, "time milliseconds");

    let mut graph = compile_fixture(0);
    let inputs = job(vec![input(
        WorldCellKey::base(0, 0, 0),
        graph.required_halo(0),
    )]);
    graph.limits.max_input_tiles = 0;
    let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap_err();
    assert_graph_limit(error, "input tiles");

    let mut graph = compile_global_fixture();
    let inputs = global_job(
        &graph,
        vec![input(WorldCellKey::base(0, 0, 0), graph.required_halo(0))],
    );
    graph.limits.max_global_stage_tiles = inputs.global_stages.len() as u64 - 1;
    let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap_err();
    assert_graph_limit(error, "global stage tiles");
}

#[test]
fn ancestor_reference_memory_is_input_specific_and_keeps_the_exact_gate() {
    let graph = compile_fixture(0);
    assert_eq!(graph.limits.max_global_stage_tiles, 1_000_000);
    let cell = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    assert_eq!(
        ancestor_reference_upper_bound(&graph, &cell, true).unwrap(),
        0
    );
    let inputs = job(vec![cell]);
    let cancellation = GraphCancellationToken::default();
    let baseline = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap();

    let mut no_global_tiles = graph;
    no_global_tiles.limits.max_global_stage_tiles = 0;
    let uncharged = BiomeGraphEvaluator::new(Arc::new(no_global_tiles.clone()), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap();
    assert_eq!(uncharged.memory_bytes, baseline.memory_bytes);

    let mut exact = no_global_tiles.clone();
    exact.limits.max_memory_bytes = uncharged.memory_bytes;
    assert!(
        BiomeGraphEvaluator::new(Arc::new(exact), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .is_ok()
    );

    no_global_tiles.limits.max_memory_bytes = uncharged.memory_bytes - 1;
    let error = BiomeGraphEvaluator::new(Arc::new(no_global_tiles), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap_err();
    assert_graph_limit(error, "memory bytes");
}

#[test]
fn ancestor_reference_bound_deduplicates_same_level_macro_stages() {
    let graph = compile_same_level_macro_stages_fixture();
    let stages = graph.spatial_plan().global_stages();
    assert_eq!(stages.len(), 2);
    assert!(
        stages
            .iter()
            .all(|stage| global_stage_has_macro_output(&graph, stage).unwrap())
    );
    let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    let covered = world_cell_count_covering_bounds(
        inputs.read_bounds,
        stages[0].owner_level,
        graph.limits.max_global_stage_tiles,
    )
    .unwrap();
    assert_eq!(
        ancestor_reference_upper_bound(&graph, &inputs, true).unwrap(),
        covered
    );
}

#[test]
fn candidate_global_stage_charges_one_coarse_owner_and_global_scope_charges_no_imports() {
    let candidate_graph = compile_candidate_only_global_fixture();
    let candidate_stage = &candidate_graph.spatial_plan().global_stages()[0];
    assert!(!global_stage_has_macro_output(&candidate_graph, candidate_stage).unwrap());
    let mut cell = input(
        WorldCellKey::base(0, 0, 0),
        candidate_graph.required_halo(0),
    );
    let edge = i128::from(BASE_CELL_TICKS);
    cell.read_bounds = WorldBounds::new([-1; 3], [edge + 1; 3]).unwrap();
    assert!(
        world_cell_count_covering_bounds(
            cell.read_bounds,
            candidate_stage.owner_level,
            candidate_graph.limits.max_global_stage_tiles,
        )
        .unwrap()
            > 1
    );
    assert_eq!(
        ancestor_reference_upper_bound(&candidate_graph, &cell, true).unwrap(),
        1
    );

    let macro_graph = compile_global_fixture();
    let macro_job = global_job(
        &macro_graph,
        vec![input(
            WorldCellKey::base(0, 0, 0),
            macro_graph.required_halo(0),
        )],
    );
    for stage in &macro_job.global_stages {
        assert_eq!(
            ancestor_reference_upper_bound(&macro_graph, &stage.inputs, false).unwrap(),
            0
        );
    }
}

#[test]
fn memory_peak_boundary_is_exact_and_rejects_before_gpu_dispatch() {
    let graph = Arc::new(compile_resident_branch_fixture());
    let inputs = job(vec![input(
        WorldCellKey::base(0, 0, 0),
        graph.required_halo(0),
    )]);
    let cancellation = GraphCancellationToken::default();
    let baseline = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .with_compute_executor(reference_compute())
        .preflight(&inputs, &cancellation)
        .unwrap();
    assert_eq!(
        baseline.memory_bytes,
        baseline
            .preflight_peak_bytes
            .max(baseline.execution_peak_bytes)
    );
    assert!(baseline.preflight_peak_bytes > 0);
    assert!(baseline.execution_peak_bytes > 0);

    let mut exact_graph = (*graph).clone();
    exact_graph.limits.max_memory_bytes = baseline.memory_bytes;
    let exact_dispatches = Arc::new(AtomicUsize::new(0));
    let exact = BiomeGraphEvaluator::new(Arc::new(exact_graph), 1)
        .unwrap()
        .with_compute_executor(counting_compute(Arc::clone(&exact_dispatches)));
    let exact_preflight = exact.preflight(&inputs, &cancellation).unwrap();
    assert_eq!(exact_preflight.memory_bytes, baseline.memory_bytes);
    exact.evaluate(inputs.clone(), &cancellation).unwrap();
    assert_eq!(exact_dispatches.load(Ordering::SeqCst), 1);

    let mut rejected_graph = (*graph).clone();
    rejected_graph.limits.max_memory_bytes = baseline.memory_bytes - 1;
    let rejected_dispatches = Arc::new(AtomicUsize::new(0));
    let rejected = BiomeGraphEvaluator::new(Arc::new(rejected_graph), 1)
        .unwrap()
        .with_compute_executor(counting_compute(Arc::clone(&rejected_dispatches)));
    let error = rejected.preflight(&inputs, &cancellation).unwrap_err();
    assert!(matches!(
        error,
        Error::GraphLimit {
            resource: "memory bytes",
            requested,
            limit,
        } if requested == baseline.memory_bytes && limit == baseline.memory_bytes - 1
    ));
    assert!(matches!(
        rejected.evaluate(inputs, &cancellation),
        Err(Error::GraphLimit {
            resource: "memory bytes",
            ..
        })
    ));
    assert_eq!(rejected_dispatches.load(Ordering::SeqCst), 0);
}

#[test]
fn preflight_separates_live_provider_generation_from_replay_retention() {
    let cell = WorldCellKey::base(0, 0, 0);
    let descriptor = SurfaceProviderDescriptor {
        id: SurfaceProviderId(77),
        revision: SurfaceRevision(3),
        bounds: cell.bounds(),
        primitive_count: 1,
        max_tags_per_hit: 0,
        capabilities: SurfaceCapabilities {
            authoritative_fields: true,
            ..SurfaceCapabilities::default()
        },
    };
    let provider: Arc<dyn SurfaceField> = Arc::new(TestSurfaceField {
        descriptor,
        failing_cell_x: None,
        project_hits: false,
        successful_samples: Arc::new(AtomicUsize::new(0)),
    });
    let provider_set_hash = canonical_surface_provider_set_hash(
        &[Arc::clone(&provider)],
        crate::GraphSafetyLimits::default().max_input_tiles,
    )
    .unwrap();
    let graph = Arc::new(compile_surface_field_fixture(provider_set_hash));
    let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
    let cancellation = GraphCancellationToken::default();

    let mut live_input = input(cell, graph.required_halo(0));
    live_input.surface_provider_set_hash = provider_set_hash;
    live_input.surface_providers.push(provider);
    let retained_live_tiles = evaluation_input_tile_count(&live_input).unwrap();
    let live_job = job(vec![live_input]);
    let live_preflight = evaluator.preflight(&live_job, &cancellation).unwrap();
    assert!(live_preflight.generated_input_bytes > 0);
    assert_eq!(live_preflight.input_tiles, retained_live_tiles + 1);
    let live_result = evaluator
        .evaluate(live_job, &cancellation)
        .unwrap()
        .cells
        .pop()
        .unwrap();
    assert_eq!(live_result.surface_field_query_tiles.len(), 1);

    let mut replay_input = input(cell, graph.required_halo(0));
    replay_input.surface_provider_set_hash = provider_set_hash;
    replay_input.surface_field_query_tiles = live_result.surface_field_query_tiles.clone();
    let retained_replay_tiles = evaluation_input_tile_count(&replay_input).unwrap();
    let replay_job = job(vec![replay_input]);
    let replay_preflight = evaluator.preflight(&replay_job, &cancellation).unwrap();
    assert_eq!(replay_preflight.generated_input_bytes, 0);
    assert_eq!(replay_preflight.input_tiles, retained_replay_tiles);
    assert!(replay_preflight.retained_input_bytes > live_preflight.retained_input_bytes);
    let replay_result = evaluator
        .evaluate(replay_job, &cancellation)
        .unwrap()
        .cells
        .pop()
        .unwrap();
    assert_eq!(
        live_result.canonical_bytes().unwrap(),
        replay_result.canonical_bytes().unwrap()
    );
}

#[test]
fn dead_authoritative_surface_branch_has_zero_work_and_zero_plan_cost() {
    let cell = WorldCellKey::base(0, 0, 0);
    let successful_samples = Arc::new(AtomicUsize::new(0));
    let descriptor = SurfaceProviderDescriptor {
        id: SurfaceProviderId(77),
        revision: SurfaceRevision(3),
        bounds: cell.bounds(),
        primitive_count: 1,
        max_tags_per_hit: 0,
        capabilities: SurfaceCapabilities {
            authoritative_fields: true,
            ..SurfaceCapabilities::default()
        },
    };
    let provider: Arc<dyn SurfaceField> = Arc::new(TestSurfaceField {
        descriptor,
        failing_cell_x: None,
        project_hits: false,
        successful_samples: Arc::clone(&successful_samples),
    });
    let provider_set_hash = canonical_surface_provider_set_hash(
        &[Arc::clone(&provider)],
        crate::GraphSafetyLimits::default().max_input_tiles,
    )
    .unwrap();
    let baseline_graph = Arc::new(compile_dead_surface_branch_fixture(
        provider_set_hash,
        false,
    ));
    let dead_graph = Arc::new(compile_dead_surface_branch_fixture(provider_set_hash, true));
    let make_job = |graph: &CompiledBiomeGraph| {
        let mut inputs = input(cell, graph.required_halo(0));
        inputs.surface_provider_set_hash = provider_set_hash;
        inputs.surface_providers.push(Arc::clone(&provider));
        job(vec![inputs])
    };
    let cancellation = GraphCancellationToken::default();
    let baseline_evaluator = BiomeGraphEvaluator::new(Arc::clone(&baseline_graph), 1).unwrap();
    let dead_evaluator = BiomeGraphEvaluator::new(Arc::clone(&dead_graph), 1).unwrap();
    let baseline_job = make_job(&baseline_graph);
    let dead_job = make_job(&dead_graph);
    assert_eq!(
        baseline_evaluator
            .preflight(&baseline_job, &cancellation)
            .unwrap(),
        dead_evaluator.preflight(&dead_job, &cancellation).unwrap()
    );
    let baseline = baseline_evaluator
        .evaluate(baseline_job, &cancellation)
        .unwrap()
        .cells
        .pop()
        .unwrap();
    let dead = dead_evaluator
        .evaluate(dead_job, &cancellation)
        .unwrap()
        .cells
        .pop()
        .unwrap();
    assert_eq!(successful_samples.load(Ordering::SeqCst), 0);
    assert!(dead.surface_projection_tiles.is_empty());
    assert!(dead.surface_field_query_tiles.is_empty());
    assert_eq!(
        baseline.canonical_bytes().unwrap(),
        dead.canonical_bytes().unwrap()
    );
}
