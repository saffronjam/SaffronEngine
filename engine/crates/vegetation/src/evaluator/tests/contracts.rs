use super::*;

#[test]
fn surface_hit_contract_enforces_declared_tags_and_identity() {
    let graph = compile_document(explicit_anchor_document());
    let node = &graph.root.nodes[0];
    let mut descriptor = SurfaceProviderDescriptor {
        id: SurfaceProviderId(77),
        revision: SurfaceRevision(3),
        bounds: WorldCellKey::base(0, 0, 0).bounds(),
        primitive_count: 1,
        max_tags_per_hit: 1,
        capabilities: saffron_spatial::SurfaceCapabilities::default(),
    };
    let mut hit = SurfaceHit {
        provider: descriptor.id,
        position: WorldPosition::origin(),
        distance_m: 0.0,
        frame: saffron_spatial::SurfaceFrame::from_normal(saffron_geometry::glam::Vec3::Y).unwrap(),
        coordinates: saffron_spatial::SurfaceCoordinates::default(),
        attachment: Some(
            SurfaceAttachment::new(
                descriptor.id,
                saffron_spatial::SurfacePrimitiveId(1),
                [UnitInterval::ONE, UnitInterval::ZERO, UnitInterval::ZERO],
                descriptor.revision,
            )
            .unwrap(),
        ),
        tags: vec![
            WeightedSurfaceTag {
                tag: saffron_spatial::SurfaceTagId(5),
                weight: UnitInterval::ONE,
            },
            WeightedSurfaceTag {
                tag: saffron_spatial::SurfaceTagId(9),
                weight: UnitInterval::ONE,
            },
        ],
        revision: descriptor.revision,
    };
    assert_graph_limit(
        validate_surface_hit_contract(node, &descriptor, &hit).unwrap_err(),
        "surface tags per hit",
    );
    descriptor.max_tags_per_hit = 2;
    validate_surface_hit_contract(node, &descriptor, &hit).unwrap();

    let valid = hit.clone();
    hit.provider = SurfaceProviderId(78);
    assert!(matches!(
        validate_surface_hit_contract(node, &descriptor, &hit),
        Err(Error::GraphDocument { .. })
    ));

    hit = valid.clone();
    hit.revision = SurfaceRevision(4);
    assert!(matches!(
        validate_surface_hit_contract(node, &descriptor, &hit),
        Err(Error::GraphDocument { .. })
    ));

    hit = valid.clone();
    hit.attachment.as_mut().unwrap().provider = SurfaceProviderId(78);
    assert!(matches!(
        validate_surface_hit_contract(node, &descriptor, &hit),
        Err(Error::GraphDocument { .. })
    ));

    hit = valid.clone();
    hit.attachment.as_mut().unwrap().revision = SurfaceRevision(4);
    assert!(matches!(
        validate_surface_hit_contract(node, &descriptor, &hit),
        Err(Error::GraphDocument { .. })
    ));

    hit = valid.clone();
    hit.tags.swap(0, 1);
    assert!(matches!(
        validate_surface_hit_contract(node, &descriptor, &hit),
        Err(Error::GraphDocument { .. })
    ));

    hit = valid;
    hit.tags[1].tag = hit.tags[0].tag;
    assert!(matches!(
        validate_surface_hit_contract(node, &descriptor, &hit),
        Err(Error::GraphDocument { .. })
    ));
}

#[test]
fn symbolic_preflight_uses_the_concrete_region_total() {
    let mut graph = compile_fixture(0);
    graph.limits.max_candidates = 30;
    let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    let mut second = inputs.regions[0];
    second.id = second.id.checked_add(1).unwrap();
    second.hierarchy_namespace = None;
    inputs.regions.push(second);
    let expected = inputs
        .regions
        .iter()
        .filter(|region| region.kind == EvaluationRegionKind::Biome)
        .count() as u64
        * 24;

    let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
        .unwrap()
        .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
        .unwrap_err();
    assert!(
        matches!(
            &error,
            Error::GraphLimit {
                resource: "candidate count",
                requested,
                limit: 30,
            } if *requested == expected
        ),
        "{error:?}"
    );
}

#[test]
fn symbolic_arithmetic_reports_a_typed_limit_before_u64_overflow() {
    assert!(matches!(
        bound_mul("candidate count", u64::MAX, 2, u64::MAX),
        Err(Error::GraphLimit {
            resource: "candidate count",
            requested: u64::MAX,
            limit: u64::MAX,
        })
    ));
}

#[test]
fn symbolic_gpu_transfer_matches_the_resident_program_abi() {
    let graph = Arc::new(compile_resident_branch_fixture());
    let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    let compute = reference_compute();
    let plan = build_execution_plan(
        &graph,
        false,
        Some(GraphGpuScheduling {
            profile: compute.profile(),
            qualifications: compute.qualifications(),
        }),
    )
    .unwrap();
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
    let actual = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .with_compute_executor(compute)
        .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
        .unwrap()
        .cells
        .pop()
        .unwrap();
    let actual_transfer = actual
        .diagnostics
        .gpu_groups
        .iter()
        .map(|group| group.transfer_bytes)
        .sum::<u64>();

    assert_eq!(bound.transfer_bytes, actual_transfer);
}
