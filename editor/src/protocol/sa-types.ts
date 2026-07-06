/**
 * GENERATED - do not edit.
 *
 * Produced by cargo run -p xtask -- gen-protocol.
 */

export type WireUuid = string;

/** The material alpha/blend mode (glTF `alphaMode`). */
export type BlendMode = "opaque" | "masked" | "translucent";

export interface Name {
  name: string;
}

export interface Transform {
  translation: Vec3;
  scale: Vec3;
  rotation: Vec3;
}

export interface Mesh {
  mesh: WireUuid;
}

export interface Camera {
  fov: number;
  near: number;
  far: number;
  primary: boolean;
  showModel: boolean;
  showFrustum: boolean;
  frustumMaxDistance: number;
}

export interface Material {
  baseColor: Vec4;
  albedoTexture: WireUuid;
  metallicRoughnessTexture: WireUuid;
  metallic: number;
  roughness: number;
  emissive: Vec3;
  emissiveStrength: number;
  unlit: boolean;
  normalTexture: WireUuid;
  occlusionTexture: WireUuid;
  emissiveTexture: WireUuid;
  heightTexture: WireUuid;
  normalStrength: number;
  heightScale: number;
  blend: BlendMode;
  alphaCutoff: number;
}

export interface MaterialSet {
  slots: Material[];
}

export interface ScriptSlot {
  scriptPath: string;
  overrides: Record<string, unknown>;
}

export interface Script {
  scripts: ScriptSlot[];
}

export interface DirectionalLight {
  direction: Vec3;
  color: Vec3;
  intensity: number;
  ambient: number;
}

export interface PointLight {
  color: Vec3;
  intensity: number;
  range: number;
}

export interface SpotLight {
  direction: Vec3;
  color: Vec3;
  intensity: number;
  range: number;
  innerAngle: number;
  outerAngle: number;
}

export interface ReflectionProbe {
  influenceRadius: number;
  intensity: number;
  boxProjection: boolean;
  boxExtent: Vec3;
}

export interface Relationship {
  parent: WireUuid;
}

export interface SkinnedMesh {
  mesh: WireUuid;
  rootBone: WireUuid;
  bones: WireUuid[];
  inverseBind: number[][];
}

export interface Bone {}

export interface ModelInstance {
  modelId: WireUuid;
}

export interface FootChainDto {
  upper: number;
  mid: number;
  end: number;
  poleVector: Vec3;
}

export interface FootIk {
  enabled: boolean;
  groundHeight: number;
  chains: FootChainDto[];
}

export interface BonePhysicsDto {
  shapeHalfExtents: Vec3;
  mass: number;
  joint: string;
  swingTwistLimits: Vec3;
  driveStiffness: number;
  driveDamping: number;
  driveMaxForce: number;
}

export interface BonePhysics {
  bones: BonePhysicsDto[];
}

export interface BVec3 {
  x: boolean;
  y: boolean;
  z: boolean;
}

export interface PhysicsMaterial {
  friction: number;
  restitution: number;
}

export interface Rigidbody {
  motion: "static" | "kinematic" | "dynamic";
  mass: number;
  linearDamping: number;
  angularDamping: number;
  gravityFactor: number;
  lockPosition: BVec3;
  lockRotation: BVec3;
  collisionLayer: number;
}

export interface Collider {
  shape: "box" | "sphere" | "capsule" | "convexhull" | "mesh";
  halfExtents: Vec3;
  sourceMesh: WireUuid;
  offset: Vec3;
  material: PhysicsMaterial;
  isSensor: boolean;
}

export interface KinematicBones {
  enabled: boolean;
  driven: number[];
}

export interface CharacterController {
  maxSpeed: number;
  maxSlopeAngle: number;
  maxStepHeight: number;
  gravityFactor: number;
}

export interface AtmosphereSettingsDto {
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
}

export interface Components {
  Name?: Name;
  Transform?: Transform;
  Mesh?: Mesh;
  Camera?: Camera;
  Material?: Material;
  MaterialSet?: MaterialSet;
  Script?: Script;
  DirectionalLight?: DirectionalLight;
  PointLight?: PointLight;
  SpotLight?: SpotLight;
  ReflectionProbe?: ReflectionProbe;
  Relationship?: Relationship;
  SkinnedMesh?: SkinnedMesh;
  Bone?: Bone;
  FootIk?: FootIk;
  BonePhysics?: BonePhysics;
  Rigidbody?: Rigidbody;
  Collider?: Collider;
  KinematicBones?: KinematicBones;
  CharacterController?: CharacterController;
}

export type ComponentBody =
  | Name
  | Transform
  | Mesh
  | Camera
  | Material
  | MaterialSet
  | Script
  | DirectionalLight
  | PointLight
  | SpotLight
  | ReflectionProbe
  | Relationship
  | SkinnedMesh
  | Bone
  | ModelInstance
  | FootIk
  | BonePhysics
  | Rigidbody
  | Collider
  | KinematicBones
  | CharacterController
  | Record<string, unknown>;

export interface EntityRef {
  id: WireUuid;
  name: string;
}

export interface PingParams {

}

export interface PingResult {
  pong: boolean;
  engine: string;
  version: string;
  pid: number;
}

export interface EmptyParams {

}

export interface RenderStatsDto {
  drawCalls: number;
  batches: number;
  instances: number;
  frameMs: number;
  fps: number;
  gpuMs: number;
  cpuFrameMs: number;
  gpuFrameMs: number;
  cpuWaitMs: number;
  triangles: number;
  descriptorBinds: number;
  commandBuffers: number;
  queueSubmits: number;
  pipelinesCreated: number;
  vramUsageBytes: number;
  vramBudgetBytes: number;
  softwareGpu: boolean;
  profilerMode: "off" | "timestamps" | "pipeline-stats";
  clustered: boolean;
  depthPrepass: boolean;
  shadows: boolean;
  ibl: boolean;
  ssao: boolean;
  contactShadows: boolean;
  ssgi: boolean;
  renderScale: number;
  skyOcclusion: boolean;
  quality: string;
  tonemap: string;
  idle: boolean;
  converged: boolean;
  redrawReasons: string[];
  powerState: string;
  ddgi: boolean;
  gdf: boolean;
  rtSupported: boolean;
  rtShadows: boolean;
  restir: boolean;
  ssr: boolean;
  rtReflections: boolean;
  blasCount: number;
  pipelines: number;
  bindlessTextures: number;
  bindlessFree: number;
  hdr: boolean;
  exposureEv: number;
  aa: "off" | "fxaa" | "taa" | "msaa2" | "msaa4" | "msaa8";
  viewMode: "lit" | "unlit" | "wireframe" | "lit-wireframe" | "detail-lighting" | "lighting-only" | "reflections" | "albedo" | "normal" | "roughness" | "metallic" | "emissive" | "depth" | "ambient-occlusion" | "gi" | "light-complexity" | "motion-vectors";
}

export interface ProfilerSetModeParams {
  mode?: "off" | "timestamps" | "pipeline-stats";
}

export interface ProfilerModeResult {
  mode: "off" | "timestamps" | "pipeline-stats";
  timestampsSupported: boolean;
  pipelineStatsSupported: boolean;
  softwareGpu: boolean;
}

export interface RenderPassTimingsDto {
  passes: RenderPassTimingDto[];
  gpuTotalMs: number;
  softwareGpu: boolean;
  profilerMode: "off" | "timestamps" | "pipeline-stats";
}

export interface RenderPassTimingDto {
  name: string;
  gpuMs: number;
}

export interface CaptureStartParams {
  mode?: "single" | "frames" | "rolling";
  frames?: number;
  filter?: string;
  includeCpu?: boolean;
  includePipelineStats?: boolean;
}

export interface CaptureStartResult {
  captureId: number;
  ack: boolean;
}

export interface CaptureStopResult {
  ready: boolean;
  mode: "single" | "frames" | "rolling";
  frameCount: number;
  inlined: boolean;
  capture: ProfileCaptureDto;
  chromeTrace: string;
  path: string;
  pending: boolean;
}

export interface ProfileCaptureDto {
  spans: ProfileSpanDto[];
  metadata: ProfileCaptureMetadataDto;
}

export interface ProfileSpanDto {
  name: string;
  lane: "cpu" | "gpu";
  startNs: number;
  endNs: number;
  parentIndex: number;
  depth: number;
  pipelineStats?: PipelineStatsDto;
}

export interface PipelineStatsDto {
  inputVertices: number;
  vertexInvocations: number;
  clippingInvocations: number;
  clippingPrimitives: number;
  fragmentInvocations: number;
  computeInvocations: number;
  pixels: number;
}

export interface ProfileCaptureMetadataDto {
  softwareGpu: boolean;
  correlated: boolean;
  deviceName: string;
  timestampPeriod: number;
  targetFps: number;
  mode: "off" | "timestamps" | "pipeline-stats";
  filter: string;
  frameCount: number;
}

export interface CaptureStatusResult {
  state: "idle" | "arming" | "recording" | "ready";
  capturedFrames: number;
  targetFrames: number;
  mode: "single" | "frames" | "rolling";
  pipelineStatsSupported: boolean;
}

export interface FrameHistoryParams {
  samples?: number;
}

export interface FrameHistoryDto {
  p50Ms: number;
  p95Ms: number;
  p99Ms: number;
  p999Ms: number;
  maxMs: number;
  meanMs: number;
  stddevMs: number;
  stutterCount: number;
  sampleCount: number;
  budgetMs: number;
  samples: FrameSampleDto[];
}

export interface FrameSampleDto {
  frameIndex: number;
  cpuMs: number;
  gpuMs: number;
  cpuWaitMs: number;
}

export interface PerfConfigDto {
  targetFps: number;
  budgetMs: number;
  greenBudgetFrac: number;
  greenMedianMul: number;
  amberMedianMul: number;
  frozenMs: number;
  vramWarnFrac: number;
  vramCritFrac: number;
  autoQuality: boolean;
}

export interface SetPerfConfigParams {
  greenBudgetFrac?: number;
  greenMedianMul?: number;
  amberMedianMul?: number;
  frozenMs?: number;
  vramWarnFrac?: number;
  vramCritFrac?: number;
}

export interface GetUpscaleResult {
  upscale: UpscaleDto;
}

export interface UpscaleDto {
  ratio: number;
  dynamic: boolean;
  targetMs: number;
  inputWidth: number;
  inputHeight: number;
  displayWidth: number;
  displayHeight: number;
}

export interface SetUpscaleParams {
  ratio?: number;
  dynamic?: boolean;
  targetMs?: number;
}

export interface SetUpscaleResult {
  upscale: UpscaleDto;
}

export interface DrainAlarmsParams {
  since?: number;
}

export interface DrainAlarmsResult {
  events: AlarmEventDto[];
  highWaterSeq: number;
  oldestSeq: number;
  overflowed: boolean;
}

export interface AlarmEventDto {
  seq: number;
  fingerprint: string;
  metric: string;
  pass: string;
  severity: "info" | "warning" | "critical";
  state: "firing" | "resolved";
  value: number;
  threshold: number;
  sinceFrame: number;
  count: number;
  durationMs: number;
}

export interface ActiveAlarmsDto {
  alarms: ActiveAlarmDto[];
}

export interface ActiveAlarmDto {
  fingerprint: string;
  metric: string;
  pass: string;
  severity: "info" | "warning" | "critical";
  value: number;
  threshold: number;
  sinceFrame: number;
  count: number;
}

export interface SetAaParams {
  mode?: "off" | "fxaa" | "taa" | "msaa2" | "msaa4" | "msaa8";
}

export interface SetAaResult {
  aa: "off" | "fxaa" | "taa" | "msaa2" | "msaa4" | "msaa8";
}

export interface GetTaaParamsResult {
  params: TaaParamsDto;
}

export interface TaaParamsDto {
  feedbackMin: number;
  feedbackMax: number;
  velocityRejection: number;
  clipGamma: number;
  sharpness: number;
}

export interface SetTaaParamsParams {
  feedbackMin?: number;
  feedbackMax?: number;
  velocityRejection?: number;
  clipGamma?: number;
  sharpness?: number;
}

export interface SetTaaParamsResult {
  params: TaaParamsDto;
}

export interface SetViewModeParams {
  mode?: "lit" | "unlit" | "wireframe" | "lit-wireframe" | "detail-lighting" | "lighting-only" | "reflections" | "albedo" | "normal" | "roughness" | "metallic" | "emissive" | "depth" | "ambient-occlusion" | "gi" | "light-complexity" | "motion-vectors";
}

export interface SetViewModeResult {
  viewMode: "lit" | "unlit" | "wireframe" | "lit-wireframe" | "detail-lighting" | "lighting-only" | "reflections" | "albedo" | "normal" | "roughness" | "metallic" | "emissive" | "depth" | "ambient-occlusion" | "gi" | "light-complexity" | "motion-vectors";
}

export interface ToggleParams {
  enabled?: boolean;
}

export interface SetClusteredResult {
  clustered: boolean;
}

export interface SetIblResult {
  ibl: boolean;
}

export interface SetSkyOcclusionResult {
  skyOcclusion: boolean;
}

export interface SetGdfResult {
  gdf: boolean;
}

export interface SetRenderQualityParams {
  tier: string;
}

export interface RenderQualityResult {
  tier: string;
  ssgi: boolean;
  gtao: boolean;
  contactShadows: boolean;
}

export interface SetTonemapParams {
  mode: string;
}

export interface TonemapResult {
  mode: string;
}

export interface SetRtShadowsResult {
  rtShadows: boolean;
}

export interface SetRestirResult {
  restir: boolean;
}

export interface SetSsrResult {
  ssr: boolean;
}

export interface SetRtReflectionsResult {
  rtReflections: boolean;
}

export interface SetGiParams {
  mode: "off" | "ddgi";
}

export interface SetGiResult {
  ddgi: boolean;
}

export interface SetShadowsResult {
  shadows: boolean;
}

export interface SetSkinningResult {
  skinning: boolean;
}

export interface SetDisplacementResult {
  displacement: boolean;
}

export interface SetDepthPrepassResult {
  depthPrepass: boolean;
}

export interface ViewportNativeInfoResult {
  platform: string;
  transport: string;
  status: string;
  controlSocket: string;
  width: number;
  height: number;
  message: string;
}

export interface SetViewportPowerStateParams {
  state: string;
}

export interface ViewportPowerStateResult {
  state: string;
}

export interface SetViewportSizeParams {
  view?: string;
  width?: number;
  height?: number;
}

export interface SetViewportSizeResult {
  width: number;
  height: number;
}

export interface EntityList {
  entities: EntityListEntry[];
}

export interface EntityListEntry {
  id: WireUuid;
  name: string;
  parentId?: WireUuid;
  bone?: boolean;
}

export interface ComponentList {
  components: string[];
}

export interface CreateEntityParams {
  name: string;
}

export interface EntityParams {
  entity: WireUuid | string | number;
}

export interface DestroyEntityResult {
  destroyed: WireUuid;
}

export interface SetParentParams {
  entity: WireUuid | string | number;
  parent?: WireUuid | string | number;
}

export interface ComponentParams {
  entity: WireUuid | string | number;
  component: string;
}

export interface AddComponentResult {
  added: string;
}

export interface RemoveComponentResult {
  removed: string;
}

export interface SetComponentParams {
  entity: WireUuid | string | number;
  component: string;
  json: ComponentBody;
}

export interface SetComponentResult {
  set: string;
}

export interface SetComponentOrderParams {
  entity: WireUuid | string | number;
  components: string[];
}

export interface SetComponentOrderResult {
  components: string[];
}

export interface SetTransformParams {
  entity: WireUuid | string | number;
  translation?: Vec3;
  rotation?: Vec3;
  scale?: Vec3;
  smooth?: boolean;
}

export interface Vec3 {
  x: number;
  y: number;
  z: number;
}

export interface SetLightParams {
  entity?: WireUuid | string | number;
  direction?: Vec3;
  color?: Vec3;
  intensity?: number;
  ambient?: number;
}

export interface PickParams {
  u?: number;
  v?: number;
}

export interface PickResult {
  hit: boolean;
  id?: WireUuid;
  name?: string;
  kind?: "billboard" | "mesh";
}

export interface InspectResult {
  id: WireUuid;
  name: string;
  components: Components;
  componentOrder: string[];
}

export interface WorldTransformResult {
  translation: Vec3;
  scale: Vec3;
}

export interface EnvironmentDto {
  skyMode: "color" | "texture" | "procedural";
  clearColor: Vec3;
  skyTexture: WireUuid;
  skyIntensity: number;
  skyRotation: number;
  exposure: number;
  visible: boolean;
  useSkyForAmbient: boolean;
  ambientColor: Vec3;
  ambientIntensity: number;
  atmosphere: AtmosphereSettingsDto;
}

export interface SetEnvironmentParams {
  json?: unknown;
  skyMode?: string;
  clearColor?: Vec3;
  skyTexture?: WireUuid;
  skyIntensity?: number;
  skyRotation?: number;
  exposure?: number;
  visible?: boolean;
  useSkyForAmbient?: boolean;
  ambientColor?: Vec3;
  ambientIntensity?: number;
}

export interface SetAtmosphereParams {
  json?: unknown;
  enabled?: boolean;
  planetRadius?: number;
  atmosphereHeight?: number;
  rayleighScattering?: Vec3;
  rayleighScaleHeight?: number;
  mieScattering?: number;
  mieScaleHeight?: number;
  mieAnisotropy?: number;
  ozoneAbsorption?: Vec3;
  sunDiskAngularRadius?: number;
  sunDiskIntensity?: number;
}

export interface SelectionResult {
  selectionVersion: number;
  sceneVersion: number;
  entity?: EntityRef;
  playState: string;
  playVersion: number;
  animationVersion: number;
}

export interface DeselectResult {
  selectionVersion: number;
}

export interface PlayStateResult {
  state: string;
  playVersion: number;
  sceneVersion: number;
  hasPrimaryCamera: boolean;
  animationVersion: number;
  previewAsset: WireUuid;
}

export interface StepParams {
  frames?: number;
}

export interface AnimationStateParams {
  entity: WireUuid | string | number;
}

export interface AnimationStateResult {
  clip: WireUuid;
  clipName: string;
  duration: number;
  time: number;
  playing: boolean;
  wrap: string;
  speed: number;
  animationVersion: number;
  morphWeights: number[];
}

export interface ListClipsParams {
  asset?: WireUuid | string | number;
}

export interface ListClipsResult {
  clips: AnimationClipDto[];
}

export interface AnimationClipDto {
  id: WireUuid;
  name: string;
  duration: number;
  channels: AnimationChannelDto[];
}

export interface AnimationChannelDto {
  kind: string;
  label: string;
  targetName: string;
  times: number[];
  width: number;
  values: number[];
}

export interface PlayAnimationParams {
  entity: WireUuid | string | number;
  clip: WireUuid | string | number;
  speed?: number;
  loop?: boolean;
  blend?: number;
  paused?: boolean;
}

export interface SetAnimationPlayingParams {
  entity: WireUuid | string | number;
  playing: boolean;
}

export interface SeekAnimationParams {
  entity: WireUuid | string | number;
  time: number;
  seekBlend?: number;
}

export interface SetAnimationLoopParams {
  entity: WireUuid | string | number;
  wrap: string;
}

export interface SkeletonOverlayResult {
  show: boolean;
  axes: boolean;
  jointSize: number;
  highlightJoint: number;
}

export interface SetSkeletonOverlayParams {
  show?: boolean;
  axes?: boolean;
  jointSize?: number;
}

export interface DebugOverlaysResult {
  bounds: boolean;
  sceneAabb: boolean;
  lightVolumes: boolean;
  grid: boolean;
  colliders: boolean;
}

export interface DebugOverlaysParams {
  bounds?: boolean;
  sceneAabb?: boolean;
  lightVolumes?: boolean;
  grid?: boolean;
  colliders?: boolean;
}

export interface SetSkeletonHighlightParams {
  joint: number;
}

export interface PickSkeletonJointParams {
  u: number;
  v: number;
  radiusPx?: number;
}

export interface PickSkeletonJointResult {
  found: boolean;
  nodeIndex: number;
}

export interface SetAssetPreviewOptionsParams {
  floor?: boolean;
}

export interface AssetPreviewOptionsResult {
  floor: boolean;
}

export interface GetFootIkParams {
  entity: WireUuid | string | number;
}

export interface FootIkResult {
  enabled: boolean;
  groundHeight: number;
  chains: number;
}

export interface SetFootIkParams {
  entity: WireUuid | string | number;
  enabled?: boolean;
  groundHeight?: number;
}

export interface SetMorphWeightsParams {
  entity: unknown;
  weights: number[];
}

export interface MorphWeightsResult {
  weights: number[];
  names: string[];
}

export interface GetMorphWeightsParams {
  entity: unknown;
}

export interface ListClipBindingsParams {
  entity: unknown;
  clip: unknown;
}

export interface ClipBindingsResult {
  channels: AnimationChannelDto[];
}

export interface ScriptStatusResult {
  state: string;
  instances: number;
  errorHighWater: number;
}

export interface PhysicsStateResult {
  active: boolean;
  bodyCount: number;
  dynamicCount: number;
}

export interface PhysicsBodiesResult {
  bodies: PhysicsBodyDto[];
}

export interface PhysicsBodyDto {
  entity: WireUuid;
  motion: string;
  active: boolean;
  position: Vec3;
}

export interface FitColliderParams {
  entity: WireUuid | string | number;
}

export interface FitColliderResult {
  entity: WireUuid;
  shape: string;
  halfExtents: Vec3;
  offset: Vec3;
}

export interface ApplyImpulseParams {
  entity: WireUuid | string | number;
  impulse: Vec3;
}

export interface ApplyImpulseResult {
  velocity: Vec3;
}

export interface DrainContactsParams {
  since?: number;
}

export interface DrainContactsResult {
  events: ContactEventDto[];
  highWaterSeq: number;
  oldestSeq: number;
  overflowed: boolean;
}

export interface ContactEventDto {
  seq: number;
  kind: string;
  entityA: WireUuid;
  entityB: WireUuid;
  sensor: boolean;
  point: Vec3;
  normal: Vec3;
  tick: number;
}

export interface SetKinematicBonesParams {
  entity: WireUuid | string | number;
  enabled?: boolean;
}

export interface KinematicBonesResult {
  entity: WireUuid;
  enabled: boolean;
  boneCount: number;
}

export interface MoveCharacterParams {
  entity: WireUuid | string | number;
  velocity: Vec3;
  jump?: boolean;
}

export interface MoveCharacterResult {
  position: Vec3;
  onGround: boolean;
}

export interface RaycastParams {
  origin: Vec3;
  dir: Vec3;
  maxDist?: number;
}

export interface RaycastResult {
  hit: boolean;
  entity: WireUuid;
  point: Vec3;
  normal: Vec3;
  distance: number;
}

export interface ShapecastParams {
  origin: Vec3;
  dir: Vec3;
  radius: number;
  maxDist?: number;
}

export interface EnableRagdollParams {
  entity: WireUuid | string | number;
  enabled?: boolean;
}

export interface RagdollResult {
  present: boolean;
  active: boolean;
  bodyWeight: number;
  bones: number;
}

export interface SetRagdollParams {
  entity: WireUuid | string | number;
  active?: boolean;
  bodyWeight?: number;
  bone?: number;
  weight?: number;
}

export interface GetRagdollParams {
  entity: WireUuid | string | number;
}

export interface DrainScriptErrorsParams {
  since?: number;
}

export interface DrainScriptErrorsResult {
  events: ScriptErrorDto[];
  highWaterSeq: number;
  oldestSeq: number;
  overflowed: boolean;
}

export interface ScriptErrorDto {
  seq: number;
  entity: WireUuid;
  script: string;
  message: string;
  tick: number;
}

export interface DrainScriptLogsParams {
  since?: number;
}

export interface DrainScriptLogsResult {
  events: ScriptLogDto[];
  highWaterSeq: number;
  oldestSeq: number;
  overflowed: boolean;
}

export interface ScriptLogDto {
  seq: number;
  entity: WireUuid;
  message: string;
  epochMs: number;
  tick: number;
}

export interface GetScriptSchemaParams {
  path: string;
}

export interface GetScriptSchemaResult {
  fields: ScriptFieldDto[];
}

export interface ScriptFieldDto {
  name: string;
  type: string;
  defaultValue: unknown;
}

export interface SetScriptOverrideParams {
  entity: WireUuid | string | number;
  slot: number;
  name: string;
  value: unknown;
}

export interface SetScriptOverrideResult {
  scriptPath: string;
  overrides: unknown;
}

export interface AddEntityParams {
  preset?: "empty" | "cube" | "plane" | "sphere" | "point-light" | "spot-light" | "directional-light" | "camera" | "reflection-probe";
}

export interface RenameEntityParams {
  entity: WireUuid | string | number;
  name: string;
}

export interface SetComponentFieldParams {
  entity: WireUuid | string | number;
  component: string;
  field: string;
  value: unknown;
  index?: number;
}

export interface SetComponentFieldResult {
  set: string;
  field: string;
}

export interface EditorCamera {
  position: Vec3;
  yaw: number;
  pitch: number;
  fov: number;
  near: number;
  far: number;
  moveSpeed: number;
  lookSpeed: number;
}

export interface SetCameraParams {
  position?: Vec3;
  yaw?: number;
  pitch?: number;
  fov?: number;
  near?: number;
  far?: number;
  moveSpeed?: number;
  lookSpeed?: number;
  pivot?: Vec3;
  distance?: number;
}

export interface GizmoState {
  op: "translate" | "rotate" | "scale";
  space: "world" | "local";
  preserveChildren: boolean;
}

export interface SetGizmoParams {
  op?: "translate" | "rotate" | "scale";
  space?: "world" | "local";
  preserveChildren?: boolean;
}

export interface GizmoPointerParams {
  phase?: "hover" | "begin" | "drag" | "end";
  x?: number;
  y?: number;
}

export interface GizmoPointerResult {
  hovered: string;
  dragging: boolean;
}

export interface FlyInputParams {
  active?: boolean;
  lookDx?: number;
  lookDy?: number;
  forward?: boolean;
  back?: boolean;
  left?: boolean;
  right?: boolean;
  up?: boolean;
  down?: boolean;
}

export interface FlyInputResult {
  active: boolean;
}

export interface ScriptInputParams {
  keys: string[];
  mouseButtons?: string[];
  mouseX?: number;
  mouseY?: number;
  scroll?: number;
}

export interface ScriptInputResult {
  keys: string[];
}

export interface SetProbesParams {
  enabled?: boolean;
}

export interface SetProbesResult {
  probes: boolean;
}

export interface RecaptureProbesResult {
  marked: number;
}

export interface ListProbesResult {
  enabled: boolean;
  count: number;
  probes: ProbeRef[];
}

export interface ProbeRef {
  slot: number;
  entity: WireUuid;
  origin: Vec3;
  influenceRadius: number;
  intensity: number;
  boxProjection: boolean;
  valid: boolean;
  dirty: boolean;
}

export interface SetExposureParams {
  ev: number;
}

export interface SetExposureResult {
  exposureEv: number;
}

export interface ProjectInfoDto {
  loaded: boolean;
  root: string;
  path: string;
  name: string;
  displayName: string;
}

export interface ProjectStatusDto {
  phase: "unloaded" | "loading" | "ready" | "failed";
  stage: "manifest" | "catalog" | "scene" | "install" | "assets" | "skybox" | "accel" | "ready" | "failed";
  done: number;
  total: number;
  label: string;
  currentItem: string;
  error: string;
  version: number;
  name: string;
  path: string;
}

export interface NewProjectParams {
  name?: string;
  displayName?: string;
  root?: string;
}

export interface CreateScriptParams {
  name: string;
}

export interface CreateScriptResult {
  path: string;
}

export interface PathParams {
  path: string;
}

export interface ImportModelParams {
  path: string;
  attribution?: AssetAttributionDto;
}

export interface AssetAttributionDto {
  licenseId: string;
  requiresAttribution: boolean;
  licenseUrl: string;
  author: string;
  sourceUrl: string;
  storeId: string;
}

export interface ImportModelResult {
  id: WireUuid;
  name: string;
  type: string;
}

export interface InstantiateModelParams {
  asset: WireUuid | string | number;
  name?: string;
}

export interface AssetPlacementParams {
  phase: "preview" | "commit" | "clear";
  asset?: unknown;
  u?: number;
  v?: number;
}

export interface AssetPlacementResult {
  active: boolean;
  valid: boolean;
  transform?: PlacementTransformDto;
  entity?: EntityRef;
  reason?: string;
}

export interface PlacementTransformDto {
  translation: Vec3;
  rotation: Vec3;
  scale: Vec3;
}

export interface ImportTextureParams {
  path: string;
  colorspace?: string;
  role?: string;
}

export interface ImportTextureResult {
  texture: WireUuid;
}

export interface AssetList {
  assets: AssetEntryDto[];
  folders: string[];
}

export interface AssetEntryDto {
  id: WireUuid;
  name: string;
  type: "mesh" | "texture" | "other" | "animation" | "material" | "model";
  path: string;
  folder?: string;
  container?: WireUuid;
  duration?: number;
  rigged?: boolean;
  colorspace?: string;
  role?: string;
  createdAt: number;
  attribution?: AssetAttributionDto;
}

export interface ScanAssetsResult {
  added: number;
  removed: number;
}

export interface ExtractSubAssetParams {
  asset: WireUuid | string | number;
  subAsset: WireUuid;
  dest?: string;
}

export interface AssetRef {
  id: WireUuid;
  name: string;
  folder?: string;
}

export interface ClearExtractionParams {
  asset: WireUuid | string | number;
  subAsset: WireUuid;
}

export interface ReimportModelParams {
  asset: WireUuid | string | number;
}

export interface ReimportModelResult {
  updated: number;
  added: number;
  removedFromSource: number;
  skipped: boolean;
}

export interface ModelInfoParams {
  asset: WireUuid | string | number;
}

export interface ModelInfoResult {
  id: WireUuid;
  name: string;
  sourcePath: string;
  sourceHash: string;
  materialCount: number;
  hasSkin: boolean;
  nodeCount: number;
  totalBytes: number;
  subAssets: ModelSubAssetDto[];
}

export interface ModelSubAssetDto {
  id: WireUuid;
  name: string;
  type: string;
  bytes: number;
}

export interface AssetReferencesParams {
  asset: WireUuid | string | number;
}

export interface AssetReferencesResult {
  referencedBy: string[];
  references: string[];
  footprint: number;
}

export interface GetAssetModelParams {
  asset: WireUuid | string | number;
}

export interface AssetModelResult {
  mesh: WireUuid;
  name: string;
  capabilities: AssetCapabilitiesDto;
  bones: BoneDto[];
  clips: AnimationClipDto[];
}

export interface AssetCapabilitiesDto {
  meshCount: number;
  materialCount: number;
  nodeCount: number;
  hasRig: boolean;
  boneCount: number;
  clipCount: number;
}

export interface BoneDto {
  index: number;
  name: string;
  parent: number;
  joint: boolean;
}

export interface EnterAssetPreviewParams {
  asset: WireUuid | string | number;
}

export interface AssetPreviewResult {
  rootEntity: WireUuid;
  bones: BoneEntityDto[];
  target: Vec3;
  distance: number;
}

export interface BoneEntityDto {
  index: number;
  entity: WireUuid;
}

export interface SetActiveViewParams {
  view: string;
}

export interface SetActiveViewResult {
  view: string;
}

export interface CleanAssetsParams {
  dryRun?: boolean;
  exclude?: string[];
}

export interface CleanReport {
  candidates: CleanCandidateDto[];
  reclaimableBytes: number;
}

export interface CleanCandidateDto {
  id: WireUuid;
  path: string;
  category: string;
  bytes: number;
  reason: string;
}

export interface DeleteUnusedParams {
  ids: string[];
  confirm?: boolean;
}

export interface DeleteUnusedResult {
  deleted: number;
  reclaimedBytes: number;
}

export interface RenameAssetParams {
  asset: WireUuid | string | number;
  name: string;
}

export interface CreateAssetFolderParams {
  folder: string;
}

export interface RenameAssetFolderParams {
  folder: string;
  name: string;
}

export interface DeleteAssetFolderParams {
  folder: string;
}

export interface MoveAssetParams {
  asset: WireUuid | string | number;
  folder?: string;
}

export interface AssetUsagesParams {
  asset: WireUuid | string | number;
}

export interface AssetUsagesResult {
  usages: AssetUsageDto[];
}

export interface AssetUsageDto {
  entity?: WireUuid;
  entityName?: string;
  slot: string;
}

export interface AssetMetadataParams {
  asset: WireUuid | string | number;
}

export interface AssetMetadataDto {
  id: WireUuid;
  name: string;
  type: "mesh" | "texture" | "other" | "animation" | "material" | "model";
  path: string;
  folder?: string;
  sizeBytes: number;
  vertexCount?: number;
  triangleCount?: number;
  createdAt: number;
}

export interface DeleteAssetParams {
  asset: WireUuid | string | number;
}

export interface DeleteAssetResult {
  id: WireUuid;
  name: string;
  cleared: AssetUsageDto[];
  fileDeleted: boolean;
}

export interface AssignAssetParams {
  entity: WireUuid | string | number;
  slot: "mesh" | "albedo" | "metallic-roughness" | "normal" | "occlusion" | "emissive" | "height";
  asset: WireUuid | string | number;
}

export interface AssignAssetResult {
  id: WireUuid;
  name: string;
  slot: "mesh" | "albedo" | "metallic-roughness" | "normal" | "occlusion" | "emissive" | "height";
}

export interface MaterialCreateParams {
  name: string;
}

export interface MaterialCreateResult {
  id: WireUuid;
  name: string;
}

export interface MaterialAssignParams {
  entity: WireUuid | string | number;
  material: WireUuid | string | number;
}

export interface MaterialAssignResult {
  material: WireUuid;
}

export interface MaterialImportParams {
  path: string;
  name: string;
  attribution?: AssetAttributionDto;
}

export interface MaterialImportResultDto {
  id: WireUuid;
  roles: string;
}

export interface MaterialListResult {
  materials: MaterialRefDto[];
}

export interface MaterialRefDto {
  id: WireUuid;
  name: string;
  folder: string;
}

export interface MaterialGetParams {
  material: WireUuid | string | number;
}

export interface MaterialGetResult {
  id: WireUuid;
  blend: string;
  unlit: boolean;
  baseColor: Vec4;
  metallic: number;
  roughness: number;
  emissive: Vec3;
  emissiveStrength: number;
  heightScale: number;
  heightMode: string;
  albedoTexture: WireUuid;
  ormTexture: WireUuid;
  normalTexture: WireUuid;
  emissiveTexture: WireUuid;
  heightTexture: WireUuid;
  vectorDisplacementTexture: WireUuid;
  graph: unknown;
}

export interface Vec4 {
  x: number;
  y: number;
  z: number;
  w: number;
}

export interface MaterialSchemaParams {
  material: unknown;
}

export interface MaterialSchemaResult {
  params: ExposedParamDto[];
}

export interface ExposedParamDto {
  name: string;
  kind: string;
  default: unknown;
}

export interface MaterialUpdateParams {
  material: WireUuid | string | number;
  baseColor?: Vec4;
  metallic?: number;
  roughness?: number;
  emissive?: Vec3;
  emissiveStrength?: number;
  normalStrength?: number;
  heightScale?: number;
  heightMode?: string;
  albedoTexture?: WireUuid;
  ormTexture?: WireUuid;
  normalTexture?: WireUuid;
  emissiveTexture?: WireUuid;
  heightTexture?: WireUuid;
  vectorDisplacementTexture?: WireUuid;
}

export interface MaterialUpdateResult {
  id: WireUuid;
}

export interface PreviewRenderParams {
  material: WireUuid | string | number;
  size?: number;
}

export interface PreviewRenderResult {
  png: string;
}

export interface MaterialSetGraphParams {
  material: WireUuid | string | number;
  graph: unknown;
}

export interface MaterialSetGraphResult {
  id: WireUuid;
  foldable: boolean;
}

export interface MaterialCreateInstanceParams {
  parent: WireUuid | string | number;
  name: string;
}

export interface MaterialSetOverrideParams {
  material: WireUuid | string | number;
  field: string;
  value: unknown;
}

export interface MaterialSetOverrideResult {
  id: WireUuid;
}

export interface MaterialCompileParams {
  material: WireUuid | string | number;
}

export interface MaterialCompileResult {
  id: WireUuid;
  ok: boolean;
}

export interface MaterialCookResult {
  compiled: number;
  failed: number;
}

export interface PathResult {
  path: string;
}

export interface OptionalPathParams {
  path?: string;
}

export interface ProjectStoresDto {
  enabled: string[];
}

export interface ScreenshotParams {
  target?: "viewport" | "window";
  path: string;
}

export interface ScreenshotResult {
  target: "viewport" | "window";
  path: string;
  pending: boolean;
}

export interface ThumbnailParams {
  asset: WireUuid | string | number;
  size?: number;
}

export interface ThumbnailResult {
  id: WireUuid;
  format: string;
  width: number;
  height: number;
  base64: string;
  pending: boolean;
}

export interface ThumbnailCacheParams {
  action: string;
}

export interface ThumbnailCacheResult {
  entries: number;
  bytes: number;
}

export interface ExportAppParams {
  outputDir: string;
  app: AppManifest;
}

export interface AppManifest {
  title: string;
  width: number;
  height: number;
  fullscreen: boolean;
  vsync: boolean;
}

export interface ExportAppResult {
  path: string;
  warnings: string[];
}

export interface QuitResult {
  quitting: boolean;
}

export interface CommandParamsMap {
  "ping": PingParams;
  "render-stats": EmptyParams;
  "profiler.set-mode": ProfilerSetModeParams;
  "pass-timings": EmptyParams;
  "profiler.capture-start": CaptureStartParams;
  "profiler.capture-stop": EmptyParams;
  "profiler.capture-status": EmptyParams;
  "frame-history": FrameHistoryParams;
  "get-perf-config": EmptyParams;
  "set-perf-config": SetPerfConfigParams;
  "get-upscale": EmptyParams;
  "set-upscale": SetUpscaleParams;
  "drain-alarms": DrainAlarmsParams;
  "list-active-alarms": EmptyParams;
  "set-aa": SetAaParams;
  "get-taa-params": EmptyParams;
  "set-taa-params": SetTaaParamsParams;
  "set-view-mode": SetViewModeParams;
  "set-clustered": ToggleParams;
  "set-ibl": ToggleParams;
  "set-sky-occlusion": ToggleParams;
  "set-gdf": ToggleParams;
  "set-render-quality": SetRenderQualityParams;
  "get-render-quality": EmptyParams;
  "set-tonemap": SetTonemapParams;
  "set-rt-shadows": ToggleParams;
  "set-restir": ToggleParams;
  "set-ssr": ToggleParams;
  "set-rt-reflections": ToggleParams;
  "set-gi": SetGiParams;
  "set-shadows": ToggleParams;
  "set-skinning": ToggleParams;
  "set-displacement": ToggleParams;
  "set-depth-prepass": ToggleParams;
  "viewport-native-info": EmptyParams;
  "set-viewport-power-state": SetViewportPowerStateParams;
  "set-viewport-size": SetViewportSizeParams;
  "list-entities": EmptyParams;
  "list-components": EmptyParams;
  "create-entity": CreateEntityParams;
  "destroy-entity": EntityParams;
  "set-parent": SetParentParams;
  "add-component": ComponentParams;
  "remove-component": ComponentParams;
  "set-component": SetComponentParams;
  "set-component-order": SetComponentOrderParams;
  "set-transform": SetTransformParams;
  "set-light": SetLightParams;
  "select": EntityParams;
  "pick": PickParams;
  "inspect": EntityParams;
  "focus": EntityParams;
  "get-world-transform": EntityParams;
  "get-environment": EmptyParams;
  "set-environment": SetEnvironmentParams;
  "set-atmosphere": SetAtmosphereParams;
  "get-selection": EmptyParams;
  "deselect": EmptyParams;
  "play": EmptyParams;
  "pause": EmptyParams;
  "step": StepParams;
  "stop": EmptyParams;
  "get-play-state": EmptyParams;
  "get-animation-state": AnimationStateParams;
  "list-clips": ListClipsParams;
  "play-animation": PlayAnimationParams;
  "set-animation-playing": SetAnimationPlayingParams;
  "seek-animation": SeekAnimationParams;
  "set-animation-loop": SetAnimationLoopParams;
  "stop-preview": AnimationStateParams;
  "get-skeleton-overlay": EmptyParams;
  "set-skeleton-overlay": SetSkeletonOverlayParams;
  "get-debug-overlays": EmptyParams;
  "set-debug-overlays": DebugOverlaysParams;
  "set-skeleton-highlight": SetSkeletonHighlightParams;
  "pick-skeleton-joint": PickSkeletonJointParams;
  "set-asset-preview-options": SetAssetPreviewOptionsParams;
  "get-foot-ik": GetFootIkParams;
  "set-foot-ik": SetFootIkParams;
  "set-morph-weights": SetMorphWeightsParams;
  "get-morph-weights": GetMorphWeightsParams;
  "list-clip-bindings": ListClipBindingsParams;
  "get-script-status": EmptyParams;
  "physics-state": EmptyParams;
  "physics-bodies": EmptyParams;
  "fit-collider": FitColliderParams;
  "apply-impulse": ApplyImpulseParams;
  "drain-contacts": DrainContactsParams;
  "set-kinematic-bones": SetKinematicBonesParams;
  "move-character": MoveCharacterParams;
  "raycast": RaycastParams;
  "shapecast": ShapecastParams;
  "enable-ragdoll": EnableRagdollParams;
  "set-ragdoll": SetRagdollParams;
  "get-ragdoll": GetRagdollParams;
  "drain-script-errors": DrainScriptErrorsParams;
  "drain-script-logs": DrainScriptLogsParams;
  "get-script-schema": GetScriptSchemaParams;
  "set-script-override": SetScriptOverrideParams;
  "add-entity": AddEntityParams;
  "copy-entity": EntityParams;
  "rename-entity": RenameEntityParams;
  "set-component-field": SetComponentFieldParams;
  "get-camera": EmptyParams;
  "set-camera": SetCameraParams;
  "get-gizmo": EmptyParams;
  "set-gizmo": SetGizmoParams;
  "gizmo-pointer": GizmoPointerParams;
  "fly-input": FlyInputParams;
  "script-input": ScriptInputParams;
  "set-probes": SetProbesParams;
  "recapture-probes": EmptyParams;
  "list-probes": EmptyParams;
  "set-exposure": SetExposureParams;
  "get-project": EmptyParams;
  "project-status": EmptyParams;
  "cancel-load": EmptyParams;
  "new-project": NewProjectParams;
  "create-script": CreateScriptParams;
  "open-project": PathParams;
  "import-model": ImportModelParams;
  "instantiate-model": InstantiateModelParams;
  "asset-placement": AssetPlacementParams;
  "import-texture": ImportTextureParams;
  "list-assets": EmptyParams;
  "scan-assets": EmptyParams;
  "extract-subasset": ExtractSubAssetParams;
  "clear-extraction": ClearExtractionParams;
  "reimport-model": ReimportModelParams;
  "model-info": ModelInfoParams;
  "asset-references": AssetReferencesParams;
  "get-asset-model": GetAssetModelParams;
  "enter-asset-preview": EnterAssetPreviewParams;
  "exit-asset-preview": EmptyParams;
  "set-active-view": SetActiveViewParams;
  "clean-assets": CleanAssetsParams;
  "delete-unused": DeleteUnusedParams;
  "rename-asset": RenameAssetParams;
  "create-asset-folder": CreateAssetFolderParams;
  "rename-asset-folder": RenameAssetFolderParams;
  "delete-asset-folder": DeleteAssetFolderParams;
  "move-asset": MoveAssetParams;
  "asset-usages": AssetUsagesParams;
  "probe-asset": AssetMetadataParams;
  "delete-asset": DeleteAssetParams;
  "assign-asset": AssignAssetParams;
  "material-create": MaterialCreateParams;
  "material-assign": MaterialAssignParams;
  "material-import": MaterialImportParams;
  "material-list": EmptyParams;
  "material-get": MaterialGetParams;
  "material-schema": MaterialSchemaParams;
  "material-update": MaterialUpdateParams;
  "preview-render": PreviewRenderParams;
  "material-set-graph": MaterialSetGraphParams;
  "material-create-instance": MaterialCreateInstanceParams;
  "material-set-override": MaterialSetOverrideParams;
  "material-compile-graph": MaterialCompileParams;
  "material-cook": EmptyParams;
  "save-scene": PathParams;
  "load-scene": PathParams;
  "save-project": OptionalPathParams;
  "load-project": OptionalPathParams;
  "reload-project": EmptyParams;
  "get-stores": EmptyParams;
  "set-stores": ProjectStoresDto;
  "screenshot": ScreenshotParams;
  "get-thumbnail": ThumbnailParams;
  "view-asset": ThumbnailParams;
  "thumbnail-cache": ThumbnailCacheParams;
  "export-app": ExportAppParams;
  "quit": EmptyParams;
}

export interface CommandResultMap {
  "ping": PingResult;
  "render-stats": RenderStatsDto;
  "profiler.set-mode": ProfilerModeResult;
  "pass-timings": RenderPassTimingsDto;
  "profiler.capture-start": CaptureStartResult;
  "profiler.capture-stop": CaptureStopResult;
  "profiler.capture-status": CaptureStatusResult;
  "frame-history": FrameHistoryDto;
  "get-perf-config": PerfConfigDto;
  "set-perf-config": PerfConfigDto;
  "get-upscale": GetUpscaleResult;
  "set-upscale": SetUpscaleResult;
  "drain-alarms": DrainAlarmsResult;
  "list-active-alarms": ActiveAlarmsDto;
  "set-aa": SetAaResult;
  "get-taa-params": GetTaaParamsResult;
  "set-taa-params": SetTaaParamsResult;
  "set-view-mode": SetViewModeResult;
  "set-clustered": SetClusteredResult;
  "set-ibl": SetIblResult;
  "set-sky-occlusion": SetSkyOcclusionResult;
  "set-gdf": SetGdfResult;
  "set-render-quality": RenderQualityResult;
  "get-render-quality": RenderQualityResult;
  "set-tonemap": TonemapResult;
  "set-rt-shadows": SetRtShadowsResult;
  "set-restir": SetRestirResult;
  "set-ssr": SetSsrResult;
  "set-rt-reflections": SetRtReflectionsResult;
  "set-gi": SetGiResult;
  "set-shadows": SetShadowsResult;
  "set-skinning": SetSkinningResult;
  "set-displacement": SetDisplacementResult;
  "set-depth-prepass": SetDepthPrepassResult;
  "viewport-native-info": ViewportNativeInfoResult;
  "set-viewport-power-state": ViewportPowerStateResult;
  "set-viewport-size": SetViewportSizeResult;
  "list-entities": EntityList;
  "list-components": ComponentList;
  "create-entity": EntityRef;
  "destroy-entity": DestroyEntityResult;
  "set-parent": EntityRef;
  "add-component": AddComponentResult;
  "remove-component": RemoveComponentResult;
  "set-component": SetComponentResult;
  "set-component-order": SetComponentOrderResult;
  "set-transform": EntityRef;
  "set-light": EntityRef;
  "select": EntityRef;
  "pick": PickResult;
  "inspect": InspectResult;
  "focus": EntityRef;
  "get-world-transform": WorldTransformResult;
  "get-environment": EnvironmentDto;
  "set-environment": EnvironmentDto;
  "set-atmosphere": EnvironmentDto;
  "get-selection": SelectionResult;
  "deselect": DeselectResult;
  "play": PlayStateResult;
  "pause": PlayStateResult;
  "step": PlayStateResult;
  "stop": PlayStateResult;
  "get-play-state": PlayStateResult;
  "get-animation-state": AnimationStateResult;
  "list-clips": ListClipsResult;
  "play-animation": AnimationStateResult;
  "set-animation-playing": AnimationStateResult;
  "seek-animation": AnimationStateResult;
  "set-animation-loop": AnimationStateResult;
  "stop-preview": AnimationStateResult;
  "get-skeleton-overlay": SkeletonOverlayResult;
  "set-skeleton-overlay": SkeletonOverlayResult;
  "get-debug-overlays": DebugOverlaysResult;
  "set-debug-overlays": DebugOverlaysResult;
  "set-skeleton-highlight": SkeletonOverlayResult;
  "pick-skeleton-joint": PickSkeletonJointResult;
  "set-asset-preview-options": AssetPreviewOptionsResult;
  "get-foot-ik": FootIkResult;
  "set-foot-ik": FootIkResult;
  "set-morph-weights": MorphWeightsResult;
  "get-morph-weights": MorphWeightsResult;
  "list-clip-bindings": ClipBindingsResult;
  "get-script-status": ScriptStatusResult;
  "physics-state": PhysicsStateResult;
  "physics-bodies": PhysicsBodiesResult;
  "fit-collider": FitColliderResult;
  "apply-impulse": ApplyImpulseResult;
  "drain-contacts": DrainContactsResult;
  "set-kinematic-bones": KinematicBonesResult;
  "move-character": MoveCharacterResult;
  "raycast": RaycastResult;
  "shapecast": RaycastResult;
  "enable-ragdoll": RagdollResult;
  "set-ragdoll": RagdollResult;
  "get-ragdoll": RagdollResult;
  "drain-script-errors": DrainScriptErrorsResult;
  "drain-script-logs": DrainScriptLogsResult;
  "get-script-schema": GetScriptSchemaResult;
  "set-script-override": SetScriptOverrideResult;
  "add-entity": EntityRef;
  "copy-entity": EntityRef;
  "rename-entity": EntityRef;
  "set-component-field": SetComponentFieldResult;
  "get-camera": EditorCamera;
  "set-camera": EditorCamera;
  "get-gizmo": GizmoState;
  "set-gizmo": GizmoState;
  "gizmo-pointer": GizmoPointerResult;
  "fly-input": FlyInputResult;
  "script-input": ScriptInputResult;
  "set-probes": SetProbesResult;
  "recapture-probes": RecaptureProbesResult;
  "list-probes": ListProbesResult;
  "set-exposure": SetExposureResult;
  "get-project": ProjectInfoDto;
  "project-status": ProjectStatusDto;
  "cancel-load": ProjectStatusDto;
  "new-project": ProjectStatusDto;
  "create-script": CreateScriptResult;
  "open-project": ProjectStatusDto;
  "import-model": ImportModelResult;
  "instantiate-model": EntityRef;
  "asset-placement": AssetPlacementResult;
  "import-texture": ImportTextureResult;
  "list-assets": AssetList;
  "scan-assets": ScanAssetsResult;
  "extract-subasset": AssetRef;
  "clear-extraction": AssetRef;
  "reimport-model": ReimportModelResult;
  "model-info": ModelInfoResult;
  "asset-references": AssetReferencesResult;
  "get-asset-model": AssetModelResult;
  "enter-asset-preview": AssetPreviewResult;
  "exit-asset-preview": PlayStateResult;
  "set-active-view": SetActiveViewResult;
  "clean-assets": CleanReport;
  "delete-unused": DeleteUnusedResult;
  "rename-asset": AssetRef;
  "create-asset-folder": AssetList;
  "rename-asset-folder": AssetList;
  "delete-asset-folder": AssetList;
  "move-asset": AssetRef;
  "asset-usages": AssetUsagesResult;
  "probe-asset": AssetMetadataDto;
  "delete-asset": DeleteAssetResult;
  "assign-asset": AssignAssetResult;
  "material-create": MaterialCreateResult;
  "material-assign": MaterialAssignResult;
  "material-import": MaterialImportResultDto;
  "material-list": MaterialListResult;
  "material-get": MaterialGetResult;
  "material-schema": MaterialSchemaResult;
  "material-update": MaterialUpdateResult;
  "preview-render": PreviewRenderResult;
  "material-set-graph": MaterialSetGraphResult;
  "material-create-instance": MaterialCreateResult;
  "material-set-override": MaterialSetOverrideResult;
  "material-compile-graph": MaterialCompileResult;
  "material-cook": MaterialCookResult;
  "save-scene": PathResult;
  "load-scene": PathResult;
  "save-project": ProjectInfoDto;
  "load-project": ProjectStatusDto;
  "reload-project": ProjectStatusDto;
  "get-stores": ProjectStoresDto;
  "set-stores": ProjectStoresDto;
  "screenshot": ScreenshotResult;
  "get-thumbnail": ThumbnailResult;
  "view-asset": ThumbnailResult;
  "thumbnail-cache": ThumbnailCacheResult;
  "export-app": ExportAppResult;
  "quit": QuitResult;
}
