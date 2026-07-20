//! Wire-contract tests: the frozen JSON spellings the editor and the contract test depend on.
//! These assert the byte-exact enum spellings, the unknown-value error, the `Option`
//! missing-key behavior, and a representative struct round-trip.

use saffron_protocol::*;

/// Every variant of every enum serializes to its exact kebab-case wire spelling, and that
/// string deserializes back to the same variant. A drift here silently breaks the editor's
/// typed client.
#[test]
fn enum_wire_spellings_match_cpp_table() {
    macro_rules! check {
        ($variant:expr, $wire:literal) => {{
            let json = serde_json::to_string(&$variant).unwrap();
            assert_eq!(json, concat!("\"", $wire, "\""), "emit spelling drifted");
            let back = serde_json::from_str(&json).unwrap();
            assert_eq!($variant, back, "round-trip drifted");
        }};
    }

    check!(AddEntityPreset::Empty, "empty");
    check!(AddEntityPreset::Cube, "cube");
    check!(AddEntityPreset::Plane, "plane");
    check!(AddEntityPreset::Sphere, "sphere");
    check!(AddEntityPreset::PointLight, "point-light");
    check!(AddEntityPreset::SpotLight, "spot-light");
    check!(AddEntityPreset::DirectionalLight, "directional-light");
    check!(AddEntityPreset::Camera, "camera");
    check!(AddEntityPreset::ReflectionProbe, "reflection-probe");

    check!(PickKind::Billboard, "billboard");
    check!(PickKind::Mesh, "mesh");

    check!(GizmoOpDto::Translate, "translate");
    check!(GizmoOpDto::Rotate, "rotate");
    check!(GizmoOpDto::Scale, "scale");

    check!(GizmoSpaceDto::World, "world");
    check!(GizmoSpaceDto::Local, "local");

    check!(GizmoPointerPhase::Hover, "hover");
    check!(GizmoPointerPhase::Begin, "begin");
    check!(GizmoPointerPhase::Drag, "drag");
    check!(GizmoPointerPhase::End, "end");

    check!(AaModeDto::Off, "off");
    check!(AaModeDto::Fxaa, "fxaa");
    check!(AaModeDto::Taa, "taa");
    check!(AaModeDto::Msaa2, "msaa2");
    check!(AaModeDto::Msaa4, "msaa4");
    check!(AaModeDto::Msaa8, "msaa8");

    check!(GiModeDto::Off, "off");
    check!(GiModeDto::Ddgi, "ddgi");

    check!(ViewModeDto::Lit, "lit");
    check!(ViewModeDto::Wireframe, "wireframe");
    check!(ViewModeDto::Albedo, "albedo");
    check!(ViewModeDto::Normal, "normal");
    check!(ViewModeDto::Roughness, "roughness");
    check!(ViewModeDto::Metallic, "metallic");
    check!(ViewModeDto::Emissive, "emissive");
    check!(ViewModeDto::Fog, "fog");
    check!(ViewModeDto::CloudDensity, "cloud-density");

    check!(AssetSlotDto::Mesh, "mesh");
    check!(AssetSlotDto::Albedo, "albedo");
    check!(AssetSlotDto::MetallicRoughness, "metallic-roughness");
    check!(AssetSlotDto::Normal, "normal");
    check!(AssetSlotDto::Occlusion, "occlusion");
    check!(AssetSlotDto::Emissive, "emissive");
    check!(AssetSlotDto::Height, "height");

    check!(ScreenshotTargetDto::Viewport, "viewport");
    check!(ScreenshotTargetDto::Window, "window");

    check!(AssetTypeDto::Mesh, "mesh");
    check!(AssetTypeDto::Texture, "texture");
    check!(AssetTypeDto::Other, "other");
    check!(AssetTypeDto::Animation, "animation");
    check!(AssetTypeDto::Material, "material");
    check!(AssetTypeDto::Model, "model");
    check!(AssetTypeDto::Lut, "lut");
    check!(AssetTypeDto::Plant, "plant");
    check!(AssetTypeDto::Biome, "biome");
    check!(AssetTypeDto::VegetationMap, "vegetation-map");

    check!(ProfilerModeDto::Off, "off");
    check!(ProfilerModeDto::Timestamps, "timestamps");
    check!(ProfilerModeDto::PipelineStats, "pipeline-stats");

    check!(ProfileLaneDto::Cpu, "cpu");
    check!(ProfileLaneDto::Gpu, "gpu");

    check!(CaptureModeDto::Single, "single");
    check!(CaptureModeDto::Frames, "frames");
    check!(CaptureModeDto::Rolling, "rolling");

    check!(CaptureStateDto::Idle, "idle");
    check!(CaptureStateDto::Arming, "arming");
    check!(CaptureStateDto::Recording, "recording");
    check!(CaptureStateDto::Ready, "ready");

    check!(AlarmSeverityDto::Info, "info");
    check!(AlarmSeverityDto::Warning, "warning");
    check!(AlarmSeverityDto::Critical, "critical");

    check!(AlarmStateDto::Firing, "firing");
    check!(AlarmStateDto::Resolved, "resolved");

    check!(
        VegetationCandidateRejectionReasonDto::SurfaceMiss,
        "surface-miss"
    );
    check!(
        VegetationCandidateRejectionReasonDto::Threshold,
        "threshold"
    );
    check!(
        VegetationCandidateRejectionReasonDto::WeightedElimination,
        "weighted-elimination"
    );
    check!(
        VegetationCandidateRejectionReasonDto::PriorityExclusion,
        "priority-exclusion"
    );
    check!(
        VegetationCandidateRejectionReasonDto::Competition,
        "competition"
    );
    check!(
        VegetationCandidateRejectionReasonDto::ForeignOwner,
        "foreign-owner"
    );
    check!(
        VegetationCandidateRejectionReasonDto::NoSpecies,
        "no-species"
    );
}

/// An unknown enum value is a `Deserialize` error, not a silent default.
#[test]
fn unknown_enum_value_is_an_error() {
    assert!(serde_json::from_str::<AaModeDto>("\"bogus\"").is_err());
    assert!(serde_json::from_str::<GizmoOpDto>("\"slide\"").is_err());
    assert!(serde_json::from_str::<AssetSlotDto>("\"roughness\"").is_err());
    assert!(
        serde_json::from_str::<VegetationCandidateRejectionReasonDto>("\"random-rejection\"")
            .is_err()
    );
}

#[test]
fn vegetation_estimates_and_diagnostics_use_complete_camel_case_fields() {
    let estimate = VegetationGraphEstimateDto {
        candidates: "10".to_owned(),
        accepted: "4".to_owned(),
        micro_samples: "128".to_owned(),
        memory_bytes: "2048".to_owned(),
        transfer_bytes: "512".to_owned(),
    };
    let estimate_json = serde_json::to_value(&estimate).unwrap();
    assert_eq!(estimate_json["microSamples"], serde_json::json!("128"));
    assert_eq!(estimate_json["transferBytes"], serde_json::json!("512"));
    assert_eq!(
        serde_json::from_value::<VegetationGraphEstimateDto>(estimate_json).unwrap(),
        estimate
    );

    let limits = VegetationGraphLimitsDto {
        workers: 8,
        output_cells: "64".to_owned(),
        global_stage_tiles: "128".to_owned(),
        input_tiles: "512".to_owned(),
        candidates: "4096".to_owned(),
        macro_points: "2048".to_owned(),
        micro_samples: "8192".to_owned(),
        memory_bytes: "1048576".to_owned(),
        transfer_bytes: "524288".to_owned(),
        module_depth: 8,
        time_ms: "30000".to_owned(),
    };
    let limits_json = serde_json::to_value(&limits).unwrap();
    assert_eq!(limits_json["globalStageTiles"], serde_json::json!("128"));
    assert_eq!(
        serde_json::from_value::<VegetationGraphLimitsDto>(limits_json).unwrap(),
        limits
    );

    let diagnostic_source = VegetationDiagnosticResultSourceDto::Cell {
        cell: WorldCellDto {
            coordinates: ["0".to_owned(), "1".to_owned(), "2".to_owned()],
            level: 0,
        },
    };
    let candidate = VegetationCandidateIdentityDto {
        node: VegetationGuid("00000000000000000000000000000003".to_owned()),
        node_address: VegetationGuid("00000000000000000000000000000004".to_owned()),
        node_semantic_revision: 1,
        ordinal: "5".to_owned(),
        ancestor: "6".to_owned(),
    };
    let summary = VegetationEvaluationSummaryDto {
        cells: "2".to_owned(),
        global_stages: "3".to_owned(),
        global_resident_bytes: "4096".to_owned(),
        candidates: "10".to_owned(),
        accepted: "4".to_owned(),
        micro_tiles: "1".to_owned(),
        rejected: "6".to_owned(),
        canonical_hash: "00".repeat(32),
        nodes: Vec::new(),
        gpu_groups: vec![VegetationGpuGroupEvaluationDiagnosticDto {
            nodes: vec![VegetationGraphNodeAddressDto {
                module_path: vec![VegetationGuid(
                    "00000000000000000000000000000001".to_owned(),
                )],
                node: VegetationGuid("00000000000000000000000000000002".to_owned()),
            }],
            invocation_count: "10".to_owned(),
            output_bytes: "256".to_owned(),
            transfer_bytes: "512".to_owned(),
            elapsed_micros: "12".to_owned(),
        }],
        streams: vec![VegetationNamedDiagnosticStreamDto {
            node: VegetationGraphNodeAddressDto {
                module_path: Vec::new(),
                node: VegetationGuid("00000000000000000000000000000002".to_owned()),
            },
            label: "density audit".to_owned(),
            scope: VegetationDiagnosticStreamScopeDto::GlobalSnapshot,
            candidate_samples: None,
            scalar_samples: Some(vec![VegetationDiagnosticScalarSampleDto {
                source: diagnostic_source.clone(),
                candidate: candidate.clone(),
                value_bits: 65536,
            }]),
            rejected: vec![VegetationDiagnosticRejectionDto {
                candidate,
                reason: VegetationCandidateRejectionReasonDto::Threshold,
                provenance: VegetationDiagnosticProvenanceIdDto {
                    source: diagnostic_source,
                    handle: 7,
                },
            }],
        }],
    };
    let summary_json = serde_json::to_value(&summary).unwrap();
    assert_eq!(summary_json["globalStages"], serde_json::json!("3"));
    assert_eq!(
        summary_json["globalResidentBytes"],
        serde_json::json!("4096")
    );
    assert_eq!(summary_json["gpuGroups"][0]["invocationCount"], "10");
    assert!(summary_json["streams"][0].get("candidateSamples").is_none());
    assert_eq!(
        summary_json["streams"][0]["scope"]["kind"],
        "global-snapshot"
    );
    assert_eq!(
        summary_json["streams"][0]["scalarSamples"][0]["valueBits"],
        65536
    );
    assert_eq!(
        summary_json["streams"][0]["rejected"][0]["provenance"]["source"]["kind"],
        "cell"
    );
    assert_eq!(
        summary_json["gpuGroups"][0]["nodes"][0]["modulePath"][0],
        "00000000000000000000000000000001"
    );
    assert_eq!(
        serde_json::from_value::<VegetationEvaluationSummaryDto>(summary_json).unwrap(),
        summary
    );

    let diagnostic = VegetationNodeEvaluationDiagnosticDto {
        module_path: vec![VegetationGuid(
            "00000000000000000000000000000001".to_owned(),
        )],
        node: VegetationGuid("00000000000000000000000000000002".to_owned()),
        operator: VegetationGraphOperatorDto::Noise,
        symbol: "noise".to_owned(),
        input_candidates: "10".to_owned(),
        output_candidates: "8".to_owned(),
        output_bytes: "256".to_owned(),
        predicted_transfer_bytes: "96".to_owned(),
        elapsed_micros: "12".to_owned(),
        execution_domain: VegetationExecutionDomainDto::SlangCompute,
    };
    let diagnostic_json = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(
        diagnostic_json["predictedTransferBytes"],
        serde_json::json!("96")
    );
    assert_eq!(diagnostic_json["executionDomain"], "slang-compute");
    assert!(diagnostic_json.get("transferBytes").is_none());
    assert_eq!(
        serde_json::from_value::<VegetationNodeEvaluationDiagnosticDto>(diagnostic_json).unwrap(),
        diagnostic
    );
}

/// An absent `Option<T>` field is a **missing key**, not `null`. `RaycastParams.maxDist`
/// is the representative case.
#[test]
fn absent_option_omits_the_key() {
    let params = RaycastParams {
        origin: Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
        dir: Vec3 {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        },
        max_dist: None,
    };
    let json = serde_json::to_value(&params).unwrap();
    let object = json.as_object().unwrap();
    assert!(
        !object.contains_key("maxDist"),
        "absent Option must omit the key, not emit null"
    );
    assert!(object.contains_key("origin"));
    assert!(object.contains_key("dir"));

    // A present value emits the camelCase key.
    let with_dist = RaycastParams {
        max_dist: Some(500.0),
        ..params
    };
    let json = serde_json::to_value(&with_dist).unwrap();
    assert_eq!(json["maxDist"], serde_json::json!(500.0));
}

/// A representative `*Result` round-trips through the wire keys (camelCase, `Uuid` as a
/// decimal string, nested `Vec3`/enum), proving field-key fidelity end-to-end.
#[test]
fn representative_result_round_trips() {
    let result = FitColliderResult {
        entity: Uuid(123_456_789),
        shape: "box".to_owned(),
        half_extents: Vec3 {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        },
        offset: Vec3 {
            x: 0.0,
            y: 0.5,
            z: 0.0,
        },
    };
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["entity"], serde_json::json!("123456789"));
    assert_eq!(json["shape"], serde_json::json!("box"));
    assert_eq!(json["halfExtents"]["y"], serde_json::json!(2.0));

    let back: FitColliderResult = serde_json::from_value(json).unwrap();
    assert_eq!(back, result);
}

/// `f32` fields accept an `f64`-shaped wire number, narrowing on read — a JS client emits
/// all numbers as doubles.
#[test]
fn f32_field_accepts_a_double_wire_number() {
    let object = serde_json::json!({
        "origin": { "x": 0.0, "y": 0.0, "z": 0.0 },
        "dir": { "x": 0.0, "y": 0.0, "z": 1.0 },
        "maxDist": 1000.0
    });
    let params: RaycastParams = serde_json::from_value(object).unwrap();
    assert_eq!(params.max_dist, Some(1000.0));
}
