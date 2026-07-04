import type {
  AssetEntryDto,
  AssetList as DtoAssetList,
  CommandParamsMap as DtoCommandParamsMap,
  CommandResultMap as DtoCommandResultMap,
  EntityRef,
  ProjectInfoDto,
  ProjectStatusDto,
  RenderStatsDto,
  ThumbnailResult,
  Vec3,
  WireUuid,
} from "./sa-types";

export type {
  ActiveAlarmDto,
  ActiveAlarmsDto,
  AddEntityParams,
  AlarmEventDto,
  AppManifest,
  ExportAppParams,
  ExportAppResult,
  AnimationClipDto,
  AnimationStateResult,
  AssetMetadataDto,
  AssetUsageDto,
  AssetUsagesParams,
  AssetUsagesResult,
  AssignAssetParams,
  BoneDto,
  AssetCapabilitiesDto,
  AssetModelResult,
  AssetPreviewResult,
  AssetPreviewOptionsResult,
  Camera,
  ComponentBody,
  ComponentList,
  ComponentParams,
  Components,
  Rigidbody,
  Collider,
  KinematicBones,
  CharacterController,
  BVec3,
  PhysicsMaterial,
  PhysicsStateResult,
  PhysicsBodiesResult,
  PhysicsBodyDto,
  DrainContactsResult,
  ContactEventDto,
  RagdollResult,
  CaptureStartParams,
  CaptureStartResult,
  CaptureStatusResult,
  CaptureStopResult,
  CreateEntityParams,
  CreateAssetFolderParams,
  CreateScriptParams,
  CreateScriptResult,
  DebugOverlaysResult,
  DeleteAssetParams,
  DeleteAssetResult,
  DeleteAssetFolderParams,
  DeselectResult,
  DestroyEntityResult,
  DirectionalLight,
  DrainAlarmsParams,
  DrainAlarmsResult,
  DrainScriptErrorsResult,
  DrainScriptLogsResult,
  EditorCamera,
  EntityList,
  EntityListEntry,
  EntityParams,
  EntityRef,
  FrameHistoryDto,
  FrameHistoryParams,
  FrameSampleDto,
  GetScriptSchemaResult,
  GizmoPointerParams,
  GizmoPointerResult,
  GizmoState,
  ImportModelResult,
  ImportTextureResult,
  InspectResult,
  ListClipsResult,
  ClipBindingsResult,
  MorphWeightsResult,
  ListProbesResult,
  Material,
  Mesh,
  MoveAssetParams,
  Name,
  OptionalPathParams,
  PathParams,
  PathResult,
  PerfConfigDto,
  PickParams,
  PickResult,
  PickSkeletonJointResult,
  PingParams,
  PingResult,
  PipelineStatsDto,
  PlayStateResult,
  PointLight,
  ProbeRef,
  ProfileCaptureDto,
  ProfileCaptureMetadataDto,
  ProfileSpanDto,
  ProfilerModeResult,
  ProfilerSetModeParams,
  QuitResult,
  RecaptureProbesResult,
  ReflectionProbe,
  RenderPassTimingDto,
  RenderPassTimingsDto,
  RenderQualityResult,
  RenameAssetParams,
  RenameAssetFolderParams,
  RenameEntityParams,
  ScreenshotParams,
  ScreenshotResult,
  Script,
  ScriptErrorDto,
  ScriptLogDto,
  ScriptFieldDto,
  ScriptInputParams,
  ScriptInputResult,
  ScriptSlot,
  ScriptStatusResult,
  SetAaParams,
  SetAaResult,
  SetAtmosphereParams,
  SetCameraParams,
  SetClusteredResult,
  SetComponentFieldParams,
  SetComponentFieldResult,
  SetComponentParams,
  SetComponentResult,
  SetDepthPrepassResult,
  SetEnvironmentParams,
  SetExposureParams,
  SetExposureResult,
  SetGiParams,
  SetGiResult,
  SetGizmoParams,
  SetIblResult,
  SetLightParams,
  SetPerfConfigParams,
  UpscaleDto,
  GetUpscaleResult,
  SetUpscaleParams,
  SetUpscaleResult,
  SetProbesParams,
  SetProbesResult,
  SetRenderQualityParams,
  SetRestirResult,
  SetRtShadowsResult,
  SetScriptOverrideParams,
  SetScriptOverrideResult,
  SetShadowsResult,
  SetTransformParams,
  SkeletonOverlayResult,
  SpotLight,
  StepParams,
  ThumbnailParams,
  ToggleParams,
  Transform,
  Vec3,
  Vec4,
  ViewportNativeInfoResult,
  WireUuid,
} from "./sa-types";

export type Uuid = WireUuid;
export type AssetEntry = AssetEntryDto;
export type AssetList = DtoAssetList;
export type ProjectInfo = ProjectInfoDto;
export type ProjectStatus = ProjectStatusDto;
// The codegen inlines enums into the DTOs that use them (no standalone enum exports), so the phase /
// boot-stage unions are derived from the status DTO's fields — the single source of truth.
export type ProjectPhase = ProjectStatusDto["phase"];
export type BootStage = ProjectStatusDto["stage"];
export type RenderStats = RenderStatsDto;
export type Thumbnail = Omit<ThumbnailResult, "format"> & { format: "png" };

export interface Envelope {
  ok: boolean;
  error?: string;
  result?: unknown;
}

export interface Environment {
  skyMode: "color" | "texture" | "procedural";
  clearColor: Vec3;
  skyTexture: Uuid;
  skyIntensity: number;
  skyRotation: number;
  exposure: number;
  visible: boolean;
  useSkyForAmbient: boolean;
  ambientColor: Vec3;
  ambientIntensity: number;
  atmosphere: {
    enabled: boolean;
    planetRadius: number;
    atmosphereHeight: number;
    rayleighScattering: Vec3;
    rayleighScaleHeight: number;
    mieScattering: number;
    mieScaleHeight: number;
    mieAnisotropy: number;
    ozoneAbsorption: Vec3;
    sunDiskAngularRadius: number;
    sunDiskIntensity: number;
  };
}

export interface Selection {
  entity: EntityRef | null;
  selectionVersion: number;
  sceneVersion: number;
  playState: string;
  playVersion: number;
  animationVersion: number;
}

type CompatCommandResultOverrides = {
  "get-environment": Environment;
  "set-environment": Environment;
  "set-atmosphere": Environment;
  "get-selection": Selection;
  "get-thumbnail": Thumbnail;
  "view-asset": Thumbnail;
};

export type CommandParamsMap = DtoCommandParamsMap;
export type CommandResultMap = Omit<
  DtoCommandResultMap,
  keyof CompatCommandResultOverrides
> &
  CompatCommandResultOverrides;
