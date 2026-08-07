use super::*;

#[test]
fn cancellation_and_deadline_abort_deterministically_at_every_publication_phase() {
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
    let mut inputs = input(cell, graph.required_halo(0));
    inputs.surface_provider_set_hash = provider_set_hash;
    inputs.surface_providers.push(provider);
    let inputs = job(vec![inputs]);

    for checkpoint in [
        TestEvaluationCheckpoint::AfterPreflight,
        TestEvaluationCheckpoint::AfterPreparation,
        TestEvaluationCheckpoint::AfterTraversal,
        TestEvaluationCheckpoint::BeforeFinalValidation,
        TestEvaluationCheckpoint::BeforePublication,
    ] {
        let cancellation = GraphCancellationToken::default();
        cancellation.abort_at_checkpoint(checkpoint, TestAbortKind::Cancelled);
        assert!(matches!(
            evaluator.evaluate(inputs.clone(), &cancellation),
            Err(Error::GraphCancelled)
        ));

        let deadline = GraphCancellationToken::default();
        deadline.abort_at_checkpoint(checkpoint, TestAbortKind::Deadline);
        assert!(matches!(
            evaluator.evaluate(inputs.clone(), &deadline),
            Err(Error::GraphLimit {
                resource: "time milliseconds",
                ..
            })
        ));
    }
}

#[test]
fn check_budget_cancels_the_final_tail_without_timing() {
    let graph = Arc::new(compile_fixture(0));
    let inputs = job(vec![input(
        WorldCellKey::base(0, 0, 0),
        graph.required_halo(0),
    )]);
    let evaluator = BiomeGraphEvaluator::new(graph, 1).unwrap();
    let observed = GraphCancellationToken::default();
    evaluator.evaluate(inputs.clone(), &observed).unwrap();
    let check_count = observed.observed_checks();
    assert!(check_count > 1);

    let cancellation = GraphCancellationToken::default();
    cancellation.cancel_after_checks(check_count - 1);
    assert!(matches!(
        evaluator.evaluate(inputs, &cancellation),
        Err(Error::GraphCancelled)
    ));
}

#[test]
fn multi_cell_runtime_failure_discards_earlier_completed_work() {
    let successful_samples = Arc::new(AtomicUsize::new(0));
    let descriptor = SurfaceProviderDescriptor {
        id: SurfaceProviderId(77),
        revision: SurfaceRevision(3),
        bounds: WorldCellKey::new(0, 0, 0, 1).unwrap().bounds(),
        primitive_count: 1,
        max_tags_per_hit: 0,
        capabilities: SurfaceCapabilities {
            authoritative_fields: true,
            ..SurfaceCapabilities::default()
        },
    };
    let provider: Arc<dyn SurfaceField> = Arc::new(TestSurfaceField {
        descriptor,
        failing_cell_x: Some(1),
        project_hits: false,
        successful_samples: Arc::clone(&successful_samples),
    });
    let provider_set_hash = canonical_surface_provider_set_hash(
        &[Arc::clone(&provider)],
        crate::GraphSafetyLimits::default().max_input_tiles,
    )
    .unwrap();
    let graph = Arc::new(compile_surface_field_fixture(provider_set_hash));
    let mut cells = Vec::new();
    for cell in [WorldCellKey::base(0, 0, 0), WorldCellKey::base(1, 0, 0)] {
        let mut cell_input = input(cell, graph.required_halo(0));
        cell_input.surface_provider_set_hash = provider_set_hash;
        cell_input.surface_providers.push(Arc::clone(&provider));
        cells.push(cell_input);
    }
    let error = BiomeGraphEvaluator::new(graph, 1)
        .unwrap()
        .evaluate(job(cells), &GraphCancellationToken::default())
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Spatial(saffron_spatial::Error::FieldUnavailable)
    ));
    assert_eq!(successful_samples.load(Ordering::SeqCst), 4);
}

#[test]
fn public_preflight_observes_entry_cancellation() {
    let graph = Arc::new(compile_fixture(0));
    let inputs = job(vec![input(
        WorldCellKey::base(0, 0, 0),
        graph.required_halo(0),
    )]);
    let cancellation = GraphCancellationToken::default();
    cancellation.cancel();
    assert!(matches!(
        BiomeGraphEvaluator::new(graph, 1)
            .unwrap()
            .preflight(&inputs, &cancellation),
        Err(Error::GraphCancelled)
    ));
}
