use super::*;

pub(super) struct NoDependencies;

struct ReferenceCompute {
    pub(super) profile: GpuExecutionProfile,
    pub(super) qualifications: GpuQualificationRegistry,
}

struct CountingCompute {
    pub(super) profile: GpuExecutionProfile,
    pub(super) qualifications: GpuQualificationRegistry,
    pub(super) dispatches: Arc<AtomicUsize>,
}

#[derive(Clone)]
pub(super) struct TestSurfaceField {
    pub(super) descriptor: SurfaceProviderDescriptor,
    pub(super) failing_cell_x: Option<i64>,
    pub(super) project_hits: bool,
    pub(super) successful_samples: Arc<AtomicUsize>,
}

impl SurfaceField for TestSurfaceField {
    fn descriptor(&self) -> SurfaceProviderDescriptor {
        self.descriptor.clone()
    }

    fn field_channels(&self) -> Vec<FieldChannel> {
        vec![FieldChannel::Moisture]
    }

    fn raycast(&self, _query: &SurfaceRay) -> saffron_spatial::Result<Option<SurfaceHit>> {
        Ok(None)
    }

    fn project(&self, query: &SurfaceProjection) -> saffron_spatial::Result<Option<SurfaceHit>> {
        if !self.project_hits {
            return Ok(None);
        }
        self.successful_samples.fetch_add(1, Ordering::SeqCst);
        Ok(Some(SurfaceHit {
            provider: self.descriptor.id,
            position: query.origin,
            distance_m: 0.0,
            frame: saffron_spatial::SurfaceFrame::from_normal(saffron_geometry::glam::Vec3::Y)?,
            coordinates: saffron_spatial::SurfaceCoordinates::default(),
            attachment: Some(SurfaceAttachment::new(
                self.descriptor.id,
                saffron_spatial::SurfacePrimitiveId(0),
                [UnitInterval::ONE, UnitInterval::ZERO, UnitInterval::ZERO],
                self.descriptor.revision,
            )?),
            tags: Vec::new(),
            revision: self.descriptor.revision,
        }))
    }

    fn nearest(&self, _query: &SurfaceNearestQuery) -> saffron_spatial::Result<Option<SurfaceHit>> {
        Ok(None)
    }

    fn availability(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        _bounds: WorldBounds,
    ) -> FieldAvailability {
        if channel == FieldChannel::Moisture && derivative == FieldDerivative::Value {
            FieldAvailability::Complete
        } else {
            FieldAvailability::Unavailable
        }
    }

    fn estimated_samples(&self, _channel: FieldChannel, _bounds: WorldBounds) -> u64 {
        24
    }

    fn sample_scalar(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        position: WorldPosition,
    ) -> saffron_spatial::Result<FieldSample> {
        if self
            .failing_cell_x
            .is_some_and(|x| position.cell().coordinates()[0] == x)
        {
            return Err(saffron_spatial::Error::FieldUnavailable);
        }
        self.successful_samples.fetch_add(1, Ordering::SeqCst);
        Ok(FieldSample {
            channel,
            derivative,
            value: DecisionScalar::from_bits(65_535),
            revision: self.descriptor.revision,
        })
    }

    fn sample_vector(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        _position: WorldPosition,
    ) -> saffron_spatial::Result<VectorFieldSample> {
        Ok(VectorFieldSample {
            channel,
            derivative,
            value: DecisionVec3::default(),
            revision: self.descriptor.revision,
        })
    }

    fn sample_hessian(
        &self,
        channel: FieldChannel,
        _position: WorldPosition,
    ) -> saffron_spatial::Result<HessianFieldSample> {
        Ok(HessianFieldSample {
            channel,
            derivative: FieldDerivative::Hessian,
            value: DecisionHessian3::default(),
            revision: self.descriptor.revision,
        })
    }

    fn authoritative_tiles(
        &self,
        _channel: FieldChannel,
        _bounds: WorldBounds,
    ) -> Vec<SurfaceTileDescriptor> {
        Vec::new()
    }

    fn changes_since(&self, _revision: SurfaceRevision) -> Vec<SurfaceDirtyRegion> {
        Vec::new()
    }

    fn reproject_attachment(
        &self,
        _attachment: SurfaceAttachment,
    ) -> saffron_spatial::Result<Option<SurfaceHit>> {
        Ok(None)
    }
}

pub(super) struct SurfaceDependencies {
    pub(super) provider_hash: [u8; 32],
}

impl BiomeGraphResolver for SurfaceDependencies {
    fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
        Err(Error::GraphDocument {
            path: "test.resolver".to_owned(),
            reason: format!("unexpected module {}", id.value()),
        })
    }

    fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]> {
        match source {
            GraphDependencySource::Asset(Uuid(702)) => Ok([7; 32]),
            GraphDependencySource::Field(FieldChannel::Moisture) => Ok([2; 32]),
            GraphDependencySource::SurfaceProvider(77) => Ok(self.provider_hash),
            _ => Err(Error::GraphDocument {
                path: "test.resolver".to_owned(),
                reason: format!("unexpected dependency {source:?}"),
            }),
        }
    }

    fn available_dependencies(&self) -> Vec<GraphDependencySource> {
        vec![GraphDependencySource::SurfaceProvider(77)]
    }
}

impl GraphComputeExecutor for ReferenceCompute {
    fn profile(&self) -> &GpuExecutionProfile {
        &self.profile
    }

    fn qualifications(&self) -> &GpuQualificationRegistry {
        &self.qualifications
    }

    fn execute_program(
        &self,
        program: &GraphGpuProgram,
        invocations: &GraphGpuInvocationBatch,
        cancellation: &GraphCancellationToken,
        deadline: Instant,
    ) -> Result<Vec<crate::GraphGpuOutput>> {
        if cancellation.is_cancelled() {
            return Err(Error::GraphCancelled);
        }
        if Instant::now() >= deadline {
            return Err(Error::GraphLimit {
                resource: "time milliseconds",
                requested: 1,
                limit: 0,
            });
        }
        evaluate_gpu_program_reference(program, invocations)
    }
}

impl GraphComputeExecutor for CountingCompute {
    fn profile(&self) -> &GpuExecutionProfile {
        &self.profile
    }

    fn qualifications(&self) -> &GpuQualificationRegistry {
        &self.qualifications
    }

    fn execute_program(
        &self,
        program: &GraphGpuProgram,
        invocations: &GraphGpuInvocationBatch,
        cancellation: &GraphCancellationToken,
        deadline: Instant,
    ) -> Result<Vec<crate::GraphGpuOutput>> {
        if cancellation.is_cancelled() {
            return Err(Error::GraphCancelled);
        }
        if Instant::now() >= deadline {
            return Err(Error::GraphLimit {
                resource: "time milliseconds",
                requested: 1,
                limit: 0,
            });
        }
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        evaluate_gpu_program_reference(program, invocations)
    }
}

pub(super) fn reference_compute() -> Arc<dyn GraphComputeExecutor> {
    let profile = GpuExecutionProfile {
        name: "reference-test".to_owned(),
        vendor_id: 1,
        device_id: 2,
        driver_version: 3,
        api_version: 4,
        driver_id: 5,
        device_uuid: [6; 16],
        driver_uuid: [7; 16],
        molten_vk: false,
    };
    let qualifications = GpuQualificationRegistry::qualify(
        profile.clone(),
        GpuShaderArtifactIdentity {
            record_hash: [8; 32],
            compile_input_hash: [9; 32],
            spirv_hash: [10; 32],
            compiler_identity_hash: [11; 32],
        },
        evaluate_gpu_program_reference,
    )
    .unwrap();
    Arc::new(ReferenceCompute {
        profile,
        qualifications,
    })
}

pub(super) fn counting_compute(dispatches: Arc<AtomicUsize>) -> Arc<dyn GraphComputeExecutor> {
    let profile = GpuExecutionProfile {
        name: "counting-test".to_owned(),
        vendor_id: 1,
        device_id: 2,
        driver_version: 3,
        api_version: 4,
        driver_id: 5,
        device_uuid: [6; 16],
        driver_uuid: [7; 16],
        molten_vk: false,
    };
    let qualifications = GpuQualificationRegistry::qualify(
        profile.clone(),
        GpuShaderArtifactIdentity {
            record_hash: [8; 32],
            compile_input_hash: [9; 32],
            spirv_hash: [10; 32],
            compiler_identity_hash: [11; 32],
        },
        evaluate_gpu_program_reference,
    )
    .unwrap();
    Arc::new(CountingCompute {
        profile,
        qualifications,
        dispatches,
    })
}

impl BiomeGraphResolver for NoDependencies {
    fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
        Err(Error::GraphDocument {
            path: "test.resolver".to_owned(),
            reason: format!("unexpected module {}", id.value()),
        })
    }

    fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]> {
        match source {
            GraphDependencySource::Asset(Uuid(702)) => Ok([7; 32]),
            GraphDependencySource::MapLayer(42) => Ok([4; 32]),
            _ => Err(Error::GraphDocument {
                path: "test.resolver".to_owned(),
                reason: format!("unexpected dependency {source:?}"),
            }),
        }
    }
}

pub(super) fn node(guid: u128, operator: GraphOperator, level: u8) -> GraphNodeDefinition {
    GraphNodeDefinition {
        guid,
        version: BIOME_NODE_VERSION,
        semantic_revision: 1,
        operator,
        authority: GraphAuthority::Authoritative,
        spatial: NodeSpatialPolicy::Partitioned {
            level,
            influence_radius: DecisionScalar::from_bits(0),
        },
        dependencies: Vec::new(),
        seed_namespaces: BTreeMap::new(),
        parameters: BTreeMap::new(),
    }
}

pub(super) fn fixture_document(level: u8) -> BiomeGraphDocument {
    let region = node(1, GraphOperator::RegionInput, level);
    let mut coverage = node(2, GraphOperator::StratifiedCoverage, level);
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(24));
    coverage.parameters.insert(
        "jitter".to_owned(),
        GraphParameterValue::Unit(UnitInterval::from_bits(32_768)),
    );
    let species = node(3, GraphOperator::SpeciesInput, level);
    let mut output = node(4, GraphOperator::MacroOutput, level);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    let unrelated = node(5, GraphOperator::RegionInput, level);
    let mut noise = node(6, GraphOperator::Noise, level);
    noise.authority = GraphAuthority::EquivalentGpu;
    noise.seed_namespaces.insert("noise".to_owned(), 17);
    noise.parameters.insert(
        "frequency".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(32_768)),
    );
    noise.parameters.insert(
        "amplitude".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
    );
    noise
        .parameters
        .insert("channel".to_owned(), GraphParameterValue::U32(3));
    let communities = node(7, GraphOperator::CommunityInput, level);
    let mut blend = node(8, GraphOperator::CommunityBlend, level);
    blend.seed_namespaces.insert("community".to_owned(), 19);
    let mut competition = node(9, GraphOperator::Competition, level);
    competition.spatial = NodeSpatialPolicy::Partitioned {
        level,
        influence_radius: DecisionScalar::from_bits(4 * 65_536),
    };
    competition.parameters.insert(
        "crownWeight".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ONE),
    );
    competition.parameters.insert(
        "rootWeight".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ONE),
    );
    BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 1004,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 4,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![
            output,
            unrelated,
            noise,
            competition,
            blend,
            communities,
            species,
            coverage,
            region,
        ],
        edges: vec![
            GraphEdge {
                from_node: 1,
                from_pin: "regions".to_owned(),
                to_node: 2,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: 9,
                from_pin: "candidates".to_owned(),
                to_node: 4,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 3,
                from_pin: "species".to_owned(),
                to_node: 4,
                to_pin: "species".to_owned(),
            },
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 6,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 8,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 7,
                from_pin: "communities".to_owned(),
                to_node: 8,
                to_pin: "communities".to_owned(),
            },
            GraphEdge {
                from_node: 8,
                from_pin: "candidates".to_owned(),
                to_node: 9,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 7,
                from_pin: "communities".to_owned(),
                to_node: 9,
                to_pin: "communities".to_owned(),
            },
        ],
    }
}
