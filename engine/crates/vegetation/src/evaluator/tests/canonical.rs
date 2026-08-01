use super::*;

const CORDIC_SWEEP_DIGEST: [u8; 32] = [
    0x40, 0xae, 0x9f, 0xe3, 0x6c, 0xea, 0x4a, 0x1c, 0x2c, 0x9a, 0xfd, 0x8e, 0x8a, 0xd4, 0xba, 0x14,
    0x1c, 0xb9, 0xde, 0x20, 0x9e, 0xb2, 0x35, 0x37, 0xc3, 0x52, 0x2a, 0x02, 0xd9, 0x26, 0x1d, 0x1d,
];

#[test]
fn stable_ordinal_streaming_hash_is_pinned() {
    assert_eq!(
        stable_ordinal(&[b"a", b"bc", b""]).unwrap(),
        0xc5db_f3ec_4ecc_a82f
    );
}

/// The Q30 CORDIC is the second integer trigonometry routine that reaches cooked bytes, through
/// the orientation column and the random-direction path. Its whole sweep is pinned by digest so a
/// different target cannot answer one bit differently and still cook.
///
/// The stride is an odd multiplier, not a power of two: the random-direction caller feeds a whole
/// 32-bit Philox lane, and it is the angle's low bits that decide the late rotations. A
/// power-of-two stride holds them at zero and leaves the tail of the table unexercised.
#[test]
fn cordic_sweep_is_byte_pinned() {
    assert_eq!(cordic_sin_cos(0), (1_073_741_822, -18_890));

    let mut bytes = Vec::new();
    for step in 0..8_192_u32 {
        let (cosine, sine) = cordic_sin_cos(step.wrapping_mul(0x9E37_79B9));
        bytes.extend_from_slice(&cosine.to_be_bytes());
        bytes.extend_from_slice(&sine.to_be_bytes());
    }
    assert_eq!(bytes.len(), 131_072);
    assert_eq!(sha256(&bytes), CORDIC_SWEEP_DIGEST);
}

/// The orientation column a cooked point carries is the CORDIC's output after quantization, so the
/// swept quaternion lanes are pinned as the bytes an artifact actually stores.
#[test]
fn cooked_yaw_orientation_sweep_is_byte_pinned() {
    let mut bytes = Vec::new();
    let mut yaw = 0_u32;
    while yaw <= u32::from(u16::MAX) {
        let orientation = yaw_orientation(UnitInterval::from_bits(yaw as u16)).unwrap();
        for lane in orientation.bits() {
            bytes.extend_from_slice(&lane.to_be_bytes());
        }
        yaw += 251;
    }
    assert_eq!(bytes.len(), 2_096);
    assert_eq!(
        sha256(&bytes),
        [
            0x12, 0x3a, 0x49, 0xb0, 0xa7, 0xe4, 0xa5, 0xcf, 0xaf, 0x22, 0x60, 0x52, 0xf5, 0xa0,
            0x80, 0xa4, 0xe8, 0xa3, 0x86, 0xb2, 0xad, 0x83, 0xbf, 0xbd, 0x88, 0x41, 0xa8, 0xe0,
            0x3c, 0xba, 0x01, 0x23,
        ]
    );
}

#[test]
fn diagnostic_merge_structurally_bounds_small_stream_maps() {
    let identity = |ordinal| CandidateIdentity {
        node: 1,
        node_address: 2,
        node_semantic_revision: 1,
        ordinal,
        ancestor: 0,
    };
    let stream = |ordinal| NamedDiagnosticStream {
        node: GraphNodeAddress {
            module_path: vec![7],
            node: 1,
        },
        label: "x".to_owned(),
        scope: DiagnosticStreamScope::GlobalSnapshot,
        candidates: Some(vec![DiagnosticCandidateSample {
            identity: identity(ordinal),
            owner: WorldCellKey::base(0, 0, 0),
            position: WorldPosition::origin(),
            family: Some(Uuid(702)),
            variation: 0,
            priority: DecisionScalar::from_bits(1),
            ecology_tick: 0,
        }]),
        field: Some(vec![DiagnosticScalarSample {
            candidate: identity(ordinal),
            value: DecisionScalar::from_bits(1),
        }]),
        rejected: Vec::new(),
    };
    let left_stream = stream(1);
    let right_stream = stream(2);
    let left = GraphValue::Diagnostics(vec![left_stream.clone()]);
    let right = GraphValue::Diagnostics(vec![right_stream.clone()]);
    let runtime_scratch = graph_value_merge_scratch_bytes(&left, &right).unwrap();
    let previous_payload_heuristic = left
        .requested_memory_bytes()
        .unwrap()
        .checked_add(right.requested_memory_bytes().unwrap())
        .and_then(|bytes| bytes.checked_mul(3))
        .unwrap();
    assert!(runtime_scratch > previous_payload_heuristic);

    let symbolic = |stream: &NamedDiagnosticStream| SymbolicValueBound {
        domain: Some(GraphDomain::Diagnostics),
        items: 1,
        bytes: diagnostic_stream_memory(stream).unwrap(),
        diagnostic_candidates: 1,
        diagnostic_fields: 1,
        diagnostic_rejected: 0,
        diagnostic_module_path_items: 1,
        diagnostic_label_bytes: 1,
    };
    assert_eq!(
        symbolic_global_merge_scratch_bytes(symbolic(&left_stream), symbolic(&right_stream))
            .unwrap(),
        runtime_scratch
    );

    let graph = compile_fixture(0);
    let mut destination = Some(left);
    merge_graph_value(&mut destination, right, &graph.root.nodes[0]).unwrap();
    let Some(GraphValue::Diagnostics(streams)) = destination else {
        panic!("diagnostic merge returned another domain");
    };
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].candidates.as_ref().unwrap().len(), 2);
    assert_eq!(streams[0].field.as_ref().unwrap().len(), 2);
}

#[test]
fn canonical_validation_rejects_duplicate_result_and_provider_query_entries() {
    let position = WorldPosition::origin();
    let projection = QuantizedSurfaceProjectionTile {
        node: 1,
        node_semantic_revision: 1,
        samples: vec![
            QuantizedSurfaceProjectionEntry {
                query: position,
                sample: None,
            },
            QuantizedSurfaceProjectionEntry {
                query: position,
                sample: None,
            },
        ],
        provider_set_hash: [1; 32],
    };
    assert!(matches!(
        projection.validate(),
        Err(Error::GraphDocument { path, .. })
            if path == "evaluation.surfaceProjectionTiles"
    ));
    let later_position = WorldPosition::from_global_ticks([1, 0, 0]).unwrap();
    let reversed_projection = QuantizedSurfaceProjectionTile {
        node: 1,
        node_semantic_revision: 1,
        samples: vec![
            QuantizedSurfaceProjectionEntry {
                query: later_position,
                sample: None,
            },
            QuantizedSurfaceProjectionEntry {
                query: position,
                sample: None,
            },
        ],
        provider_set_hash: [1; 32],
    };
    assert!(matches!(
        reversed_projection.validate(),
        Err(Error::GraphDocument { path, .. })
            if path == "evaluation.surfaceProjectionTiles"
    ));

    let identity = CandidateIdentity {
        node: 1,
        node_address: 1,
        node_semantic_revision: 1,
        ordinal: 1,
        ancestor: 0,
    };
    let field = QuantizedSurfaceFieldQueryTile {
        node: 1,
        node_semantic_revision: 1,
        channel: FieldChannel::Moisture,
        derivative: FieldDerivative::Value,
        samples: vec![
            QuantizedSurfaceFieldQueryEntry {
                candidate: identity,
                query: position,
                value: QuantizedSurfaceFieldValue::Scalar(1),
            },
            QuantizedSurfaceFieldQueryEntry {
                candidate: identity,
                query: position,
                value: QuantizedSurfaceFieldValue::Scalar(1),
            },
        ],
        provider_set_hash: [1; 32],
    };
    assert!(matches!(
        validate_field_query_tile_with_guard(&field, None),
        Err(Error::GraphDocument { path, .. })
            if path == "evaluation.surfaceFieldQueryTiles"
    ));
    let mut later_identity = identity;
    later_identity.ordinal = 2;
    let reversed_field = QuantizedSurfaceFieldQueryTile {
        samples: vec![
            QuantizedSurfaceFieldQueryEntry {
                candidate: later_identity,
                query: position,
                value: QuantizedSurfaceFieldValue::Scalar(1),
            },
            QuantizedSurfaceFieldQueryEntry {
                candidate: identity,
                query: position,
                value: QuantizedSurfaceFieldValue::Scalar(1),
            },
        ],
        ..field
    };
    assert!(matches!(
        validate_field_query_tile_with_guard(&reversed_field, None),
        Err(Error::GraphDocument { path, .. })
            if path == "evaluation.surfaceFieldQueryTiles"
    ));

    let graph = Arc::new(compile_document(micro_document()));
    let mut result = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .evaluate(
            job(vec![input(
                WorldCellKey::base(0, 0, 0),
                graph.required_halo(0),
            )]),
            &GraphCancellationToken::default(),
        )
        .unwrap()
        .cells
        .pop()
        .unwrap();
    let mut duplicate_result = result.clone();
    duplicate_result
        .micro_fields
        .push(duplicate_result.micro_fields[0].clone());
    assert!(matches!(
        duplicate_result.canonical_bytes(),
        Err(Error::GraphDocument { path, .. }) if path == "evaluation.canonicalEncoding"
    ));

    let mut later_tile = result.micro_fields[0].clone();
    later_tile.cell = WorldCellKey::base(1, 0, 0);
    result.micro_fields.push(later_tile);
    assert!(result.canonical_bytes().is_ok());
    let sections = result.cell_artifact_sections().unwrap();
    assert_eq!(
        sections
            .iter()
            .map(|section| section.kind)
            .collect::<Vec<_>>(),
        vec![
            VegetationCellSectionKind::MacroPoints,
            VegetationCellSectionKind::MicroFields,
            VegetationCellSectionKind::Provenance,
            VegetationCellSectionKind::RejectionDiagnostics,
            VegetationCellSectionKind::SurfaceAttachments,
            VegetationCellSectionKind::SurfaceDependencies,
            VegetationCellSectionKind::RenderReferences,
            VegetationCellSectionKind::RenderBounds,
            VegetationCellSectionKind::CollisionInputs,
            VegetationCellSectionKind::NavigationContributions,
            VegetationCellSectionKind::EcologyBoundary,
            VegetationCellSectionKind::EcologyCheckpoint,
        ]
    );
    assert_eq!(
        sections[0].bytes,
        result.macro_points.canonical_bytes().unwrap()
    );
    assert!(sections[1].bytes.starts_with(b"SVEGMIC2"));
    assert!(sections[6].bytes.starts_with(b"SVEGRRF1"));
    assert!(sections[7].bytes.starts_with(b"SVEGRBD1"));
    assert!(sections[8].bytes.starts_with(b"SVEGCOL1"));
    assert!(sections[9].bytes.starts_with(b"SVEGNAV1"));
    assert!(sections[10].bytes.starts_with(b"SVEGEBD1"));
    assert!(sections[11].bytes.starts_with(b"SVEGECP1"));
    result.micro_fields.reverse();
    assert!(matches!(
        result.canonical_bytes(),
        Err(Error::GraphDocument { path, .. }) if path == "evaluation.canonicalEncoding"
    ));
}

#[test]
fn every_cell_facet_strictly_decodes_the_canonical_encoder_output() {
    let graph = Arc::new(compile_fixture(0));
    let mut result = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .evaluate(
            job(vec![input(
                WorldCellKey::base(0, 0, 0),
                graph.required_halo(0),
            )]),
            &GraphCancellationToken::default(),
        )
        .unwrap()
        .cells
        .pop()
        .unwrap();
    let micro_graph = Arc::new(compile_document(micro_document()));
    let micro_result = BiomeGraphEvaluator::new(Arc::clone(&micro_graph), 1)
        .unwrap()
        .evaluate(
            job(vec![input(
                WorldCellKey::base(0, 0, 0),
                micro_graph.required_halo(0),
            )]),
            &GraphCancellationToken::default(),
        )
        .unwrap()
        .cells
        .pop()
        .unwrap();
    result.micro_fields = micro_result.micro_fields;
    let query = WorldPosition::origin();
    let candidate = CandidateIdentity {
        node: 1,
        node_address: 1,
        node_semantic_revision: 1,
        ordinal: 1,
        ancestor: 0,
    };
    result.surface_projection_tiles = vec![QuantizedSurfaceProjectionTile {
        node: 1,
        node_semantic_revision: 1,
        samples: vec![QuantizedSurfaceProjectionEntry {
            query,
            sample: Some(QuantizedSurfaceProjectionSample {
                position: query,
                attachment: SurfaceAttachment::new(
                    SurfaceProviderId(7),
                    saffron_spatial::SurfacePrimitiveId(1),
                    [UnitInterval::ONE, UnitInterval::ZERO, UnitInterval::ZERO],
                    SurfaceRevision(3),
                )
                .unwrap(),
                normal: [
                    SignedUnit::from_bits(0).unwrap(),
                    SignedUnit::from_bits(i16::MAX).unwrap(),
                    SignedUnit::from_bits(0).unwrap(),
                ],
                projection: [DecisionScalar::from_bits(3); 3],
                tags: vec![WeightedSurfaceTag {
                    tag: saffron_spatial::SurfaceTagId(5),
                    weight: UnitInterval::ONE,
                }],
            }),
        }],
        provider_set_hash: [1; 32],
    }];
    result.surface_field_query_tiles = vec![QuantizedSurfaceFieldQueryTile {
        node: 2,
        node_semantic_revision: 1,
        channel: FieldChannel::Moisture,
        derivative: FieldDerivative::Value,
        samples: vec![QuantizedSurfaceFieldQueryEntry {
            candidate,
            query,
            value: QuantizedSurfaceFieldValue::Scalar(7),
        }],
        provider_set_hash: [1; 32],
    }];
    result.diagnostics.streams = vec![NamedDiagnosticStream {
        node: GraphNodeAddress {
            module_path: vec![3],
            node: 4,
        },
        label: "accepted".to_owned(),
        scope: DiagnosticStreamScope::CandidateLineage(CandidateLineage(5)),
        candidates: Some(vec![DiagnosticCandidateSample {
            identity: candidate,
            owner: result.cell,
            position: query,
            family: Some(Uuid(702)),
            variation: 1,
            priority: DecisionScalar::from_bits(7),
            ecology_tick: 9,
        }]),
        field: Some(vec![DiagnosticScalarSample {
            candidate,
            value: DecisionScalar::from_bits(11),
        }]),
        rejected: Vec::new(),
    }];

    let sections = result.cell_artifact_sections().unwrap();
    assert_eq!(sections.len(), VegetationCellSectionKind::ALL.len());
    for section in &sections {
        let decoded = decode_vegetation_cell_facet(section.kind, &section.bytes).unwrap();
        match decoded {
            VegetationCellFacet::MacroPoints(points) => {
                assert_eq!(*points, result.macro_points);
                assert_eq!(points.point(0).unwrap().id, points.ids[0]);
            }
            VegetationCellFacet::MicroFields(tiles) => {
                assert_eq!(tiles, result.micro_fields);
            }
            VegetationCellFacet::Provenance(table) => {
                assert_eq!(table, result.provenance);
            }
            VegetationCellFacet::RejectionDiagnostics(diagnostics) => {
                assert_eq!(
                    diagnostics.candidate_count,
                    result.diagnostics.candidate_count
                );
                assert_eq!(
                    diagnostics.accepted_count,
                    result.diagnostics.accepted_count
                );
                assert_eq!(diagnostics.rejected, result.diagnostics.rejected);
                assert_eq!(diagnostics.streams, result.diagnostics.streams);
            }
            VegetationCellFacet::SurfaceAttachments(tiles) => {
                assert_eq!(tiles, result.surface_projection_tiles);
            }
            VegetationCellFacet::SurfaceDependencies(tiles) => {
                assert_eq!(tiles, result.surface_field_query_tiles);
            }
            VegetationCellFacet::RenderReferences(rows) => {
                assert_eq!(rows.len(), result.macro_points.ids.len());
                assert_eq!(rows[0].plant, result.macro_points.ids[0]);
                assert_eq!(rows[0].family, result.macro_points.families[0]);
            }
            VegetationCellFacet::RenderBounds(rows) => {
                assert_eq!(rows.len(), result.macro_points.ids.len());
                assert_eq!(rows[0].bounds, result.macro_points.bounds[0]);
            }
            VegetationCellFacet::CollisionInputs(rows) => {
                assert_eq!(rows.len(), result.macro_points.ids.len());
                assert_eq!(rows[0].position, result.macro_points.positions[0]);
            }
            VegetationCellFacet::NavigationContributions(rows) => {
                assert_eq!(rows.len(), result.macro_points.ids.len());
                assert_eq!(rows[0].bounds, result.macro_points.bounds[0]);
            }
            VegetationCellFacet::EcologyBoundary(rows) => {
                assert!(rows.len() <= result.macro_points.ids.len());
                for row in rows {
                    assert!(result.macro_points.ids.contains(&row.plant));
                }
            }
            VegetationCellFacet::EcologyCheckpoint(rows) => {
                assert_eq!(rows.len(), result.macro_points.ids.len());
                assert_eq!(rows[0].ecology_tick, result.macro_points.ecology_ticks[0]);
            }
        }

        let mut corrupt = section.bytes.clone();
        corrupt[0] ^= 0xff;
        assert!(matches!(
            decode_vegetation_cell_facet(section.kind, &corrupt),
            Err(Error::ArtifactFormat { field, .. }) if field == "magic"
        ));
        let mut truncated = section.bytes.clone();
        truncated.pop();
        assert!(decode_vegetation_cell_facet(section.kind, &truncated).is_err());
        let mut trailing = section.bytes.clone();
        trailing.push(0);
        assert!(matches!(
            decode_vegetation_cell_facet(section.kind, &trailing),
            Err(Error::ArtifactFormat { field, .. }) if field == "trailingBytes"
        ));
    }

    let dependencies = sections
        .iter()
        .find(|section| section.kind == VegetationCellSectionKind::SurfaceDependencies)
        .unwrap();
    let mut wrong_domain = dependencies.bytes.clone();
    wrong_domain[37] = 1;
    assert!(matches!(
        decode_vegetation_cell_facet(dependencies.kind, &wrong_domain),
        Err(Error::ArtifactFormat { field, .. }) if field == "samples.valueType"
    ));
}

#[test]
fn rejection_facet_summary_validates_the_complete_payload() {
    let candidate = |ordinal| CandidateIdentity {
        node: 1,
        node_address: 2,
        node_semantic_revision: 1,
        ordinal,
        ancestor: 0,
    };
    let result = GraphEvaluationResult {
        cell: WorldCellKey::base(-1, 0, 2),
        macro_points: PlantPointColumns::default(),
        micro_fields: Vec::new(),
        surface_projection_tiles: Vec::new(),
        surface_field_query_tiles: Vec::new(),
        ancestor_references: Vec::new(),
        provenance: ProvenanceTable::default(),
        diagnostics: GraphEvaluationDiagnostics {
            rejected: vec![
                RejectedCandidate {
                    candidate: candidate(1),
                    position: WorldPosition::origin(),
                    reason: CandidateRejectionReason::Threshold,
                    provenance: ProvenanceHandle(0),
                },
                RejectedCandidate {
                    candidate: candidate(2),
                    position: WorldPosition::origin(),
                    reason: CandidateRejectionReason::NoSpecies,
                    provenance: ProvenanceHandle(0),
                },
            ],
            candidate_count: 2,
            accepted_count: 0,
            ..GraphEvaluationDiagnostics::default()
        },
    };
    let mut bytes = result
        .cell_artifact_sections()
        .unwrap()
        .into_iter()
        .find(|section| section.kind == VegetationCellSectionKind::RejectionDiagnostics)
        .unwrap()
        .bytes;
    assert_eq!(
        vegetation_rejection_totals(&bytes).unwrap(),
        vec![
            (CandidateRejectionReason::Threshold, 1),
            (CandidateRejectionReason::NoSpecies, 1),
        ]
    );
    bytes.push(0);
    assert!(matches!(
        vegetation_rejection_totals(&bytes),
        Err(Error::ArtifactFormat { field, .. }) if field == "trailingBytes"
    ));
}

#[test]
fn micro_density_texels_linearize_with_z_fastest() {
    // Non-square dimensions: a square grid hides a transposed linearization behind a symmetric
    // relabel, so only an asymmetric grid pins the order the reconstruction decodes.
    const DIMENSIONS: [u32; 3] = [4, 1, 2];
    let ticks = |meters: i128| meters * i128::from(LOCAL_TICKS_PER_METER);
    // Texel (3, 0, 0) of the base cell: 16 m of X per texel against 32 m of Z.
    let authored = WorldPosition::from_global_ticks([ticks(52), 0, ticks(8)]).unwrap();

    let graph = Arc::new(compile_document(explicit_micro_document(DIMENSIONS)));
    let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    inputs.anchors = vec![explicit_point(0, 42, authored)];
    let result = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
        .unwrap()
        .cells
        .pop()
        .unwrap();

    let tile = &result.micro_fields[0];
    assert_eq!(tile.dimensions, DIMENSIONS);
    let occupied = tile
        .density
        .iter()
        .enumerate()
        .filter(|(_, density)| **density > 0)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(occupied.len(), 1);

    // The reconstruction's decode, verbatim: Z runs fastest, then Y, then X. A blade must
    // rebuild in the texel its candidate was authored in.
    let index = occupied[0];
    let decoded = [
        index / (DIMENSIONS[2] as usize * DIMENSIONS[1] as usize),
        (index / DIMENSIONS[2] as usize) % DIMENSIONS[1] as usize,
        index % DIMENSIONS[2] as usize,
    ];
    assert_eq!(decoded, [3, 0, 0]);

    let cell = WorldCellKey::base(0, 0, 0).bounds();
    let point = authored.global_ticks();
    for (axis, coordinate) in decoded.into_iter().enumerate() {
        let edge = (cell.max_ticks_exclusive()[axis] - cell.min_ticks()[axis])
            / i128::from(DIMENSIONS[axis]);
        let base = cell.min_ticks()[axis] + coordinate as i128 * edge;
        assert!(
            point[axis] >= base && point[axis] < base + edge,
            "axis {axis}: {} is outside texel {coordinate}",
            point[axis]
        );
    }
}
