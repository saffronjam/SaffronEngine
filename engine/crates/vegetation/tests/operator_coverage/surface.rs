use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use saffron_geometry::glam::{DVec3, Vec3};
use saffron_spatial::{
    DecisionHessian3, DecisionScalar, DecisionVec3, FieldAvailability, FieldChannel,
    FieldDerivative, FieldSample, HessianFieldSample, SurfaceAttachment, SurfaceCapabilities,
    SurfaceCoordinates, SurfaceDirtyRegion, SurfaceField, SurfaceFrame, SurfaceHit,
    SurfaceNearestQuery, SurfacePrimitiveId, SurfaceProjection, SurfaceProviderDescriptor,
    SurfaceProviderId, SurfaceRay, SurfaceRevision, SurfaceTagId, SurfaceTileDescriptor,
    UnitInterval, VectorFieldSample, WeightedSurfaceTag, WorldBounds, WorldPosition,
};
use saffron_vegetation::{
    BiomeGraphEvaluator, GraphCancellationToken, GraphCompileOptions, GraphDependencySource,
    GraphEvaluationJobInputs, GraphOperator, GraphParameterValue, NodeSpatialPolicy,
    QuantizedFieldTileValues, QuantizedSurfaceFieldValue, canonical_surface_provider_set_hash,
    compile_biome_graph, precompute_surface_field_tile,
};

use crate::fixtures::{
    FIXED_ONE, FixtureResolver, asset, diagnostic_document, edge, evaluation_input, node,
};
use GraphOperator as O;

#[derive(Clone)]
struct CanonicalSurfaceProvider {
    descriptor: SurfaceProviderDescriptor,
}

impl CanonicalSurfaceProvider {
    fn new() -> Self {
        Self {
            descriptor: SurfaceProviderDescriptor {
                id: SurfaceProviderId(77),
                revision: SurfaceRevision(3),
                bounds: WorldBounds::new([-1_000_000_000_000; 3], [1_000_000_000_000; 3]).unwrap(),
                primitive_count: 1,
                max_tags_per_hit: 2,
                capabilities: SurfaceCapabilities {
                    ray: true,
                    project: true,
                    nearest: true,
                    uv: false,
                    authoritative_attachments: true,
                    authoritative_fields: true,
                },
            },
        }
    }

    fn hit(&self, position: WorldPosition, distance_m: f64) -> saffron_spatial::Result<SurfaceHit> {
        Ok(SurfaceHit {
            provider: self.descriptor.id,
            position,
            distance_m,
            frame: SurfaceFrame::from_normal(Vec3::Y)?,
            coordinates: SurfaceCoordinates {
                uv: None,
                projection: DVec3::new(0.25, 0.5, 0.75),
            },
            attachment: Some(SurfaceAttachment::new(
                self.descriptor.id,
                SurfacePrimitiveId(901),
                [UnitInterval::ONE, UnitInterval::ZERO, UnitInterval::ZERO],
                self.descriptor.revision,
            )?),
            tags: vec![
                WeightedSurfaceTag {
                    tag: SurfaceTagId(5),
                    weight: UnitInterval::from_bits(40_000),
                },
                WeightedSurfaceTag {
                    tag: SurfaceTagId(9),
                    weight: UnitInterval::from_bits(25_535),
                },
            ],
            revision: self.descriptor.revision,
        })
    }

    fn require_altitude(&self, channel: FieldChannel) -> saffron_spatial::Result<()> {
        if channel == FieldChannel::Altitude {
            Ok(())
        } else {
            Err(saffron_spatial::Error::FieldUnavailable)
        }
    }
}

impl SurfaceField for CanonicalSurfaceProvider {
    fn descriptor(&self) -> SurfaceProviderDescriptor {
        self.descriptor.clone()
    }

    fn field_channels(&self) -> Vec<FieldChannel> {
        vec![FieldChannel::Altitude]
    }

    fn raycast(&self, query: &SurfaceRay) -> saffron_spatial::Result<Option<SurfaceHit>> {
        let projection =
            SurfaceProjection::new(query.origin, query.direction, query.max_distance_m)?;
        self.project(&projection)
    }

    fn project(&self, query: &SurfaceProjection) -> saffron_spatial::Result<Option<SurfaceHit>> {
        if query.direction.x <= 0.0
            || query.direction.y >= 0.0
            || (query.direction.x.abs() - query.direction.y.abs()).abs() > 1.0e-12
        {
            return Err(saffron_spatial::Error::DegenerateDirection);
        }
        let origin = query.origin.global_ticks();
        if origin[1] < 0 {
            return Ok(None);
        }
        let projected = WorldPosition::from_global_ticks([
            origin[0]
                .checked_add(origin[1])
                .ok_or(saffron_spatial::Error::NumericOverflow)?,
            0,
            origin[2],
        ])?;
        let vertical_metres = origin[1] as f64 / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER);
        let distance_m = vertical_metres * 2.0_f64.sqrt();
        if distance_m > query.max_distance_m {
            return Ok(None);
        }
        self.hit(projected, distance_m).map(Some)
    }

    fn nearest(&self, query: &SurfaceNearestQuery) -> saffron_spatial::Result<Option<SurfaceHit>> {
        let ticks = query.position.global_ticks();
        let distance_m =
            ticks[1].unsigned_abs() as f64 / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER);
        if distance_m > query.max_distance_m {
            return Ok(None);
        }
        self.hit(
            WorldPosition::from_global_ticks([ticks[0], 0, ticks[2]])?,
            distance_m,
        )
        .map(Some)
    }

    fn availability(
        &self,
        channel: FieldChannel,
        _derivative: FieldDerivative,
        _bounds: WorldBounds,
    ) -> FieldAvailability {
        if channel == FieldChannel::Altitude {
            FieldAvailability::Complete
        } else {
            FieldAvailability::Unavailable
        }
    }

    fn estimated_samples(&self, channel: FieldChannel, _bounds: WorldBounds) -> u64 {
        u64::from(channel == FieldChannel::Altitude)
    }

    fn sample_scalar(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        _position: WorldPosition,
    ) -> saffron_spatial::Result<FieldSample> {
        self.require_altitude(channel)?;
        if derivative != FieldDerivative::Value {
            return Err(saffron_spatial::Error::FieldUnavailable);
        }
        Ok(FieldSample {
            channel,
            derivative,
            value: DecisionScalar::from_bits(12_345),
            revision: self.descriptor.revision,
        })
    }

    fn sample_vector(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        _position: WorldPosition,
    ) -> saffron_spatial::Result<VectorFieldSample> {
        self.require_altitude(channel)?;
        if derivative != FieldDerivative::Gradient {
            return Err(saffron_spatial::Error::FieldUnavailable);
        }
        Ok(VectorFieldSample {
            channel,
            derivative,
            value: DecisionVec3 {
                x: DecisionScalar::from_bits(100),
                y: DecisionScalar::from_bits(200),
                z: DecisionScalar::from_bits(300),
            },
            revision: self.descriptor.revision,
        })
    }

    fn sample_hessian(
        &self,
        channel: FieldChannel,
        _position: WorldPosition,
    ) -> saffron_spatial::Result<HessianFieldSample> {
        self.require_altitude(channel)?;
        Ok(HessianFieldSample {
            channel,
            derivative: FieldDerivative::Hessian,
            value: DecisionHessian3 {
                xx: DecisionScalar::from_bits(400),
                xy: DecisionScalar::from_bits(500),
                xz: DecisionScalar::from_bits(600),
                yy: DecisionScalar::from_bits(700),
                yz: DecisionScalar::from_bits(800),
                zz: DecisionScalar::from_bits(900),
            },
            revision: self.descriptor.revision,
        })
    }

    fn authoritative_tiles(
        &self,
        channel: FieldChannel,
        bounds: WorldBounds,
    ) -> Vec<SurfaceTileDescriptor> {
        if channel != FieldChannel::Altitude {
            return Vec::new();
        }
        vec![SurfaceTileDescriptor {
            provider: self.descriptor.id,
            revision: self.descriptor.revision,
            bounds,
            dimensions: [2, 2, 2],
            value_quantum_bits: 1,
        }]
    }

    fn changes_since(&self, _revision: SurfaceRevision) -> Vec<SurfaceDirtyRegion> {
        Vec::new()
    }

    fn reproject_attachment(
        &self,
        attachment: SurfaceAttachment,
    ) -> saffron_spatial::Result<Option<SurfaceHit>> {
        if attachment.provider != self.descriptor.id
            || attachment.primitive != SurfacePrimitiveId(901)
        {
            return Ok(None);
        }
        self.hit(WorldPosition::origin(), 0.0).map(Some)
    }
}

#[test]
fn canonical_provider_precomputes_all_derivatives_and_replays_live_queries() {
    let region = node(601, O::RegionInput);
    let mut coverage = node(602, O::StratifiedCoverage);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    let mut projection = node(603, O::SurfaceProjection);
    projection.parameters.insert(
        "direction".to_owned(),
        GraphParameterValue::FixedVec3([
            FIXED_ONE,
            DecisionScalar::from_bits(-65_536),
            DecisionScalar::from_bits(0),
        ]),
    );
    projection.parameters.insert(
        "maxDistance".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(64 * 65_536)),
    );
    projection
        .parameters
        .insert("provider".to_owned(), GraphParameterValue::U64(77));
    projection
        .parameters
        .insert("tags".to_owned(), GraphParameterValue::TagList(vec![5]));
    projection.parameters.insert(
        "materialTags".to_owned(),
        GraphParameterValue::TagList(vec![9]),
    );
    projection.spatial = NodeSpatialPolicy::Partitioned {
        level: 0,
        influence_radius: DecisionScalar::from_bits(64 * 65_536),
    };
    let mut value = node(604, O::FieldSample);
    value.parameters.insert(
        "channel".to_owned(),
        GraphParameterValue::FieldChannel(FieldChannel::Altitude),
    );
    value.parameters.insert(
        "derivative".to_owned(),
        GraphParameterValue::FieldDerivative(FieldDerivative::Value),
    );
    let mut gradient = node(605, O::FieldSample);
    gradient.parameters.insert(
        "channel".to_owned(),
        GraphParameterValue::FieldChannel(FieldChannel::Altitude),
    );
    gradient.parameters.insert(
        "derivative".to_owned(),
        GraphParameterValue::FieldDerivative(FieldDerivative::Gradient),
    );
    let mut hessian = node(606, O::FieldSample);
    hessian.parameters.insert(
        "channel".to_owned(),
        GraphParameterValue::FieldChannel(FieldChannel::Altitude),
    );
    hessian.parameters.insert(
        "derivative".to_owned(),
        GraphParameterValue::FieldDerivative(FieldDerivative::Hessian),
    );
    let mut field_diagnostic = node(607, O::DiagnosticOutput);
    field_diagnostic.parameters.insert(
        "label".to_owned(),
        GraphParameterValue::String("canonical-field".to_owned()),
    );
    let mut projection_diagnostic = node(608, O::DiagnosticOutput);
    projection_diagnostic.parameters.insert(
        "label".to_owned(),
        GraphParameterValue::String("canonical-projection".to_owned()),
    );
    let document = diagnostic_document(
        vec![
            region,
            coverage,
            projection,
            value,
            gradient,
            hessian,
            field_diagnostic,
            projection_diagnostic,
        ],
        vec![
            edge(601, "regions", 602, "regions"),
            edge(602, "candidates", 603, "candidates"),
            edge(602, "candidates", 604, "candidates"),
            edge(602, "candidates", 605, "candidates"),
            edge(602, "candidates", 606, "candidates"),
            edge(602, "candidates", 607, "candidates"),
            edge(604, "field", 607, "field"),
            edge(603, "candidates", 608, "candidates"),
        ],
        &[(607, "field-output"), (608, "projection-output")],
    );
    let provider = Arc::new(CanonicalSurfaceProvider::new());
    let provider_dyn: Arc<dyn SurfaceField> = provider.clone();
    let provider_hash =
        canonical_surface_provider_set_hash(std::slice::from_ref(&provider_dyn), 1).unwrap();
    let mut root = asset(document);
    root.policy.maximum_influence_radius = DecisionScalar::from_bits(64 * 65_536);
    let resolver = FixtureResolver {
        dependency_hashes: BTreeMap::from([(
            GraphDependencySource::SurfaceProvider(77),
            provider_hash,
        )]),
        available: vec![GraphDependencySource::SurfaceProvider(77)],
        ..FixtureResolver::default()
    };
    let graph =
        compile_biome_graph(&root, &[], &resolver, GraphCompileOptions::canonical()).unwrap();
    let mut input = evaluation_input(&graph);
    let output_bounds = input.output_bounds;
    input.set_hierarchical_region(611, output_bounds).unwrap();
    input.surface_provider_set_hash = provider_hash;
    input.surface_providers = vec![provider_dyn];
    assert_eq!(
        provider.availability(
            FieldChannel::Altitude,
            FieldDerivative::Hessian,
            input.read_bounds,
        ),
        FieldAvailability::Complete
    );
    assert_eq!(
        provider
            .authoritative_tiles(FieldChannel::Altitude, input.read_bounds)
            .as_slice(),
        &[SurfaceTileDescriptor {
            provider: SurfaceProviderId(77),
            revision: SurfaceRevision(3),
            bounds: input.read_bounds,
            dimensions: [2, 2, 2],
            value_quantum_bits: 1,
        }]
    );
    let descriptor = provider
        .authoritative_tiles(FieldChannel::Altitude, input.read_bounds)
        .pop()
        .unwrap();
    let cancellation = GraphCancellationToken::default();
    for (derivative, expected) in [
        (
            FieldDerivative::Value,
            QuantizedFieldTileValues::Scalar(vec![12_345; 8]),
        ),
        (
            FieldDerivative::Gradient,
            QuantizedFieldTileValues::Gradient(vec![[100, 200, 300]; 8]),
        ),
        (
            FieldDerivative::Hessian,
            QuantizedFieldTileValues::Hessian(vec![[400, 500, 600, 700, 800, 900]; 8]),
        ),
    ] {
        let tile = precompute_surface_field_tile(
            provider.as_ref(),
            descriptor,
            FieldChannel::Altitude,
            derivative,
            provider_hash,
            &cancellation,
        )
        .unwrap();
        assert_eq!(tile.values, expected);
    }
    let evaluator = BiomeGraphEvaluator::new(Arc::new(graph), 1).unwrap();
    let live_job = GraphEvaluationJobInputs {
        cells: vec![input],
        global_stages: Vec::new(),
    };
    let live_bound = evaluator.preflight(&live_job, &cancellation).unwrap();
    let mut result = evaluator
        .evaluate(live_job, &cancellation)
        .unwrap()
        .cells
        .pop()
        .unwrap();
    assert!(live_bound.input_tiles >= 3);
    let mut replay_input = evaluation_input(evaluator.graph());
    let replay_bounds = replay_input.output_bounds;
    replay_input
        .set_hierarchical_region(611, replay_bounds)
        .unwrap();
    replay_input.surface_provider_set_hash = provider_hash;
    replay_input.surface_projection_tiles = result.surface_projection_tiles.clone();
    replay_input.surface_field_query_tiles = result.surface_field_query_tiles.clone();
    let mut mismatched_projection = replay_input.clone();
    mismatched_projection.surface_projection_tiles[0].provider_set_hash = [0xA5; 32];
    let error = evaluator
        .preflight(
            &GraphEvaluationJobInputs {
                cells: vec![mismatched_projection],
                global_stages: Vec::new(),
            },
            &cancellation,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        saffron_vegetation::Error::GraphDocument { path, .. }
            if path == "evaluation.surfaceProjectionTiles"
    ));
    let mut mismatched_field_query = replay_input.clone();
    mismatched_field_query.surface_field_query_tiles[0].provider_set_hash = [0x5A; 32];
    let error = evaluator
        .preflight(
            &GraphEvaluationJobInputs {
                cells: vec![mismatched_field_query],
                global_stages: Vec::new(),
            },
            &cancellation,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        saffron_vegetation::Error::GraphDocument { path, .. }
            if path == "evaluation.surfaceFieldQueryTiles"
    ));
    let replay_job = GraphEvaluationJobInputs {
        cells: vec![replay_input],
        global_stages: Vec::new(),
    };
    let replay_bound = evaluator.preflight(&replay_job, &cancellation).unwrap();
    assert_eq!(replay_bound.input_tiles, 3);
    let replay = evaluator
        .evaluate(replay_job, &cancellation)
        .unwrap()
        .cells
        .pop()
        .unwrap();
    assert_eq!(
        replay.canonical_byte_len().unwrap(),
        replay.canonical_bytes().unwrap().len()
    );
    assert!(replay_bound.memory_bytes >= replay.canonical_bytes().unwrap().len() as u64);
    result = replay;
    assert_eq!(result.surface_projection_tiles.len(), 1);
    let projection_tile = &result.surface_projection_tiles[0];
    assert_eq!(projection_tile.node, 603);
    assert_eq!(projection_tile.provider_set_hash, provider_hash);
    assert_eq!(projection_tile.samples.len(), 4);
    for entry in &projection_tile.samples {
        let sample = entry.sample.as_ref().unwrap();
        let query = entry.query.global_ticks();
        assert_eq!(
            sample.position.global_ticks(),
            [query[0] + query[1], 0, query[2]]
        );
        let attachment = sample.attachment;
        assert_eq!(attachment.provider, SurfaceProviderId(77));
        assert_eq!(attachment.primitive, SurfacePrimitiveId(901));
        assert_eq!(attachment.revision, SurfaceRevision(3));
        assert_eq!(
            sample.tags,
            vec![
                WeightedSurfaceTag {
                    tag: SurfaceTagId(5),
                    weight: UnitInterval::from_bits(40_000)
                },
                WeightedSurfaceTag {
                    tag: SurfaceTagId(9),
                    weight: UnitInterval::from_bits(25_535)
                },
            ]
        );
    }
    assert_eq!(result.surface_field_query_tiles.len(), 1);
    for tile in &result.surface_field_query_tiles {
        assert_eq!(tile.channel, FieldChannel::Altitude);
        assert_eq!(tile.provider_set_hash, provider_hash);
        assert_eq!(tile.samples.len(), 4);
        for entry in &tile.samples {
            let expected = match tile.derivative {
                FieldDerivative::Value => QuantizedSurfaceFieldValue::Scalar(12_345),
                FieldDerivative::Gradient => QuantizedSurfaceFieldValue::Gradient([100, 200, 300]),
                FieldDerivative::Hessian => {
                    QuantizedSurfaceFieldValue::Hessian([400, 500, 600, 700, 800, 900])
                }
            };
            assert_eq!(entry.value, expected);
        }
    }
    let field_stream = result
        .diagnostics
        .streams
        .iter()
        .find(|stream| stream.label == "canonical-field")
        .unwrap();
    let projection_stream = result
        .diagnostics
        .streams
        .iter()
        .find(|stream| stream.label == "canonical-projection")
        .unwrap();
    let field_candidates = field_stream
        .candidates
        .as_ref()
        .unwrap()
        .iter()
        .map(|candidate| candidate.identity)
        .collect::<BTreeSet<_>>();
    let field_values = field_stream
        .field
        .as_ref()
        .unwrap()
        .iter()
        .map(|sample| sample.candidate)
        .collect::<BTreeSet<_>>();
    let projection_candidates = projection_stream
        .candidates
        .as_ref()
        .unwrap()
        .iter()
        .map(|candidate| candidate.identity)
        .collect::<BTreeSet<_>>();
    assert_eq!(field_candidates, field_values);
    assert_eq!(field_candidates, projection_candidates);
}
