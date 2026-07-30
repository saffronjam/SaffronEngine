//! Compiled-IR and global-stage identity.

use super::*;

#[test]
fn dependency_content_hashes_participate_in_compiled_identity() {
    let mut document = simple_document();
    document
        .nodes
        .iter_mut()
        .find(|node| node.guid == 2)
        .unwrap()
        .dependencies = vec![GraphDependencySource::Asset(Uuid(99))];
    let asset = biome(document);
    let first = compile_biome_graph(
        &asset,
        &[],
        &HashedDependency(1),
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    let second = compile_biome_graph(
        &asset,
        &[],
        &HashedDependency(2),
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    assert_ne!(first.identity, second.identity);
    assert_eq!(first.dependencies()[0].content_hash, [1; 32]);
}

#[test]
fn execution_and_stage_identities_ignore_dead_global_branches() {
    let mut document = simple_document();
    document
        .nodes
        .iter_mut()
        .find(|node| node.guid == 2)
        .unwrap()
        .spatial = NodeSpatialPolicy::Global { level: 2 };
    let baseline = compile_biome_graph(
        &biome(document.clone()),
        &[],
        &NoModules,
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    assert_eq!(baseline.spatial_plan().global_stages().len(), 1);
    let mut other_root = biome(document.clone());
    other_root.id = Uuid(8);
    let other_root = compile_biome_graph(
        &other_root,
        &[],
        &NoModules,
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    assert_ne!(
        baseline.spatial_plan().global_stages()[0].id,
        other_root.spatial_plan().global_stages()[0].id
    );

    let dead_region = node(99, GraphOperator::RegionInput);
    let mut dead_coverage = node(100, GraphOperator::StratifiedCoverage);
    dead_coverage.spatial = NodeSpatialPolicy::Global { level: 4 };
    dead_coverage
        .seed_namespaces
        .insert("sampling".to_owned(), 11);
    dead_coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(u32::MAX));
    dead_coverage.parameters.insert(
        "jitter".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ZERO),
    );
    document.nodes.extend([dead_region, dead_coverage]);
    document.edges.push(GraphEdge {
        from_node: 99,
        from_pin: "regions".to_owned(),
        to_node: 100,
        to_pin: "regions".to_owned(),
    });
    let with_dead_branch = compile_biome_graph(
        &biome(document.clone()),
        &[],
        &NoModules,
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    compile_biome_graph(
        &biome(document.clone()),
        &[],
        &NoModules,
        GraphCompileOptions {
            limits: GraphSafetyLimits {
                max_candidates: 16,
                ..GraphSafetyLimits::default()
            },
        },
    )
    .unwrap();
    assert_eq!(baseline.identity, with_dead_branch.identity);
    assert_eq!(baseline.dependencies(), with_dead_branch.dependencies());
    assert_eq!(
        baseline
            .spatial_plan()
            .global_stages()
            .iter()
            .map(|stage| stage.id)
            .collect::<Vec<_>>(),
        with_dead_branch
            .spatial_plan()
            .global_stages()
            .iter()
            .map(|stage| stage.id)
            .collect::<Vec<_>>()
    );

    document
        .nodes
        .iter_mut()
        .find(|node| node.guid == 2)
        .unwrap()
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(17));
    assert!(matches!(
        compile_biome_graph(
            &biome(document.clone()),
            &[],
            &NoModules,
            GraphCompileOptions {
                limits: GraphSafetyLimits {
                    max_candidates: 16,
                    ..GraphSafetyLimits::default()
                },
            },
        ),
        Err(Error::GraphLimit {
            resource: "candidate count",
            requested: 17,
            limit: 16,
        })
    ));
    let live_edit = compile_biome_graph(
        &biome(document),
        &[],
        &NoModules,
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    assert_ne!(baseline.identity, live_edit.identity);
    assert_ne!(
        baseline.spatial_plan().global_stages()[0].id,
        live_edit.spatial_plan().global_stages()[0].id
    );
}

#[test]
fn execution_identity_is_invariant_to_dependency_order() {
    let mut document = simple_document();
    let live = document
        .nodes
        .iter_mut()
        .find(|node| node.guid == 2)
        .unwrap();
    live.spatial = NodeSpatialPolicy::Global { level: 2 };
    live.dependencies = vec![
        GraphDependencySource::Asset(Uuid(80)),
        GraphDependencySource::Asset(Uuid(81)),
    ];
    let first = compile_biome_graph(
        &biome(document.clone()),
        &[],
        &HashedDependency(7),
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    document
        .nodes
        .iter_mut()
        .find(|node| node.guid == 2)
        .unwrap()
        .dependencies
        .reverse();
    let second = compile_biome_graph(
        &biome(document),
        &[],
        &HashedDependency(7),
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    assert_eq!(first.identity, second.identity);
    assert_eq!(
        first.spatial_plan().global_stages()[0].id,
        second.spatial_plan().global_stages()[0].id
    );
}
