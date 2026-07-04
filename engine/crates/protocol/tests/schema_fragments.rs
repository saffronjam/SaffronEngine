//! The exhaustive schema-fragment oracle: every DTO's `fragment_for` output must equal the
//! committed `openrpc.generated.json` `components.schemas` entry (after a canonical deep key
//! sort, since `schemars` orders properties alphabetically while the committed schema uses
//! declaration order — a difference phase-5 resolves, irrelevant to the validation shape).
//!
//! This proves the normalizer + selector/special-case override table reproduces the frozen
//! wire shape for all 232 deriving DTO structs, not just the phase gate's samples — so the
//! phase-5 OpenRPC assembly inherits a proven per-DTO fragment source.

use saffron_protocol::{fragment_for, *};
use schemars::JsonSchema;
use serde_json::Value;

/// The committed contract artifact: the byte-equivalence target.
const OPENRPC: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../schemas/control/openrpc.generated.json"
));

/// Canonical deep key sort, so property insertion order does not affect equality.
fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), sorted(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

#[test]
fn every_dto_fragment_matches_committed_openrpc() {
    let doc: Value = serde_json::from_str(OPENRPC).expect("committed openrpc parses");
    let schemas = &doc["components"]["schemas"];

    fn check<T: JsonSchema>(name: &str, schemas: &Value) {
        let got = fragment_for::<T>(name);
        let want = &schemas[name];
        assert!(!want.is_null(), "{name} missing from committed openrpc");
        assert_eq!(
            sorted(&got),
            sorted(want),
            "fragment for {name} drifted from committed openrpc"
        );
    }

    macro_rules! check {
        ($t:ty, $n:literal) => {
            check::<$t>($n, schemas);
        };
    }

    check!(EntityRef, "EntityRef");
    check!(Vec3, "Vec3");
    check!(Vec4, "Vec4");
    check!(PingParams, "PingParams");
    check!(EmptyParams, "EmptyParams");
    check!(PingResult, "PingResult");
    check!(RenderStatsDto, "RenderStatsDto");
    check!(RenderPassTimingDto, "RenderPassTimingDto");
    check!(RenderPassTimingsDto, "RenderPassTimingsDto");
    check!(ProfilerSetModeParams, "ProfilerSetModeParams");
    check!(ProfilerModeResult, "ProfilerModeResult");
    check!(PipelineStatsDto, "PipelineStatsDto");
    check!(ProfileSpanDto, "ProfileSpanDto");
    check!(ProfileCaptureMetadataDto, "ProfileCaptureMetadataDto");
    check!(ProfileCaptureDto, "ProfileCaptureDto");
    check!(CaptureStartParams, "CaptureStartParams");
    check!(CaptureStartResult, "CaptureStartResult");
    check!(CaptureStopResult, "CaptureStopResult");
    check!(CaptureStatusResult, "CaptureStatusResult");
    check!(FrameSampleDto, "FrameSampleDto");
    check!(FrameHistoryParams, "FrameHistoryParams");
    check!(FrameHistoryDto, "FrameHistoryDto");
    check!(PerfConfigDto, "PerfConfigDto");
    check!(SetPerfConfigParams, "SetPerfConfigParams");
    check!(AlarmEventDto, "AlarmEventDto");
    check!(DrainAlarmsParams, "DrainAlarmsParams");
    check!(DrainAlarmsResult, "DrainAlarmsResult");
    check!(ScriptStatusResult, "ScriptStatusResult");
    check!(PhysicsStateResult, "PhysicsStateResult");
    check!(FitColliderParams, "FitColliderParams");
    check!(FitColliderResult, "FitColliderResult");
    check!(ContactEventDto, "ContactEventDto");
    check!(DrainContactsParams, "DrainContactsParams");
    check!(DrainContactsResult, "DrainContactsResult");
    check!(PhysicsBodyDto, "PhysicsBodyDto");
    check!(PhysicsBodiesResult, "PhysicsBodiesResult");
    check!(ApplyImpulseParams, "ApplyImpulseParams");
    check!(ApplyImpulseResult, "ApplyImpulseResult");
    check!(SetKinematicBonesParams, "SetKinematicBonesParams");
    check!(KinematicBonesResult, "KinematicBonesResult");
    check!(MoveCharacterParams, "MoveCharacterParams");
    check!(MoveCharacterResult, "MoveCharacterResult");
    check!(RaycastParams, "RaycastParams");
    check!(ShapecastParams, "ShapecastParams");
    check!(RaycastResult, "RaycastResult");
    check!(EnableRagdollParams, "EnableRagdollParams");
    check!(RagdollResult, "RagdollResult");
    check!(SetRagdollParams, "SetRagdollParams");
    check!(GetRagdollParams, "GetRagdollParams");
    check!(ScriptErrorDto, "ScriptErrorDto");
    check!(DrainScriptErrorsParams, "DrainScriptErrorsParams");
    check!(DrainScriptErrorsResult, "DrainScriptErrorsResult");
    check!(ScriptLogDto, "ScriptLogDto");
    check!(DrainScriptLogsParams, "DrainScriptLogsParams");
    check!(DrainScriptLogsResult, "DrainScriptLogsResult");
    check!(GetScriptSchemaParams, "GetScriptSchemaParams");
    check!(ScriptFieldDto, "ScriptFieldDto");
    check!(GetScriptSchemaResult, "GetScriptSchemaResult");
    check!(SetScriptOverrideParams, "SetScriptOverrideParams");
    check!(SetScriptOverrideResult, "SetScriptOverrideResult");
    check!(CreateScriptParams, "CreateScriptParams");
    check!(CreateScriptResult, "CreateScriptResult");
    check!(ActiveAlarmDto, "ActiveAlarmDto");
    check!(ActiveAlarmsDto, "ActiveAlarmsDto");
    check!(SetAaParams, "SetAaParams");
    check!(SetAaResult, "SetAaResult");
    check!(SetViewModeParams, "SetViewModeParams");
    check!(SetViewModeResult, "SetViewModeResult");
    check!(ToggleParams, "ToggleParams");
    check!(SetClusteredResult, "SetClusteredResult");
    check!(SetIblResult, "SetIblResult");
    check!(SetRenderQualityParams, "SetRenderQualityParams");
    check!(RenderQualityResult, "RenderQualityResult");
    check!(SetTonemapParams, "SetTonemapParams");
    check!(TonemapResult, "TonemapResult");
    check!(SetRtShadowsResult, "SetRtShadowsResult");
    check!(SetRestirResult, "SetRestirResult");
    check!(SetGiParams, "SetGiParams");
    check!(SetGiResult, "SetGiResult");
    check!(SetShadowsResult, "SetShadowsResult");
    check!(SetSkinningResult, "SetSkinningResult");
    check!(SetDepthPrepassResult, "SetDepthPrepassResult");
    check!(ViewportNativeInfoResult, "ViewportNativeInfoResult");
    check!(SetViewportPowerStateParams, "SetViewportPowerStateParams");
    check!(ViewportPowerStateResult, "ViewportPowerStateResult");
    check!(SetViewportSizeParams, "SetViewportSizeParams");
    check!(SetViewportSizeResult, "SetViewportSizeResult");
    check!(SetActiveViewParams, "SetActiveViewParams");
    check!(SetActiveViewResult, "SetActiveViewResult");
    check!(ProjectInfoDto, "ProjectInfoDto");
    check!(ProjectStatusDto, "ProjectStatusDto");
    check!(NewProjectParams, "NewProjectParams");
    check!(PathParams, "PathParams");
    check!(OptionalPathParams, "OptionalPathParams");
    check!(ImportModelResult, "ImportModelResult");
    check!(InstantiateModelParams, "InstantiateModelParams");
    check!(ExtractSubAssetParams, "ExtractSubAssetParams");
    check!(ClearExtractionParams, "ClearExtractionParams");
    check!(ImportTextureResult, "ImportTextureResult");
    check!(AssetEntryDto, "AssetEntryDto");
    check!(AssetList, "AssetList");
    check!(ScanAssetsResult, "ScanAssetsResult");
    check!(ReimportModelResult, "ReimportModelResult");
    check!(ReimportModelParams, "ReimportModelParams");
    check!(ModelInfoParams, "ModelInfoParams");
    check!(ModelSubAssetDto, "ModelSubAssetDto");
    check!(ModelInfoResult, "ModelInfoResult");
    check!(AssetReferencesParams, "AssetReferencesParams");
    check!(AssetReferencesResult, "AssetReferencesResult");
    check!(CleanCandidateDto, "CleanCandidateDto");
    check!(CleanReport, "CleanReport");
    check!(CleanAssetsParams, "CleanAssetsParams");
    check!(DeleteUnusedParams, "DeleteUnusedParams");
    check!(DeleteUnusedResult, "DeleteUnusedResult");
    check!(RenameAssetParams, "RenameAssetParams");
    check!(AssetRef, "AssetRef");
    check!(CreateAssetFolderParams, "CreateAssetFolderParams");
    check!(RenameAssetFolderParams, "RenameAssetFolderParams");
    check!(DeleteAssetFolderParams, "DeleteAssetFolderParams");
    check!(MoveAssetParams, "MoveAssetParams");
    check!(AssetUsagesParams, "AssetUsagesParams");
    check!(AssetUsageDto, "AssetUsageDto");
    check!(AssetUsagesResult, "AssetUsagesResult");
    check!(AssetMetadataParams, "AssetMetadataParams");
    check!(AssetMetadataDto, "AssetMetadataDto");
    check!(DeleteAssetParams, "DeleteAssetParams");
    check!(DeleteAssetResult, "DeleteAssetResult");
    check!(AssignAssetParams, "AssignAssetParams");
    check!(MaterialCreateParams, "MaterialCreateParams");
    check!(MaterialCreateResult, "MaterialCreateResult");
    check!(MaterialAssignParams, "MaterialAssignParams");
    check!(MaterialAssignResult, "MaterialAssignResult");
    check!(MaterialImportParams, "MaterialImportParams");
    check!(MaterialImportResultDto, "MaterialImportResultDto");
    check!(MaterialRefDto, "MaterialRefDto");
    check!(MaterialListResult, "MaterialListResult");
    check!(MaterialGetParams, "MaterialGetParams");
    check!(MaterialGetResult, "MaterialGetResult");
    check!(MaterialSchemaParams, "MaterialSchemaParams");
    check!(ExposedParamDto, "ExposedParamDto");
    check!(MaterialSchemaResult, "MaterialSchemaResult");
    check!(MaterialUpdateParams, "MaterialUpdateParams");
    check!(MaterialUpdateResult, "MaterialUpdateResult");
    check!(PreviewRenderParams, "PreviewRenderParams");
    check!(PreviewRenderResult, "PreviewRenderResult");
    check!(MaterialSetGraphParams, "MaterialSetGraphParams");
    check!(MaterialSetGraphResult, "MaterialSetGraphResult");
    check!(MaterialCreateInstanceParams, "MaterialCreateInstanceParams");
    check!(MaterialSetOverrideParams, "MaterialSetOverrideParams");
    check!(MaterialSetOverrideResult, "MaterialSetOverrideResult");
    check!(MaterialCompileParams, "MaterialCompileParams");
    check!(MaterialCompileResult, "MaterialCompileResult");
    check!(MaterialCookResult, "MaterialCookResult");
    check!(AssignAssetResult, "AssignAssetResult");
    check!(PathResult, "PathResult");
    check!(ScreenshotParams, "ScreenshotParams");
    check!(ScreenshotResult, "ScreenshotResult");
    check!(ThumbnailParams, "ThumbnailParams");
    check!(ThumbnailResult, "ThumbnailResult");
    check!(ThumbnailCacheParams, "ThumbnailCacheParams");
    check!(ThumbnailCacheResult, "ThumbnailCacheResult");
    check!(QuitResult, "QuitResult");
    check!(CreateEntityParams, "CreateEntityParams");
    check!(EntityParams, "EntityParams");
    check!(SetParentParams, "SetParentParams");
    check!(DestroyEntityResult, "DestroyEntityResult");
    check!(EntityListEntry, "EntityListEntry");
    check!(EntityList, "EntityList");
    check!(ComponentList, "ComponentList");
    check!(ComponentParams, "ComponentParams");
    check!(AddComponentResult, "AddComponentResult");
    check!(RemoveComponentResult, "RemoveComponentResult");
    check!(SetComponentParams, "SetComponentParams");
    check!(SetComponentResult, "SetComponentResult");
    check!(SetComponentOrderParams, "SetComponentOrderParams");
    check!(SetComponentOrderResult, "SetComponentOrderResult");
    check!(SetTransformParams, "SetTransformParams");
    check!(SetLightParams, "SetLightParams");
    check!(PickParams, "PickParams");
    check!(PickResult, "PickResult");
    check!(InspectResult, "InspectResult");
    check!(EnvironmentDto, "EnvironmentDto");
    check!(SetEnvironmentParams, "SetEnvironmentParams");
    check!(SetAtmosphereParams, "SetAtmosphereParams");
    check!(SelectionResult, "SelectionResult");
    check!(PlayStateResult, "PlayStateResult");
    check!(AnimationClipDto, "AnimationClipDto");
    check!(BoneDto, "BoneDto");
    check!(AssetCapabilitiesDto, "AssetCapabilitiesDto");
    check!(GetAssetModelParams, "GetAssetModelParams");
    check!(AssetModelResult, "AssetModelResult");
    check!(EnterAssetPreviewParams, "EnterAssetPreviewParams");
    check!(BoneEntityDto, "BoneEntityDto");
    check!(AssetPreviewResult, "AssetPreviewResult");
    check!(ListClipsParams, "ListClipsParams");
    check!(ListClipsResult, "ListClipsResult");
    check!(PlayAnimationParams, "PlayAnimationParams");
    check!(SeekAnimationParams, "SeekAnimationParams");
    check!(SetAnimationLoopParams, "SetAnimationLoopParams");
    check!(SetAnimationPlayingParams, "SetAnimationPlayingParams");
    check!(AnimationStateParams, "AnimationStateParams");
    check!(AnimationStateResult, "AnimationStateResult");
    check!(SetSkeletonOverlayParams, "SetSkeletonOverlayParams");
    check!(SkeletonOverlayResult, "SkeletonOverlayResult");
    check!(DebugOverlaysParams, "DebugOverlaysParams");
    check!(DebugOverlaysResult, "DebugOverlaysResult");
    check!(SetSkeletonHighlightParams, "SetSkeletonHighlightParams");
    check!(PickSkeletonJointParams, "PickSkeletonJointParams");
    check!(PickSkeletonJointResult, "PickSkeletonJointResult");
    check!(SetAssetPreviewOptionsParams, "SetAssetPreviewOptionsParams");
    check!(AssetPreviewOptionsResult, "AssetPreviewOptionsResult");
    check!(SetFootIkParams, "SetFootIkParams");
    check!(GetFootIkParams, "GetFootIkParams");
    check!(FootIkResult, "FootIkResult");
    check!(WorldTransformResult, "WorldTransformResult");
    check!(StepParams, "StepParams");
    check!(DeselectResult, "DeselectResult");
    check!(AddEntityParams, "AddEntityParams");
    check!(RenameEntityParams, "RenameEntityParams");
    check!(SetComponentFieldParams, "SetComponentFieldParams");
    check!(SetComponentFieldResult, "SetComponentFieldResult");
    check!(EditorCamera, "EditorCamera");
    check!(SetCameraParams, "SetCameraParams");
    check!(GizmoState, "GizmoState");
    check!(SetGizmoParams, "SetGizmoParams");
    check!(GizmoPointerParams, "GizmoPointerParams");
    check!(GizmoPointerResult, "GizmoPointerResult");
    check!(FlyInputParams, "FlyInputParams");
    check!(FlyInputResult, "FlyInputResult");
    check!(ScriptInputParams, "ScriptInputParams");
    check!(ScriptInputResult, "ScriptInputResult");
    check!(SetProbesParams, "SetProbesParams");
    check!(SetProbesResult, "SetProbesResult");
    check!(RecaptureProbesResult, "RecaptureProbesResult");
    check!(ProbeRef, "ProbeRef");
    check!(ListProbesResult, "ListProbesResult");
    check!(SetExposureParams, "SetExposureParams");
    check!(SetExposureResult, "SetExposureResult");
}
