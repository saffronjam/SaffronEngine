use super::*;

#[test]
fn symbolic_bound_is_above_the_actual_reference_evaluation() {
    let graph = compile_fixture(0);
    let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    let plan = build_execution_plan(&graph, false, None).unwrap();
    let global_store = SymbolicGlobalStore::default();
    let cancellation = GraphCancellationToken::default();
    let guard = PreflightGuard {
        cancellation: &cancellation,
        deadline: evaluation_deadline(&graph).unwrap(),
        time_limit_ms: graph.limits.max_time_ms,
    };
    let bound = symbolic_evaluation_bound(
        &graph,
        &inputs,
        &plan,
        SymbolicEvaluationScope::Cell {
            global_store: &global_store,
        },
        guard,
    )
    .unwrap()
    .bound;
    let actual =
        evaluate_cell_reference(&graph, &inputs, &GraphCancellationToken::default()).unwrap();
    let actual_node_bytes = actual
        .diagnostics
        .nodes
        .iter()
        .map(|node| node.output_bytes)
        .sum::<u64>();
    let canonical_bytes = actual.canonical_bytes().unwrap();
    let mut hasher = VegetationContentHasher::new();
    actual.update_content_hasher(&mut hasher).unwrap();

    assert_eq!(actual.canonical_byte_len().unwrap(), canonical_bytes.len());
    assert_eq!(hasher.finalize().unwrap(), sha256(&canonical_bytes));
    assert!(bound.candidate_peak >= actual.diagnostics.candidate_count);
    assert!(bound.accepted >= actual.macro_points.row_count().unwrap() as u64);
    assert!(bound.memory_bytes >= actual_node_bytes);
    assert_eq!(bound.rejected, bound.candidate_peak * 3);
    assert!(bound.rejected >= actual.diagnostics.rejected.len() as u64);
    assert_eq!(bound.transfer_bytes, 0);
}

#[test]
fn public_preflight_matches_anchor_spline_recursive_and_module_bounds() {
    let cancellation = GraphCancellationToken::default();

    let graph = Arc::new(compile_document(explicit_anchor_document()));
    let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    inputs.anchors = vec![
        explicit_point(0, 42, WorldPosition::from_global_ticks([0, 0, 0]).unwrap()),
        explicit_point(
            1,
            42,
            WorldPosition::from_global_ticks([i128::from(LOCAL_TICKS_PER_METER), 0, 0]).unwrap(),
        ),
        explicit_point(
            2,
            7,
            WorldPosition::from_global_ticks([2 * i128::from(LOCAL_TICKS_PER_METER), 0, 0])
                .unwrap(),
        ),
    ];
    let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 4).unwrap();
    let anchor_job = job(vec![inputs]);
    let bound = evaluator.preflight(&anchor_job, &cancellation).unwrap();
    let actual = evaluator.evaluate(anchor_job, &cancellation).unwrap();
    assert_eq!((bound.candidate_count, bound.accepted_count), (2, 2));
    assert_eq!(bound.worker_count, 1);
    assert_eq!(actual.cells[0].macro_points.row_count().unwrap(), 2);

    let graph = Arc::new(compile_document(spline_document()));
    let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    inputs.splines.push(EvaluationSpline {
        id: 1,
        layer: 1,
        points: vec![
            WorldPosition::from_global_ticks([0, 0, 0]).unwrap(),
            WorldPosition::from_global_ticks([10 * i128::from(LOCAL_TICKS_PER_METER), 0, 0])
                .unwrap(),
        ],
    });
    let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
    let spline_job = job(vec![inputs]);
    let bound = evaluator.preflight(&spline_job, &cancellation).unwrap();
    let actual = evaluator.evaluate(spline_job, &cancellation).unwrap();
    assert_eq!((bound.candidate_count, bound.accepted_count), (11, 11));
    assert_eq!(actual.cells[0].macro_points.row_count().unwrap(), 11);

    let graph = Arc::new(compile_document(recursive_document()));
    let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    inputs
        .set_hierarchical_region(71, inputs.output_bounds)
        .unwrap();
    let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
    let recursive_job = global_job(&graph, vec![inputs]);
    let bound = evaluator.preflight(&recursive_job, &cancellation).unwrap();
    let actual = evaluator.evaluate(recursive_job, &cancellation).unwrap();
    assert_eq!((bound.candidate_count, bound.accepted_count), (28, 14));
    assert_eq!(actual.cells[0].macro_points.row_count().unwrap(), 14);

    let graph = Arc::new(compile_module_fixture());
    let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    inputs
        .set_hierarchical_region(72, inputs.output_bounds)
        .unwrap();
    let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
    let module_job = job(vec![inputs]);
    let bound = evaluator.preflight(&module_job, &cancellation).unwrap();
    let actual = evaluator.evaluate(module_job, &cancellation).unwrap();
    assert_eq!((bound.candidate_count, bound.accepted_count), (9, 9));
    assert_eq!(actual.cells[0].macro_points.row_count().unwrap(), 9);
}

#[test]
fn module_output_demand_reaches_nested_earlier_global_stage_sources() {
    let graph = Arc::new(compile_module_global_prerequisite_fixture(false));
    let stages = graph.spatial_plan().global_stages();
    assert_eq!(stages.len(), 2);
    assert_eq!((stages[0].owner_level, stages[1].owner_level), (0, 2));
    assert!(stages[1].input_pins.contains(&QualifiedGraphPin {
        node: GraphNodeAddress {
            module_path: vec![99],
            node: 11,
        },
        pin: "candidates".to_owned(),
    }));

    let inputs = global_job(
        &graph,
        vec![input(WorldCellKey::base(0, 0, 0), graph.required_halo(0))],
    );
    let cancellation = GraphCancellationToken::default();
    let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
    evaluator.preflight(&inputs, &cancellation).unwrap();
    let result = evaluator.evaluate(inputs, &cancellation).unwrap();
    let later_stage = result
        .global_stages
        .iter()
        .find(|tile| tile.stage == stages[1].id)
        .unwrap();
    assert!(later_stage.result.diagnostics.nodes.iter().any(|node| {
        node.module_path.is_empty() && node.node == 6 && node.operator == GraphOperator::Transform
    }));
    assert!(!later_stage.result.diagnostics.nodes.iter().any(|node| {
        node.module_path == [99] && node.node == 11 && node.operator == GraphOperator::Transform
    }));
}

#[test]
fn global_module_load_boundary_does_not_request_cell_preparation() {
    let graph = compile_module_global_surface_fixture();
    assert!(
        demand_requires_canonical_preparation(&graph.root, graph.demand_plan().execution_slice(),)
            .unwrap()
    );
    assert!(
        !demand_requires_canonical_preparation(&graph.root, graph.demand_plan().public_slice(),)
            .unwrap()
    );
    let stage = &graph.spatial_plan().global_stages()[0];
    assert!(
        demand_requires_canonical_preparation(
            &graph.root,
            graph.demand_plan().stage_slice(stage.id).unwrap(),
        )
        .unwrap()
    );
}

#[test]
fn split_scope_global_loads_only_the_demanded_surface_projection_pin() {
    let descriptor = SurfaceProviderDescriptor {
        id: SurfaceProviderId(77),
        revision: SurfaceRevision(3),
        bounds: WorldCellKey::new(0, 0, 0, 2).unwrap().bounds(),
        primitive_count: 1,
        max_tags_per_hit: 0,
        capabilities: SurfaceCapabilities {
            project: true,
            authoritative_attachments: true,
            ..SurfaceCapabilities::default()
        },
    };
    let provider: Arc<dyn SurfaceField> = Arc::new(TestSurfaceField {
        descriptor,
        failing_cell_x: None,
        project_hits: false,
        successful_samples: Arc::new(AtomicUsize::new(0)),
    });
    let provider_hash = canonical_surface_provider_set_hash(
        &[Arc::clone(&provider)],
        crate::GraphSafetyLimits::default().max_input_tiles,
    )
    .unwrap();
    let graph = compile_split_surface_projection_fixture(provider_hash);
    let projection = GraphNodeAddress {
        module_path: Vec::new(),
        node: 3,
    };
    let transform = GraphNodeAddress {
        module_path: Vec::new(),
        node: 4,
    };
    let projection_stage = graph
        .spatial_plan()
        .global_stage_for_node(&projection)
        .unwrap();
    let transform_stage = graph
        .spatial_plan()
        .global_stage_for_node(&transform)
        .unwrap();
    assert_ne!(projection_stage.id, transform_stage.id);
    let projection_candidates = QualifiedGraphPin {
        node: projection.clone(),
        pin: "candidates".to_owned(),
    };
    let projection_surface = QualifiedGraphPin {
        node: projection,
        pin: "surface".to_owned(),
    };
    assert!(
        graph
            .demand_plan()
            .public_slice()
            .output_pins
            .contains(&projection_candidates)
    );
    assert!(
        !graph
            .demand_plan()
            .public_slice()
            .output_pins
            .contains(&projection_surface)
    );
    let later_demand = graph.demand_plan().stage_slice(transform_stage.id).unwrap();
    assert!(later_demand.output_pins.contains(&projection_candidates));
    assert!(later_demand.output_pins.contains(&projection_surface));

    let cell = WorldCellKey::base(0, 0, 0);
    let mut cell_input = input(cell, graph.required_halo(0));
    cell_input.surface_provider_set_hash = provider_hash;
    let mut inputs = global_job(&graph, vec![cell_input]);
    for stage_input in &mut inputs.global_stages {
        stage_input.inputs.surface_provider_set_hash = provider_hash;
        if stage_input.stage == projection_stage.id {
            stage_input
                .inputs
                .surface_providers
                .push(Arc::clone(&provider));
        }
    }
    let cancellation = GraphCancellationToken::default();
    let baseline = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap();
    let result = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
        .unwrap()
        .evaluate(inputs.clone(), &cancellation)
        .unwrap();
    assert!(result.cells[0].surface_projection_tiles.is_empty());
    assert!(
        result
            .global_stages
            .iter()
            .filter(|tile| tile.stage == transform_stage.id)
            .all(|tile| tile
                .result
                .diagnostics
                .nodes
                .iter()
                .any(|node| { node.node == 4 && node.operator == GraphOperator::Transform }))
    );

    let mut exact_graph = graph.clone();
    exact_graph.limits.max_memory_bytes = baseline.memory_bytes;
    BiomeGraphEvaluator::new(Arc::new(exact_graph), 1)
        .unwrap()
        .evaluate(inputs.clone(), &cancellation)
        .unwrap();
    let mut rejected_graph = graph;
    rejected_graph.limits.max_memory_bytes = baseline.memory_bytes - 1;
    let error = BiomeGraphEvaluator::new(Arc::new(rejected_graph), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap_err();
    assert!(matches!(
        error,
        Error::GraphLimit {
            resource: "memory bytes",
            requested,
            limit,
        } if requested == baseline.memory_bytes && limit == baseline.memory_bytes - 1
    ));
}

#[test]
fn surface_projection_candidates_only_materializes_exact_demand() {
    let (provider, provider_hash) = projection_provider();
    let graph = compile_projection_output_demand_fixture(provider_hash, false);
    let projection = graph
        .root
        .nodes
        .iter()
        .find(|node| node.definition.guid == 3)
        .unwrap();
    let demand = NodeOutputDemand::new(graph.demand_plan().public_slice(), projection);
    assert!(demand.contains("candidates"));
    assert!(!demand.contains("surface"));

    let inputs = projection_job(&graph, provider, provider_hash);
    let cancellation = GraphCancellationToken::default();
    let baseline = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap();
    let result = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
        .unwrap()
        .evaluate(inputs.clone(), &cancellation)
        .unwrap();
    let cell = &result.cells[0];
    assert_eq!(cell.macro_points.row_count().unwrap(), 4);
    assert_eq!(cell.surface_projection_tiles.len(), 1);
    let diagnostic = cell
        .diagnostics
        .nodes
        .iter()
        .find(|diagnostic| diagnostic.node == 3)
        .unwrap();
    assert_eq!(diagnostic.output_candidates, 108);
    assert_eq!(
        diagnostic.output_bytes,
        requested_vec_bytes::<GraphCandidate>(
            usize::try_from(diagnostic.output_candidates).unwrap()
        )
        .unwrap()
    );
    let explanation = cell.explain_plant(cell.macro_points.ids[0]).unwrap();
    assert!(explanation.decisions.iter().any(|(_, decision)| {
        decision.node == 3 && decision.operator == GraphOperator::SurfaceProjection
    }));

    let mut exact_graph = graph.clone();
    exact_graph.limits.max_memory_bytes = baseline.memory_bytes;
    BiomeGraphEvaluator::new(Arc::new(exact_graph), 1)
        .unwrap()
        .evaluate(inputs.clone(), &cancellation)
        .unwrap();
    let mut rejected_graph = graph;
    rejected_graph.limits.max_memory_bytes = baseline.memory_bytes - 1;
    let error = BiomeGraphEvaluator::new(Arc::new(rejected_graph), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap_err();
    assert!(matches!(
        error,
        Error::GraphLimit {
            resource: "memory bytes",
            requested,
            limit,
        } if requested == baseline.memory_bytes && limit == baseline.memory_bytes - 1
    ));
}

#[test]
fn surface_projection_surface_only_materializes_exact_demand() {
    let (provider, provider_hash) = projection_provider();
    let graph = compile_projection_output_demand_fixture(provider_hash, true);
    let projection = graph
        .root
        .nodes
        .iter()
        .find(|node| node.definition.guid == 3)
        .unwrap();
    let demand = NodeOutputDemand::new(graph.demand_plan().public_slice(), projection);
    assert!(!demand.contains("candidates"));
    assert!(demand.contains("surface"));

    let inputs = projection_job(&graph, provider, provider_hash);
    let cancellation = GraphCancellationToken::default();
    let baseline = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap();
    let result = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
        .unwrap()
        .evaluate(inputs.clone(), &cancellation)
        .unwrap();
    let cell = &result.cells[0];
    assert_eq!(cell.macro_points.row_count().unwrap(), 4);
    assert_eq!(cell.surface_projection_tiles.len(), 1);
    let diagnostic = cell
        .diagnostics
        .nodes
        .iter()
        .find(|diagnostic| diagnostic.node == 3)
        .unwrap();
    assert_eq!(diagnostic.output_candidates, 108);
    assert_eq!(
        diagnostic.output_bytes,
        requested_btree_bytes::<CandidateIdentity, ProjectedSurfaceSample>(
            usize::try_from(diagnostic.output_candidates).unwrap()
        )
        .unwrap()
    );
    let explanation = cell.explain_plant(cell.macro_points.ids[0]).unwrap();
    assert!(explanation.decisions.iter().any(|(_, decision)| {
        decision.node == 3 && decision.operator == GraphOperator::SurfaceProjection
    }));

    let mut exact_graph = graph.clone();
    exact_graph.limits.max_memory_bytes = baseline.memory_bytes;
    BiomeGraphEvaluator::new(Arc::new(exact_graph), 1)
        .unwrap()
        .evaluate(inputs.clone(), &cancellation)
        .unwrap();
    let mut rejected_graph = graph;
    rejected_graph.limits.max_memory_bytes = baseline.memory_bytes - 1;
    let error = BiomeGraphEvaluator::new(Arc::new(rejected_graph), 1)
        .unwrap()
        .preflight(&inputs, &cancellation)
        .unwrap_err();
    assert!(matches!(
        error,
        Error::GraphLimit {
            resource: "memory bytes",
            requested,
            limit,
        } if requested == baseline.memory_bytes && limit == baseline.memory_bytes - 1
    ));
}

#[test]
fn micro_output_replays_its_live_dynamic_attribute_channel() {
    let channel = 77_u128;
    let graph = Arc::new(compile_document(micro_attribute_document(channel)));
    let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    let result = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
        .unwrap()
        .cells
        .pop()
        .unwrap();
    let tile = &result.micro_fields[0];
    assert_eq!(tile.density.len(), 8);
    assert_eq!(tile.attributes[&channel].len(), 8);
    assert!(result.diagnostics.nodes.iter().any(|node| {
        node.node == 3 && node.operator == GraphOperator::Noise && node.output_bytes > 0
    }));
}

#[test]
fn micro_output_emits_one_canonical_tile_per_assigned_family() {
    let graph = Arc::new(compile_document(explicit_family_micro_document()));
    let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    let mut second = explicit_point(1, 42, WorldPosition::from_global_ticks([1, 0, 0]).unwrap());
    second.point.family = Uuid(703);
    inputs.anchors = vec![
        explicit_point(0, 42, WorldPosition::from_global_ticks([0, 0, 0]).unwrap()),
        second,
    ];

    let result = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
        .unwrap()
        .cells
        .pop()
        .unwrap();

    assert_eq!(
        result
            .micro_fields
            .iter()
            .map(|tile| tile.family.value())
            .collect::<Vec<_>>(),
        vec![702, 703]
    );
    assert_ne!(
        result.micro_fields[0].reconstruction_seed,
        result.micro_fields[1].reconstruction_seed
    );
    assert!(result.canonical_bytes().is_ok());
}

#[test]
fn stage_materialized_module_output_is_demanded_without_a_consumer_edge() {
    let graph = Arc::new(compile_stage_materialized_module_output_fixture());
    assert!(graph.root.edges.is_empty());
    let stages = graph.spatial_plan().global_stages();
    assert_eq!(stages.len(), 1);
    assert_eq!(stages[0].owner_level, 2);
    assert_eq!(
        stages[0].nodes.iter().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            GraphNodeAddress {
                module_path: vec![99],
                node: 11,
            },
            GraphNodeAddress {
                module_path: vec![99],
                node: 13,
            },
        ])
    );
    assert!(stages[0].output_pins.contains(&QualifiedGraphPin {
        node: GraphNodeAddress {
            module_path: vec![99],
            node: 13,
        },
        pin: "points".to_owned(),
    }));

    let inputs = global_job(
        &graph,
        vec![input(WorldCellKey::base(0, 0, 0), graph.required_halo(0))],
    );
    let cancellation = GraphCancellationToken::default();
    let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
    evaluator.preflight(&inputs, &cancellation).unwrap();
    let result = evaluator.evaluate(inputs, &cancellation).unwrap();
    assert!(result.global_stages.iter().all(|tile| {
        tile.result
            .diagnostics
            .nodes
            .iter()
            .any(|node| node.module_path == [99] && node.node == 13)
    }));
    assert!(result.global_stages.iter().all(|tile| {
        tile.result
            .diagnostics
            .nodes
            .iter()
            .all(|node| !(node.module_path == [99] && node.node == 14))
    }));
    assert!(
        result
            .global_stages
            .iter()
            .all(|tile| tile.resident_bytes > 0)
    );
    assert!(
        result.cells[0]
            .diagnostics
            .nodes
            .iter()
            .all(|node| node.module_path != [99])
    );
}

#[test]
fn dead_module_output_sibling_has_zero_bytes_and_preserves_the_exact_cap() {
    let baseline_graph = compile_module_fixture_with_dead_sibling(false);
    let sibling_graph = compile_module_fixture_with_dead_sibling(true);
    let baseline_inputs = job(vec![input(
        WorldCellKey::base(0, 0, 0),
        baseline_graph.required_halo(0),
    )]);
    let sibling_inputs = job(vec![input(
        WorldCellKey::base(0, 0, 0),
        sibling_graph.required_halo(0),
    )]);
    let cancellation = GraphCancellationToken::default();
    let baseline = BiomeGraphEvaluator::new(Arc::new(baseline_graph), 1)
        .unwrap()
        .preflight(&baseline_inputs, &cancellation)
        .unwrap();
    let sibling = BiomeGraphEvaluator::new(Arc::new(sibling_graph.clone()), 1)
        .unwrap()
        .preflight(&sibling_inputs, &cancellation)
        .unwrap();
    assert_eq!(sibling, baseline);

    let mut exact_graph = sibling_graph.clone();
    exact_graph.limits.max_memory_bytes = baseline.memory_bytes;
    let exact = BiomeGraphEvaluator::new(Arc::new(exact_graph), 1).unwrap();
    assert_eq!(
        exact
            .evaluate(sibling_inputs.clone(), &cancellation)
            .unwrap()
            .cells[0]
            .macro_points
            .row_count()
            .unwrap(),
        9
    );

    let mut rejected_graph = sibling_graph;
    rejected_graph.limits.max_memory_bytes = baseline.memory_bytes - 1;
    let error = BiomeGraphEvaluator::new(Arc::new(rejected_graph), 1)
        .unwrap()
        .preflight(&sibling_inputs, &cancellation)
        .unwrap_err();
    assert!(matches!(
        error,
        Error::GraphLimit {
            resource: "memory bytes",
            requested,
            limit,
        } if requested == baseline.memory_bytes && limit == baseline.memory_bytes - 1
    ));
}

#[test]
fn resident_earlier_module_stage_is_loaded_without_redispatch() {
    let graph = Arc::new(compile_module_global_prerequisite_fixture(true));
    let stages = graph.spatial_plan().global_stages();
    assert_eq!(stages.len(), 2);
    assert_eq!((stages[0].owner_level, stages[1].owner_level), (0, 2));
    assert_eq!(
        stages[0].nodes.iter().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            GraphNodeAddress {
                module_path: vec![99],
                node: 11,
            },
            GraphNodeAddress {
                module_path: vec![99],
                node: 12,
            },
        ])
    );

    let inputs = global_job(
        &graph,
        vec![input(WorldCellKey::base(0, 0, 0), graph.required_halo(0))],
    );
    let earlier_stage_tiles = inputs
        .global_stages
        .iter()
        .filter(|tile| tile.stage == stages[0].id)
        .count();
    let dispatches = Arc::new(AtomicUsize::new(0));
    let cancellation = GraphCancellationToken::default();
    let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .with_compute_executor(counting_compute(Arc::clone(&dispatches)));
    evaluator.preflight(&inputs, &cancellation).unwrap();
    let result = evaluator.evaluate(inputs, &cancellation).unwrap();

    assert_eq!(dispatches.load(Ordering::SeqCst), earlier_stage_tiles);
    assert!(
        result
            .global_stages
            .iter()
            .filter(|tile| tile.stage == stages[0].id)
            .all(|tile| tile.result.diagnostics.gpu_groups.len() == 1)
    );
    assert!(
        result
            .global_stages
            .iter()
            .filter(|tile| tile.stage == stages[1].id)
            .all(|tile| tile.result.diagnostics.gpu_groups.is_empty())
    );
    assert!(
        result
            .cells
            .iter()
            .all(|cell| cell.diagnostics.gpu_groups.is_empty())
    );
}
