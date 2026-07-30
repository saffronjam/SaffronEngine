/**
 * GENERATED - do not edit.
 *
 * Produced by cargo run -p xtask -- gen-protocol.
 */

export type WireUuid = string;

export type ControlFailureDto = { "code": "command", message: string, } | { "code": "params", message: string, } | { "code": "busy-loading", message: string, } | { "code": "invalid-request", message: string, } | { "code": "transport", message: string, } | { "code": "malformed-reply", message: string, } | { "code": "bridge", message: string, } | { "code": "diagnostic", message: string, diagnostic: ControlDiagnosticDto, };

export type ControlDiagnosticDto = { "domain": "vegetation-graph", "detail": VegetationGraphDiagnosticDto } | { "domain": "vegetation-artifact", "detail": VegetationArtifactDiagnosticDto } | { "domain": "reimport-conflict", "detail": ReimportConflictDiagnosticDto };

export type VegetationGraphDiagnosticDto = { "category": "numeric-overflow" } | { "category": "memory-reservation", resource: string, reason: string, } | { "category": "worker-spawn", reason: string, } | { "category": "worker-panicked" } | { "category": "document", path: string, reason: string, } | { "category": "cycle", node: VegetationGuid, } | { "category": "type-mismatch", fromNode: VegetationGuid, fromPin: string, fromDomain: string, toNode: VegetationGuid, toPin: string, toDomain: string, } | { "category": "authority", node: VegetationGuid, reason: string, } | { "category": "unbounded-influence", node: VegetationGuid, } | { "category": "limit", resource: string, requested: string, limit: string, } | { "category": "cancelled" } | { "category": "authoritative-input", node: VegetationGuid, input: string, } | { "category": "gpu-qualification", node: VegetationGuid, profile: string, } | { "category": "gpu-execution", profile: string, reason: string, };

export type VegetationArtifactDiagnosticDto = { "category": "truncated", format: string, } | { "category": "version", format: string, found: number, expected: number, } | { "category": "schema", format: string, } | { "category": "unknown-section", format: string, section: number, } | { "category": "unknown-codec", format: string, codec: number, } | { "category": "duplicate-section", format: string, section: number, } | { "category": "misaligned-section", format: string, section: number, } | { "category": "overlapping-section", format: string, section: number, } | { "category": "hash-mismatch", format: string, subject: string, } | { "category": "format", format: string, field: string, } | { "category": "not-found", kind: string, contentHash: string, } | { "category": "content-address-collision", path: string, } | { "category": "cancelled" } | { "category": "superseded" };

export interface ReimportConflictEntryDto {
  target: VegetationGuid;
  source: VegetationGuid;
  selector: PlantSourceSelectorDto;
  destination: PlantSemanticDestinationDto;
  reason: PlantReimportConflictReasonDto;
}

export interface ReimportConflictDiagnosticDto {
  plant: WireUuid;
  conflicts: ReimportConflictEntryDto[];
}

export type EntitySelector = number | string;

export type AssetSelector = number | string;

export interface EntityRef {
  id: WireUuid;
  name: string;
}

export interface Vec3 {
  x: number;
  y: number;
  z: number;
}

export interface Vec4 {
  x: number;
  y: number;
  z: number;
  w: number;
}

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

export interface VegetationField {
  map: WireUuid;
  enabled: boolean;
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

export interface MaterialSlot {
  material: WireUuid;
  overrides: Record<string, unknown>;
}

export interface MaterialSet {
  slots: MaterialSlot[];
}

export interface ModelInstance {
  modelId: WireUuid;
}

export interface ScriptSlot {
  scriptPath: string;
  overrides: Record<string, unknown>;
}

export interface Script {
  scripts: ScriptSlot[];
}

export type AnimationWrapDto = "once" | "loop" | "pingpong";

export type AnimationTransitionDto = "inertialize" | "crossfade";

export interface AnimationPlayer {
  clip: WireUuid;
  autoplay: boolean;
  speed: number;
  wrap: AnimationWrapDto;
  transitionMode: AnimationTransitionDto;
  loopBlend: number;
}

export type AtmosphereRoleDto = "sun" | "moon";

export interface DirectionalLight {
  atmosphereRole: AtmosphereRoleDto;
  direction: Vec3;
  color: Vec3;
  intensity: number;
  ambient: number;
  volumetricScattering: number;
  castVolumetricShadow: boolean;
}

export interface PointLight {
  color: Vec3;
  intensity: number;
  range: number;
  volumetricScattering: number;
  castVolumetricShadow: boolean;
}

export interface SpotLight {
  direction: Vec3;
  color: Vec3;
  intensity: number;
  range: number;
  innerAngle: number;
  outerAngle: number;
  volumetricScattering: number;
  castVolumetricShadow: boolean;
}

export interface ReflectionProbe {
  influenceRadius: number;
  intensity: number;
  boxProjection: boolean;
  boxExtent: Vec3;
}

export type FogShapeDto = "box" | "sphere";

export interface FogVolume {
  shape: FogShapeDto;
  extents: Vec3;
  radius: number;
  edgeFalloff: number;
  density: number;
  albedo: Vec3;
  emissive: Vec3;
  phaseG: number;
  heightFalloff: number;
  noiseScale: number;
  noiseIntensity: number;
  noiseDetail: number;
  wind: Vec3;
  speed: number;
}

export type WindSourceKindDto = "directional" | "point" | "vortex" | "wake" | "volume";

export interface WindSource {
  kind: WindSourceKindDto;
  strength: number;
  radius: number;
  falloff: number;
  enabled: boolean;
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

export interface Morph {
  weights: number[];
  names: string[];
}

export interface Bone {

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

export type JointDto = "fixed" | "hinge" | "swingtwist" | "free";

export interface BonePhysicsDto {
  shapeHalfExtents: Vec3;
  mass: number;
  joint: JointDto;
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

export type MotionDto = "static" | "kinematic" | "dynamic";

export interface Rigidbody {
  motion: MotionDto;
  mass: number;
  linearDamping: number;
  angularDamping: number;
  gravityFactor: number;
  lockPosition: BVec3;
  lockRotation: BVec3;
  collisionLayer: number;
}

export type ColliderShapeDto = "box" | "sphere" | "capsule" | "convexhull" | "mesh";

export interface PhysicsMaterial {
  friction: number;
  restitution: number;
}

export interface Collider {
  shape: ColliderShapeDto;
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

export interface Components {
  Name?: Name;
  Transform?: Transform;
  Mesh?: Mesh;
  VegetationField?: VegetationField;
  Camera?: Camera;
  MaterialSet?: MaterialSet;
  ModelInstance?: ModelInstance;
  Script?: Script;
  AnimationPlayer?: AnimationPlayer;
  DirectionalLight?: DirectionalLight;
  PointLight?: PointLight;
  SpotLight?: SpotLight;
  ReflectionProbe?: ReflectionProbe;
  FogVolume?: FogVolume;
  WindSource?: WindSource;
  Relationship?: Relationship;
  SkinnedMesh?: SkinnedMesh;
  Morph?: Morph;
  Bone?: Bone;
  FootIk?: FootIk;
  BonePhysics?: BonePhysics;
  Rigidbody?: Rigidbody;
  Collider?: Collider;
  KinematicBones?: KinematicBones;
  CharacterController?: CharacterController;
}

export type ComponentBody = Name | Transform | Mesh | VegetationField | Camera | MaterialSet | ModelInstance | Script | AnimationPlayer | DirectionalLight | PointLight | SpotLight | ReflectionProbe | FogVolume | WindSource | Relationship | SkinnedMesh | Morph | Bone | FootIk | BonePhysics | Rigidbody | Collider | KinematicBones | CharacterController;

export type SkyModeDto = "color" | "texture" | "procedural";

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
  moonDiskAngularRadius: number;
  moonDiskIntensity: number;
  moonEarthshine: number;
  perPixelTransmittance: boolean;
  skyCaptureCadence: number;
}

export interface FogSettingsDto {
  enabled: boolean;
  mode: FogMode;
  quality: FogQuality;
  historyBlend: number;
  neighborhoodClamp: boolean;
  lightClamp: number;
  baseDensity: number;
  scatterAlbedo: number;
  phaseG: number;
  density: number;
  albedo: Vec3;
  height: number;
  heightFalloff: number;
  startDistance: number;
  maxOpacity: number;
  emissive: Vec3;
  directionalColor: Vec3;
  directionalExponent: number;
  layer2Density: number;
  layer2Falloff: number;
  layer2Height: number;
  aerialPerspective: boolean;
  aerialIntensity: number;
}

export interface CloudSettingsDto {
  enabled: boolean;
  coverage: number;
  cloudType: number;
  precipitation: number;
  anvilBias: number;
  layerAltitude: number;
  layerHeight: number;
  baseScale: number;
  detailScale: number;
  detailStrength: number;
  curlStrength: number;
  weatherScale: number;
  weatherOffset: Vec3;
  weatherTexture: WireUuid;
  primarySteps: number;
  lightSteps: number;
  dropletDiameter: number;
  temporalFactor: number;
  castCloudShadows: boolean;
  cloudShadowStrength: number;
  cloudShadowOnSurfaceStrength: number;
}

export interface WindSettingsDto {
  orientation: number;
  speed: number;
  gust: number;
  turbulenceOctaves: number;
  turbulenceRoughness: number;
  gustFrequency: number;
  referenceHeight: number;
  heightExponent: number;
  seed: number;
}

export interface TodCurvePointDto {
  x: number;
  y: number;
}

export interface TodTintSettingsDto {
  master: TodCurvePointDto[];
  red: TodCurvePointDto[];
  green: TodCurvePointDto[];
  blue: TodCurvePointDto[];
}

export interface TimeOfDaySettingsDto {
  enabled: boolean;
  manualOverride: boolean;
  timeOfDay: number;
  year: number;
  month: number;
  day: number;
  latitude: number;
  longitude: number;
  dayLengthSeconds: number;
  exposureCurve: TodCurvePointDto[];
  tintCurve: TodTintSettingsDto;
  coverageCurve: TodCurvePointDto[];
  cloudTypeCurve: TodCurvePointDto[];
}

export type AddEntityPreset = "empty" | "cube" | "plane" | "sphere" | "point-light" | "spot-light" | "directional-light" | "camera" | "reflection-probe" | "fog-volume";

export type PickKind = "billboard" | "mesh" | "vegetation" | "micro-vegetation";

export type GizmoOpDto = "translate" | "rotate" | "scale";

export type GizmoSpaceDto = "world" | "local";

export type GizmoPointerPhase = "hover" | "begin" | "drag" | "end";

export type FogMode = "analytic" | "volumetric";

export type FogQuality = "low" | "medium" | "high";

export type AaModeDto = "off" | "fxaa" | "taa" | "msaa2" | "msaa4" | "msaa8";

export type GiModeDto = "off" | "ddgi";

export type ViewModeDto = "lit" | "unlit" | "wireframe" | "lit-wireframe" | "detail-lighting" | "lighting-only" | "reflections" | "albedo" | "normal" | "roughness" | "metallic" | "emissive" | "depth" | "ambient-occlusion" | "gi" | "light-complexity" | "motion-vectors" | "fog" | "cloud-density" | "shadow-pages";

export type AssetSlotDto = "mesh" | "albedo" | "metallic-roughness" | "normal" | "occlusion" | "emissive" | "height";

export type ScreenshotTargetDto = "viewport" | "window";

export type ThumbnailFormatDto = "png";

export type AssetTypeDto = "mesh" | "texture" | "other" | "animation" | "material" | "model" | "lut" | "environment" | "plant" | "biome" | "vegetation-map";

export type PlantId = string;

export type VegetationGuid = string;

export interface WorldCellDto {
  coordinates: [string, string, string];
  level: number;
}

export interface WorldBoundsDto {
  minTicks: [string, string, string];
  maxTicksExclusive: [string, string, string];
}

export type PlantLifecycleDto = "seed" | "sprout" | "juvenile" | "mature" | "senescent" | "dead" | "stump" | "removed";

export type InteractionPolicyDto = "decorative" | "interactive" | "harvestable" | "structural";

export type FieldChannelKindDto = "altitude" | "slope" | "curvature" | "concavity" | "drainage" | "moisture" | "temperature" | "precipitation" | "sunlight" | "exposure" | "water-distance" | "water-depth" | "signed-blocker" | "spline-distance" | "user";

export interface FieldChannelDto {
  kind: FieldChannelKindDto;
  user?: string;
}

export interface SurfaceAttachmentDto {
  provider: string;
  primitive: string;
  barycentric: [number, number, number];
  revision: string;
}

export interface PlantPointDto {
  id: PlantId;
  owner: WorldCellDto;
  localPosition: [number, number, number];
  orientation: [number, number, number, number];
  scaleBits: [number, number, number];
  bounds: WorldBoundsDto;
  family: WireUuid;
  variation: number;
  lifecycle: PlantLifecycleDto;
  phenotype: number;
  representationClass: number;
  deterministicKey: VegetationGuid;
  candidate: string;
  parent?: PlantId;
  colony?: PlantId;
  ecologyTick: string;
  health: number;
  moisture: number;
  fuel: number;
  phenology: number;
  flags: number;
  interactionPolicy: InteractionPolicyDto;
  provenance: number;
  attachment?: SurfaceAttachmentDto;
  surfaceProjectionBits: [number, number, number];
}

export interface ProvenanceDto {
  map: WireUuid;
  layer: VegetationGuid;
  biome: WireUuid;
  decision: number;
  candidate: string;
  family?: WireUuid;
  plant?: PlantId;
  variation: number;
}

export type ProvenanceDecisionOutcomeDto = "produced" | "retained" | "accepted" | "rejected";

export interface ProvenanceDecisionDto {
  handle: number;
  parents: number[];
  subgraphPath: VegetationGuid[];
  node: VegetationGuid;
  operator: string;
  candidate: string;
  outcome: ProvenanceDecisionOutcomeDto;
}

export type VegetationCandidateRejectionReasonDto = "surface-miss" | "threshold" | "weighted-elimination" | "priority-exclusion" | "competition" | "foreign-owner" | "no-species";

export interface ProvenanceExplanationDto {
  handle: number;
  record: ProvenanceDto;
  decisions: ProvenanceDecisionDto[];
  rejectionReason?: VegetationCandidateRejectionReasonDto;
}

export type VegetationCompileTargetDto = { "scope": "asset", biome: WireUuid, } | { "scope": "instance", map: WireUuid, biomeInstance: VegetationGuid, };

export interface VegetationCompileBiomeParams {
  target: VegetationCompileTargetDto;
}

export interface VegetationGraphEstimateDto {
  candidates: string;
  accepted: string;
  microSamples: string;
  memoryBytes: string;
  transferBytes: string;
}

export interface VegetationGraphLimitsDto {
  workers: number;
  outputCells: string;
  globalStageTiles: string;
  inputTiles: string;
  candidates: string;
  macroPoints: string;
  microSamples: string;
  memoryBytes: string;
  transferBytes: string;
  moduleDepth: number;
  timeMs: string;
}

export interface VegetationGraphDependencyDto {
  kind: string;
  identity: string;
  contentHash: string;
}

export interface VegetationCompileBiomeResult {
  biome: WireUuid;
  biomeInstance?: VegetationGuid;
  graphIdentity: string;
  requiredHaloBits: number;
  estimate: VegetationGraphEstimateDto;
  limits: VegetationGraphLimitsDto;
  dependencies: VegetationGraphDependencyDto[];
}

export type VegetationGraphOperatorDto = "interface-input" | "region-input" | "spline-input" | "species-input" | "community-input" | "explicit-anchors" | "stratified-coverage" | "blue-noise-poisson" | "surface-projection" | "field-sample" | "painted-tile" | "noise" | "gradient" | "curve" | "remap" | "combine" | "clamp" | "distance-field" | "weighted-elimination" | "variable-spacing" | "field-importance" | "cluster-patch-colony" | "spline-follow" | "recursive-companion" | "transform" | "priority-exclusion" | "bounds-overlap" | "competition" | "suitability" | "community-blend" | "succession-input" | "macro-output" | "micro-output" | "diagnostic-output" | "module-call";

export interface VegetationNodeSchemaParams {
  operator?: VegetationGraphOperatorDto;
}

export interface VegetationGraphPinDto {
  name: string;
  domain: string;
  required: boolean;
}

export interface VegetationGraphParameterDto {
  name: string;
  kind: string;
  required: boolean;
}

export interface VegetationNodeSchemaDto {
  operator: VegetationGraphOperatorDto;
  inputs: VegetationGraphPinDto[];
  outputs: VegetationGraphPinDto[];
  parameters: VegetationGraphParameterDto[];
  seedNamespaces: string[];
  slangCompute: boolean;
}

export interface VegetationNodeSchemaResult {
  nodes: VegetationNodeSchemaDto[];
}

export interface VegetationPreflightRegionParams {
  map: WireUuid;
  biomeInstance: VegetationGuid;
  bounds: WorldBoundsDto;
  level: number;
  ecologyTick?: string;
  workers?: number;
}

export type VegetationEvaluationJobStateDto = "prepared" | "running" | "completed" | "cancelled" | "failed";

export interface VegetationEvaluationPreflightDto {
  outputCells: string;
  globalStageTiles: string;
  inputTiles: string;
  retainedInputBytes: string;
  generatedInputBytes: string;
  candidateCount: string;
  acceptedCount: string;
  microSamples: string;
  preflightPeakBytes: string;
  executionPeakBytes: string;
  memoryBytes: string;
  transferBytes: string;
  workerCount: number;
  timeLimitMs: string;
  limits: VegetationGraphLimitsDto;
}

export interface VegetationEvaluationJobDto {
  job: string;
  state: VegetationEvaluationJobStateDto;
  preflight: VegetationEvaluationPreflightDto;
}

export interface VegetationEvaluationJobParams {
  job: string;
}

export type VegetationExecutionDomainDto = "reference-cpu" | "parallel-cpu" | "slang-compute";

export interface VegetationNodeEvaluationDiagnosticDto {
  modulePath: VegetationGuid[];
  node: VegetationGuid;
  operator: VegetationGraphOperatorDto;
  symbol: string;
  inputCandidates: string;
  outputCandidates: string;
  outputBytes: string;
  predictedTransferBytes: string;
  elapsedMicros: string;
  executionDomain: VegetationExecutionDomainDto;
}

export interface VegetationGraphNodeAddressDto {
  modulePath: VegetationGuid[];
  node: VegetationGuid;
}

export type VegetationDiagnosticResultSourceDto = { "kind": "cell", cell: WorldCellDto, } | { "kind": "global-stage", stage: string, owner: WorldCellDto, };

export type VegetationDiagnosticStreamScopeDto = { "kind": "global-snapshot" } | { "kind": "candidate-lineage", lineage: VegetationGuid, };

export interface VegetationDiagnosticCandidateSampleDto {
  source: VegetationDiagnosticResultSourceDto;
  identity: VegetationCandidateIdentityDto;
  owner: WorldCellDto;
  positionTicks: [string, string, string];
  family?: WireUuid;
  variation: number;
  priorityBits: number;
  ecologyTick: string;
}

export interface VegetationDiagnosticScalarSampleDto {
  source: VegetationDiagnosticResultSourceDto;
  candidate: VegetationCandidateIdentityDto;
  valueBits: number;
}

export interface VegetationDiagnosticProvenanceIdDto {
  source: VegetationDiagnosticResultSourceDto;
  handle: number;
}

export interface VegetationDiagnosticRejectionDto {
  candidate: VegetationCandidateIdentityDto;
  reason: VegetationCandidateRejectionReasonDto;
  provenance: VegetationDiagnosticProvenanceIdDto;
}

export interface VegetationNamedDiagnosticStreamDto {
  node: VegetationGraphNodeAddressDto;
  label: string;
  scope: VegetationDiagnosticStreamScopeDto;
  candidateSamples?: VegetationDiagnosticCandidateSampleDto[];
  scalarSamples?: VegetationDiagnosticScalarSampleDto[];
  rejected: VegetationDiagnosticRejectionDto[];
}

export interface VegetationGpuGroupEvaluationDiagnosticDto {
  nodes: VegetationGraphNodeAddressDto[];
  invocationCount: string;
  outputBytes: string;
  transferBytes: string;
  elapsedMicros: string;
}

export interface VegetationEvaluationSummaryDto {
  cells: string;
  globalStages: string;
  globalResidentBytes: string;
  candidates: string;
  accepted: string;
  microTiles: string;
  rejected: string;
  canonicalHash: string;
  nodes: VegetationNodeEvaluationDiagnosticDto[];
  gpuGroups: VegetationGpuGroupEvaluationDiagnosticDto[];
  streams: VegetationNamedDiagnosticStreamDto[];
}

export interface VegetationEvaluationStatusDto {
  job: string;
  state: VegetationEvaluationJobStateDto;
  preflight: VegetationEvaluationPreflightDto;
  summary?: VegetationEvaluationSummaryDto;
  error?: ControlFailureDto;
}

export interface VegetationCandidateIdentityDto {
  node: VegetationGuid;
  nodeAddress: VegetationGuid;
  nodeSemanticRevision: number;
  ordinal: string;
  ancestor: string;
}

export type VegetationExplainSubjectDto = { "kind": "plant", plant: PlantId, } | { "kind": "rejected", candidate: VegetationCandidateIdentityDto, };

export interface VegetationExplainPointParams {
  job: string;
  cell: WorldCellDto;
  subject: VegetationExplainSubjectDto;
}

export type ThinSheetNormalBehaviorDto = "preserve" | "face-forward-back" | "symmetric";

export type CoverageSourceDto = { "kind": "albedo-alpha" } | { "kind": "texture", texture: WireUuid, } | { "kind": "modeled-geometry" };

export type AlphaClassificationDto = "opaque" | "masked" | "transmissive";

export interface CoverageMipMetadataDto {
  referenceCutoff: number;
  sourceExtent: [number, number];
  spatialHashSalt: string;
  classification: AlphaClassificationDto;
  mipHashes: string[];
}

export interface VoxelMaterialMomentsDto {
  occupancy: number;
  albedoMeanBits: [number, number, number];
  roughnessMean: number;
  transmissionMeanBits: [number, number, number];
  thicknessMeanBits: number;
  normalSecondMomentsBits: [number, number, number, number, number, number];
}

export interface OpacityMicromapDerivationDto {
  enabled: boolean;
  maxSubdivision: number;
  transparentThreshold: number;
  opaqueThreshold: number;
}

export interface ThinSheetFoliageParametersDto {
  frontAlbedoResponse: number;
  backAlbedoResponse: number;
  thicknessBits: number;
  absorptionColorBits: [number, number, number];
  transmissionColorBits: [number, number, number];
  roughness: number;
  normalBehavior: ThinSheetNormalBehaviorDto;
  coverageSource: CoverageSourceDto;
  coverage: CoverageMipMetadataDto;
  voxelMoments: VoxelMaterialMomentsDto;
  opacityMicromap: OpacityMicromapDerivationDto;
  energyLimit: number;
}

export type MaterialSurfaceDto = { "model": "standard" } | { "model": "thin-sheet-foliage", parameters: ThinSheetFoliageParametersDto, };

export type PlantSourceKindDto = "imported" | "native";

export type VegetationValidationSeverityDto = "info" | "warning" | "error";

export interface VegetationValidationIssueDto {
  severity: VegetationValidationSeverityDto;
  code: string;
  path: string;
  message: string;
  sourceSelector?: string;
}

export interface VegetationValidationSummaryDto {
  valid: boolean;
  issues: VegetationValidationIssueDto[];
}

export interface VegetationSourceProvenanceDto {
  source: string;
  sourceUri: string;
  licenseId: string;
  licenseUri: string;
  author: string;
  attribution: string;
  requiresAttribution: boolean;
}

export type PlantSourceLocatorDto = { "kind": "asset", asset: WireUuid, } | { "kind": "file", uri: string, };

export type PlantSourceRoleDto = "geometry" | "material" | "skeleton" | "collision" | "navigation";

export type PlantSourceSelectorDto = { "kind": "whole" } | { "kind": "element", id: VegetationGuid, path: string, } | { "kind": "submesh", element: VegetationGuid, index: number, };

export type PlantSemanticDestinationDto = { "kind": "part", id: VegetationGuid, } | { "kind": "spine", id: VegetationGuid, } | { "kind": "material-slot", slot: number, } | { "kind": "collision-proxy", id: VegetationGuid, } | { "kind": "navigation-proxy", id: VegetationGuid, } | { "kind": "phenotype", id: number, };

export type PlantCompileDiagnosticCodeDto = "missing-source" | "duplicate-source" | "empty-selection" | "invalid-geometry" | "missing-material" | "invalid-material" | "invalid-skeleton" | "missing-coverage-uv" | "invalid-leaf-orientation" | "bounds-mismatch" | "limit-exceeded" | "source-changed" | "orphaned-edit";

export interface PlantCompileDiagnosticDto {
  severity: VegetationValidationSeverityDto;
  code: PlantCompileDiagnosticCodeDto;
  source?: VegetationGuid;
  sourceSelector?: PlantSourceSelectorDto;
  path: string;
  message: string;
}

export type PlantReimportConflictReasonDto = "missing-source" | "missing-element";

export interface PlantSourceHashUpdateDto {
  source: VegetationGuid;
  previous: string;
  current: string;
}

export interface PlantCompileStatisticsDto {
  sources: string;
  meshes: string;
  vertices: string;
  indices: string;
  joints: string;
  materials: string;
  rejected: string;
}

export type SourceUnitsDto = "meters" | "centimeters" | "millimeters" | "feet";

export type SourceAxisDto = "positive-x" | "negative-x" | "positive-y" | "negative-y" | "positive-z" | "negative-z";

export type SourceHandednessDto = "right" | "left";

export type SourceWindingDto = "counter-clockwise" | "clockwise";

export type SourceUvOriginDto = "top-left" | "bottom-left";

export type PlantTangentPolicyDto = "require" | "generate-missing" | "regenerate";

export type PlantPivotDto = { "kind": "source-origin" } | { "kind": "bounds-base-center" } | { "kind": "explicit", 
/**
 * Position in source metres, as Q15.16 bits.
 */
positionBits: [number, number, number], } | { "kind": "semantic-part", 
/**
 * The part identity the origin follows.
 */
part: VegetationGuid, };

export interface PlantImportSettingsDto {
  units: SourceUnitsDto;
  upAxis: SourceAxisDto;
  forwardAxis: SourceAxisDto;
  handedness: SourceHandednessDto;
  scaleBits: number;
  pivot: PlantPivotDto;
  winding: SourceWindingDto;
  uvOrigin: SourceUvOriginDto;
  uvScaleBits: [number, number];
  uvOffsetBits: [number, number];
  tangentPolicy: PlantTangentPolicyDto;
}

export interface PlantGraftSourceDto {
  id: VegetationGuid;
  locator: PlantSourceLocatorDto;
  selector: PlantSourceSelectorDto;
  settings: PlantImportSettingsDto;
  provenance: VegetationSourceProvenanceDto;
}

export interface PlantSourceReferenceDto {
  id: VegetationGuid;
  locator: PlantSourceLocatorDto;
  role: PlantSourceRoleDto;
  selector: PlantSourceSelectorDto;
  contentHash: string;
  provenance: VegetationSourceProvenanceDto;
}

export interface PlantAssetSummaryDto {
  id: WireUuid;
  name: string;
  version: number;
  source: PlantSourceKindDto;
  partCount: number;
  phenotypeCount: number;
  materialSlots: WireUuid[];
  validation: VegetationValidationSummaryDto;
  provenance: VegetationSourceProvenanceDto[];
  dependencies: VegetationManifestDependencyDto[];
  latestCook?: VegetationCookStatisticsDto;
}

export type BiomeRoleDto = "root" | "module";

export interface BiomeAssetSummaryDto {
  id: WireUuid;
  name: string;
  version: number;
  role: BiomeRoleDto;
  plantPalette: WireUuid[];
  modules: WireUuid[];
  parameterCount: number;
  graph: Record<string, unknown>;
  validation: VegetationValidationSummaryDto;
  provenance: VegetationSourceProvenanceDto[];
  dependencies: VegetationManifestDependencyDto[];
  latestCook?: VegetationCookStatisticsDto;
}

export interface VegetationMapSummaryDto {
  id: WireUuid;
  name: string;
  version: number;
  generation: string;
  bounds: WorldBoundsDto;
  layerCount: number;
  biomeInstances: VegetationBiomeInstanceRefDto[];
  chunkLevel: number;
  dirtyLayers: VegetationGuid[];
  validation: VegetationValidationSummaryDto;
  provenance: VegetationSourceProvenanceDto[];
  dependencies: VegetationManifestDependencyDto[];
  latestCook?: VegetationCookStatisticsDto;
}

export type VegetationAssetSummaryDto = { "kind": "plant", "asset": PlantAssetSummaryDto } | { "kind": "biome", "asset": BiomeAssetSummaryDto } | { "kind": "vegetation-map", "asset": VegetationMapSummaryDto };

export type LayerCoordinateSpaceDto = "world" | "surface" | "owner-local";

export type FieldBlendOperatorDto = "replace" | "add" | "multiply" | "minimum" | "maximum";

export type InclusionOperatorDto = "include" | "exclude";

export interface SpeciesWeightDto {
  family: WireUuid;
  weight: number;
}

export interface PlantTransformOverrideDto {
  plant: PlantId;
  globalTicks: [string, string, string];
  scaleBits: [number, number, number];
}

export interface PlantStateOverrideDto {
  plant: PlantId;
  health?: number;
  moisture?: number;
  fuel?: number;
  interactionPolicy?: InteractionPolicyDto;
}

export type VegetationLayerOperatorDto = { "kind": "scalar-field", channel: FieldChannelDto, tileSet: VegetationGuid, blend: FieldBlendOperatorDto, weight: number, } | { "kind": "vector-field", channel: FieldChannelDto, tileSet: VegetationGuid, valueBits: [number, number, number], blend: FieldBlendOperatorDto, } | { "kind": "species-weights", weights: Array<SpeciesWeightDto>, } | { "kind": "density", channel: FieldChannelDto, tileSet: VegetationGuid, blend: FieldBlendOperatorDto, weight: number, } | { "kind": "mask", tileSet: VegetationGuid, operation: InclusionOperatorDto, } | { "kind": "volume", bounds: WorldBoundsDto, operation: InclusionOperatorDto, falloffBits: number, } | { "kind": "spline", spline: VegetationGuid, points: Array<[string, string, string]>, radiusBits: number, operation: InclusionOperatorDto, } | { "kind": "anchors", plants: Array<PlantId>, } | { "kind": "pins", plants: Array<PlantId>, } | { "kind": "transform-overrides", overrides: Array<PlantTransformOverrideDto>, } | { "kind": "state-overrides", overrides: Array<PlantStateOverrideDto>, } | { "kind": "blocker", tileSet: VegetationGuid, categories: number, };

export interface VegetationLayerDto {
  id: VegetationGuid;
  name: string;
  coordinateSpace: LayerCoordinateSpaceDto;
  bounds: WorldBoundsDto;
  operator: VegetationLayerOperatorDto;
  dependencies: VegetationGuid[];
  order: number;
  locked: boolean;
  muted: boolean;
  revision: string;
}

export interface VegetationCookVersionSetDto {
  schema: number;
  compiler: number;
  evaluator: number;
  numeric: number;
  simulation: number;
}

export interface VegetationCookPlatformProfileDto {
  target: string;
  contentProfile: string;
  toolchain: string;
  features: string[];
  identity: string;
}

export interface VegetationCookWorkEstimateDto {
  workUnits: string;
  peakMemoryBytes: string;
  inputBytes: string;
  outputBytes: string;
}

export interface VegetationCookWorkActualDto {
  elapsedMicros: string;
  peakMemoryBytes: string;
  inputBytes: string;
  outputBytes: string;
  rejectionCount: string;
  cacheHit: boolean;
}

export interface VegetationCookRejectionTotalDto {
  reason: VegetationCandidateRejectionReasonDto;
  count: string;
}

export interface VegetationCookStatisticsDto {
  nodes: string;
  elapsedMicros: string;
  peakMemoryBytes: string;
  inputBytes: string;
  outputBytes: string;
  cacheHits: string;
  cacheMisses: string;
  publishedCells: string;
  rejections: VegetationCookRejectionTotalDto[];
}

export type VegetationCookNodeAddressDto = { "kind": "plant", family: WireUuid, } | { "kind": "global-stage", map: WireUuid, biomeInstance: VegetationGuid, stage: string, owner: WorldCellDto, } | { "kind": "cell", map: WireUuid, cell: WorldCellDto, };

export type VegetationMapChunkKindDto = "field" | "anchor-override" | "graph-instance" | "layer-metadata" | "editor-metadata";

export type VegetationMapTileKeyDto = { "kind": "global" } | { "kind": "cell", cell: WorldCellDto, };

export interface VegetationMapChunkKeyDto {
  layer: VegetationGuid;
  tile: VegetationMapTileKeyDto;
  kind: VegetationMapChunkKindDto;
}

export type VegetationManifestDependencyAddressDto = { "kind": "source-asset", asset: WireUuid, } | { "kind": "source-file", uri: string, } | { "kind": "material-coverage", material: WireUuid, } | { "kind": "biome-ir", map: WireUuid, instance: VegetationGuid, } | { "kind": "map-manifest", map: WireUuid, } | { "kind": "map-object", map: WireUuid, key: VegetationMapChunkKeyDto, } | { "kind": "surface-provider", provider: string, revision: string, } | { "kind": "surface-tile", provider: string, revision: string, channel: FieldChannelDto | null, bounds: WorldBoundsDto, } | { "kind": "contract", namespace: string, } | { "kind": "node", node: VegetationCookNodeAddressDto, };

export interface VegetationManifestDependencyDto {
  address: VegetationManifestDependencyAddressDto;
  contentHash: string;
  bounds?: WorldBoundsDto;
  haloBits: number;
  ancestorLevel?: number;
}

export interface VegetationSeedNamespaceDto {
  name: string;
  namespace: VegetationGuid;
}

export type VegetationPointColumnTypeDto = "id128" | "world-cell" | "orientation" | "fixed-vec3" | "world-bounds" | "asset-uuid" | "u32" | "u64" | "optional-id128" | "unit" | "surface-projection" | "optional-surface-attachment" | "world-position";

export interface VegetationManifestPointColumnDto {
  id: number;
  name: string;
  elementType: VegetationPointColumnTypeDto;
}

export interface VegetationManifestPlantDto {
  family: WireUuid;
  tags: string[];
  sourceHash: string;
  artifactHash: string;
  localBoundsMinBits: [number, number, number];
  localBoundsMaxBits: [number, number, number];
  variationCount: number;
  phenotypeCount: number;
}

export type VegetationManifestCellDependencyRoleDto = "neighbour" | "halo" | "ancestor" | "global-stage";

export interface VegetationManifestCellDependencyDto {
  cell: WorldCellDto;
  contentHash: string;
  role: VegetationManifestCellDependencyRoleDto;
  haloBits: number;
}

export interface VegetationSpeciesCountDto {
  family: WireUuid;
  macroCount: string;
  microCount: string;
}

export interface VegetationManifestCellSectionDto {
  kind: VegetationCellSectionKindDto;
  version: number;
  codec: VegetationArtifactSectionCodecDto;
  alignment: number;
  storedSize: string;
  decodedSize: string;
  contentHash: string;
}

export interface VegetationManifestCellDto {
  cell: WorldCellDto;
  bounds: WorldBoundsDto;
  artifactHash: string;
  payloadHash: string;
  dependencies: VegetationManifestCellDependencyDto[];
  speciesCounts: VegetationSpeciesCountDto[];
  macroCount: string;
  microCount: string;
  residentMemoryBytes: string;
  storedBytes: string;
  estimate: VegetationCookWorkEstimateDto;
  actual: VegetationCookWorkActualDto;
  sections: VegetationManifestCellSectionDto[];
}

export interface VegetationBaseManifestDto {
  version: number;
  world: WireUuid;
  map: WireUuid;
  mapHash: string;
  versions: VegetationCookVersionSetDto;
  platform: VegetationCookPlatformProfileDto;
  cookGraphHash: string;
  dependencies: VegetationManifestDependencyDto[];
  seedNamespaces: VegetationSeedNamespaceDto[];
  pointSchemaHash: string;
  pointColumns: VegetationManifestPointColumnDto[];
  plants: VegetationManifestPlantDto[];
  cells: VegetationManifestCellDto[];
  identity: string;
}

export type VegetationCookScopeDto = { "kind": "all" } | { "kind": "bounds", bounds: WorldBoundsDto, level: number, } | { "kind": "cells", cells: Array<WorldCellDto>, };

export interface VegetationCookParams {
  map: AssetSelector;
  scope: VegetationCookScopeDto;
  platformProfile?: string;
  workers?: number;
}

export interface VegetationCookJobParams {
  job: string;
}

export type VegetationCookJobStateDto = "queued" | "running" | "completed" | "cancelled" | "superseded" | "failed";

export interface VegetationCookProgressDto {
  completedNodes: string;
  totalNodes: string;
  cacheHits: string;
  publishedCells: string;
  current?: VegetationCookNodeAddressDto;
}

export interface VegetationCookJobDto {
  job: string;
  state: VegetationCookJobStateDto;
  scope: VegetationCookScopeDto;
  progress: VegetationCookProgressDto;
}

export interface VegetationCookStatusDto {
  job: string;
  state: VegetationCookJobStateDto;
  progress: VegetationCookProgressDto;
  statistics?: VegetationCookStatisticsDto;
  manifest?: VegetationBaseManifestDto;
  error?: ControlFailureDto;
}

export interface VegetationManifestParams {
  map: AssetSelector;
  identity?: string;
}

export interface VegetationManifestResult {
  manifest: VegetationBaseManifestDto;
  latestCook?: VegetationCookStatisticsDto;
}

export interface VegetationCellInspectParams {
  map: AssetSelector;
  cell: WorldCellDto;
  manifest?: string;
}

export type VegetationCellSectionKindDto = "macro-points" | "micro-fields" | "provenance" | "rejection-diagnostics" | "surface-attachments" | "surface-dependencies" | "render-references" | "render-bounds" | "collision-inputs" | "navigation-contributions" | "ecology-boundary" | "ecology-checkpoint";

export type VegetationArtifactSectionCodecDto = "raw" | "zstd";

export interface VegetationCellSectionDto {
  kind: VegetationCellSectionKindDto;
  version: number;
  codec: VegetationArtifactSectionCodecDto;
  alignment: number;
  offset: string;
  storedSize: string;
  decodedSize: string;
  contentHash: string;
}

export interface VegetationCellSummaryDto {
  map: WireUuid;
  manifest: string;
  cell: WorldCellDto;
  contentHash: string;
  cookKey: string;
  platformProfile: string;
  payloadHash: string;
  bounds: WorldBoundsDto;
  macroPoints: string;
  microSamples: string;
  sections: VegetationCellSectionDto[];
}

export interface VegetationCellInspectResult {
  cell: VegetationCellSummaryDto;
}

export interface VegetationRuntimeQueryFilterDto {
  families: WireUuid[];
  requiredTags: string[];
  lifecycles: PlantLifecycleDto[];
  interactionPolicies: InteractionPolicyDto[];
}

export type VegetationRuntimeQueryDto = { "kind": "bounds", bounds: WorldBoundsDto, } | { "kind": "radius", centerTicks: [string, string, string], radiusM: number, } | { "kind": "ray", originTicks: [string, string, string], direction: [number, number, number], maxDistanceM: number, } | { "kind": "nearest", positionTicks: [string, string, string], maxDistanceM: number | null, };

export interface VegetationRuntimeQueryParams {
  query: VegetationRuntimeQueryDto;
  filter: VegetationRuntimeQueryFilterDto;
  limit?: number;
}

export interface VegetationRuntimePlantDto {
  plant: PlantId;
  cell: WorldCellDto;
  generation: string;
  ecologyTick: string;
  positionTicks: [string, string, string];
  orientation: [number, number, number, number];
  scaleBits: [number, number, number];
  bounds: WorldBoundsDto;
  family: WireUuid;
  tags: string[];
  lifecycle: PlantLifecycleDto;
  phenotype: number;
  renderedPhenotype: number;
  interactionPolicy: InteractionPolicyDto;
  health: number;
  moisture: number;
  fuel: number;
  provenance?: ProvenanceDto;
}

export interface VegetationRuntimeQueryHitDto {
  plant: VegetationRuntimePlantDto;
  distanceM?: number;
}

export interface VegetationRuntimeQueryResult {
  matches: string;
  truncated: boolean;
  hits: VegetationRuntimeQueryHitDto[];
}

export interface VegetationRuntimePendingCellDto {
  cell: WorldCellDto;
  facets: ResidencyFacetDto[];
  priority: number;
  sourceRevision: string;
}

export interface VegetationRuntimeFacetBytesDto {
  render: string;
  physics: string;
  simulation: string;
  editing: string;
  navigation: string;
  network: string;
}

export type VegetationRuntimeUnavailableReasonDto = "no-project" | "no-enabled-field" | "no-cooked-manifest" | "fault";

export interface VegetationCollisionResidencyDto {
  residentCells: string;
  residentBodies: string;
  createdTotal: string;
  removedTotal: string;
  hullSkippedTotal: string;
  failedFamilies: string;
}

export interface VegetationRuntimeAvailableStatusDto {
  world: WireUuid;
  map: WireUuid;
  manifestIdentity: string;
  persistentStateIdentity: string;
  persistentCells: string;
  persistentPlants: string;
  predictionCount: string;
  sourceCount: string;
  requestedCells: string;
  residentCells: string;
  requestedBytes: VegetationRuntimeFacetBytesDto;
  residentBytes: VegetationRuntimeFacetBytesDto;
  budgets: VegetationRuntimeFacetBytesDto;
  pending: VegetationRuntimePendingCellDto[];
  regenerationCells: WorldCellDto[];
  collision?: VegetationCollisionResidencyDto;
  promotion?: VegetationPromotionReportDto;
}

export type VegetationRuntimeStatusDto = { "state": "unavailable", reason: VegetationRuntimeUnavailableReasonDto, detail: string | null, } | { "state": "available" } & VegetationRuntimeAvailableStatusDto;

export interface VegetationRuntimeCellParams {
  cell: WorldCellDto;
}

export interface VegetationRuntimeCellResult {
  cell: WorldCellDto;
  generation: string;
  manifestIdentity: string;
  residentFacets: ResidencyFacetDto[];
  macroPlants: string;
  microTiles: string;
  disturbanceMasks: string;
}

export interface VegetationRuntimePlantParams {
  plant: PlantId;
}

export type PlantPromotionStateDto = { "state": "bulk" } | { "state": "promoting" } | { "state": "promoted", entity: WireUuid, } | { "state": "demoting", entity: WireUuid, };

export interface VegetationPromotionResult {
  plant: PlantId;
  state: PlantPromotionStateDto;
}

export interface VegetationPromotionReportDto {
  promoted: string;
  promoting: string;
  demoting: string;
  promotedTotal: string;
  demotedTotal: string;
  felledTotal: string;
  failedTotal: string;
  flushedTotal: string;
}

export type NavigationContributionKindDto = "Cost" | "StaticObstacle" | "DynamicObstacle";

export interface VegetationNavigationContributionDto {
  plant: PlantId;
  kind: NavigationContributionKindDto;
  bounds: WorldBoundsDto;
  footprint: [number, number][];
  heightM: number;
  cost: number;
}

export interface VegetationNavigationCellDto {
  cell: WorldCellDto;
  contributions: VegetationNavigationContributionDto[];
}

export interface VegetationNavigationParams {
  drainDirty?: boolean;
}

export interface VegetationNavigationResult {
  cells: VegetationNavigationCellDto[];
  dirtyRegions: WorldBoundsDto[];
  contributions: string;
  obstacles: string;
  dynamicObstacles: string;
  drained: boolean;
}

export type VegetationTransitionKindDto = { "kind": "damaged", amount: number, health: number, } | { "kind": "harvested", phenotype: number, } | { "kind": "burned", phenotype: number, remainingFuel: number, } | { "kind": "removed" } | { "kind": "planted" } | { "kind": "regrew", lifecycle: PlantLifecycleDto, phenotype: number, } | { "kind": "lifecycle-changed", from: PlantLifecycleDto | null, to: PlantLifecycleDto, } | { "kind": "ignited" } | { "kind": "extinguished" } | { "kind": "wetted", moisture: number, fuel: number, } | { "kind": "state-replaced" } | { "kind": "moved" } | { "kind": "disturbed", categories: number, };

export interface VegetationEventDto {
  seq: string;
  transaction: string;
  cell: WorldCellDto;
  plant?: PlantId;
  transition: VegetationTransitionKindDto;
}

export interface VegetationDrainEventsParams {
  since?: string;
}

export interface VegetationDrainEventsResult {
  events: VegetationEventDto[];
  highWaterSeq: string;
  oldestSeq: string;
  overflowed: boolean;
}

export interface VegetationRuntimePlantStateDto {
  cell: WorldCellDto;
  cellRevision: string;
  added: boolean;
  tombstoned: boolean;
  positionTicks?: [string, string, string];
  lifecycle?: PlantLifecycleDto;
  phenotype?: number;
  ecologyTick?: string;
  health?: number;
  moisture?: number;
  fuel?: number;
  interactionPolicy?: InteractionPolicyDto;
  promoted: boolean;
}

export interface VegetationRuntimePlantInspectResult {
  plant: PlantId;
  resident?: VegetationRuntimePlantDto;
  persistent: VegetationRuntimePlantStateDto[];
  promotion?: PlantPromotionStateDto;
}

export interface VegetationStateSnapshotDto {
  manifestIdentity: string;
  contentHash: string;
  bytes: string;
  dataHex: string;
}

export interface VegetationStateImportParams {
  dataHex: string;
}

export type BotanicalElementDto = "trunk" | "branch" | "root" | "vine" | "frond" | "leaf" | "needle" | "blade" | "flower" | "fruit" | "bud" | "scar" | "dead-part";

export type PhyllotaxisPatternDto = "alternate" | "opposite" | "whorled" | "spiral";

export type TropismKindDto = "phototropism" | "gravitropism" | "thigmotropism";

export type PruneRuleDto = "below-height" | "shorter-than" | "keep-strongest";

export interface BotanicalCurvePointDto {
  at: number;
  factorBits: number;
}

export interface BotanicalDrawnPointDto {
  positionBits: [number, number, number];
  radiusBits: number;
}

export type BotanicalEditActionDto = { "kind": "transform", 
/**
 * Translation in family-local metres, as Q15.16 bits.
 */
offsetBits: [number, number, number], 
/**
 * Turn about the target's base, as `UnitInterval` bits.
 */
roll: number, 
/**
 * Uniform scale as Q15.16 bits, where 65536 is unchanged.
 */
scaleBits: number, } | { "kind": "trim", 
/**
 * Where along the axis the cut falls, as `UnitInterval` bits.
 */
at: number, } | { "kind": "remove" } | { "kind": "graft", 
/**
 * The family graft source supplying the geometry.
 */
source: VegetationGuid, 
/**
 * Which of that source's elements to take.
 */
selector: PlantSourceSelectorDto, };

export interface BotanicalManualEditDto {
  target: string;
  action: BotanicalEditActionDto;
}

export type BotanicalEditOrphanReasonDto = "target-missing" | "target-kind" | "target-removed";

export interface BotanicalEditOrphanDto {
  target: string;
  action: BotanicalEditActionDto;
  reason: BotanicalEditOrphanReasonDto;
}

export type BotanicalOperatorDto = { "kind": "drawn", element: BotanicalElementDto, points: Array<BotanicalDrawnPointDto>, } | { "kind": "trunk", element: BotanicalElementDto, lengthBits: number, baseRadiusBits: number, taper: Array<BotanicalCurvePointDto>, segments: number, } | { "kind": "branch", element: BotanicalElementDto, lengthRatio: number, radiusRatio: number, declination: number, jitter: number, segments: number, } | { "kind": "phyllotaxis", pattern: PhyllotaxisPatternDto, count: number, nodes: number, start: number, end: number, divergence: number, } | { "kind": "tropism", kindOf: TropismKindDto, strength: number, } | { "kind": "prune", rule: PruneRuleDto, thresholdBits: number, count: number, } | { "kind": "roots", depthRatio: number, spreadRatio: number, count: number, } | { "kind": "shell", materialSlot: number, sides: number, } | { "kind": "instance", element: BotanicalElementDto, materialSlot: number, sizeBits: number, jitter: number, } | { "kind": "module-call", 
/**
 * Call-site GUID as a canonical 32-hex-digit string.
 */
callGuid: VegetationGuid, } | { "kind": "family" };

export interface BotanicalNodeDto {
  guid: string;
  version: number;
  semanticRevision: number;
  operator: BotanicalOperatorDto;
}

export interface BotanicalEdgeDto {
  fromNode: string;
  fromPin: string;
  toNode: string;
  toPin: string;
}

export interface VegetationPointPrototypeDto {
  name: string;
  family: WireUuid;
}

export interface VegetationStageTimesDto {
  residencyUs: number;
  promotionUs: number;
  collisionUs: number;
  navigationUs: number;
  ecologyUs: number;
  totalUs: number;
}

export interface VegetationWorkCountersDto {
  synchronizations: string;
  queries: string;
  queryHits: string;
  mutations: string;
  mutationBytes: string;
  snapshots: string;
  snapshotBytes: string;
  ecologyTicks: string;
}

export interface VegetationArtifactFaultDto {
  path: string;
  fault: string;
}

export interface UsdSkeletonsParams {
  path: string;
}

export interface UsdSkeletonsResult {
  skeletons: UsdSkeletonDto[];
  unsupported: string[];
}

export interface UsdSkeletonDto {
  name: string;
  skelRoot?: string;
  joints: UsdJointDto[];
}

export interface UsdJointDto {
  path: string;
  parent?: number;
  rest: number[];
  bind: number[];
}

export interface VegetationWindRecordParams {
  cell: WorldCellDto;
  plant: PlantId;
}

export interface VegetationWindRecordResult {
  slot: number;
  swayCurrentM: [number, number, number];
  swayPreviousM: [number, number, number];
  interactionCurrentM: [number, number, number];
  interactionPreviousM: [number, number, number];
  interactionReset: boolean;
  branchQuadrature: [number, number, number, number];
  branchAmplitudeM: number;
  flutterAmplitudeM: number;
  heightScale: number;
  boundsInflationM: number;
  mechanics?: VegetationMechanicsDto;
}

export interface VegetationBudgetsParams {
  cellPlants?: number;
  familyInstances?: number;
  familyMicroPredicted?: string;
}

export interface VegetationBudgetsResult {
  cellPlants: number;
  familyInstances: number;
  familyMicroPredicted: string;
}

export type PhenotypeRoleDto = "healthy" | "harvested" | "damaged" | "burned" | "dead" | "flowering" | "fruiting" | "senescent" | "wet";

export interface PlantPhenotypeDto {
  id: number;
  role: PhenotypeRoleDto;
  variation: number;
  seasonWindow?: [number, number];
  materialRemap: [number, number][];
  activeParts: string[];
}

export interface PlantPhenotypesParams {
  plant: AssetSelector;
  phenotypes?: PlantPhenotypeDto[];
}

export interface PlantPhenotypesResult {
  plant: WireUuid;
  phenotypes: PlantPhenotypeDto[];
}

export type HierarchyCutDto = "auto" | "coarse" | "fine";

export type HierarchyCutViewDto = "camera" | "shadow" | "gi";

export interface SetHierarchyCutParams {
  cut?: HierarchyCutDto;
  view?: HierarchyCutViewDto;
}

export interface HierarchyCutResult {
  view: HierarchyCutViewDto;
  cut: HierarchyCutDto;
}

export interface PlantAtlasPlacementDto {
  slot: number;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface PlantAtlasParams {
  plant: AssetSelector;
  level: number;
}

export interface PlantAtlasResult {
  plant: WireUuid;
  level: number;
  levelCount: number;
  width: number;
  height: number;
  gutter: number;
  placements: PlantAtlasPlacementDto[];
  base64: string;
}

export interface AppearanceErrorDto {
  silhouette: number;
  coverage: number;
  transmission: number;
  material: number;
  normalDistribution: number;
  total: number;
}

export interface PlantHierarchyNodeDto {
  id: number;
  parent?: number;
  depth: number;
  representation: string;
  primitives: number;
  page: number;
  childCount: number;
  appearanceError: AppearanceErrorDto;
}

export interface PlantHierarchyParams {
  plant: AssetSelector;
}

export interface PlantHierarchyResult {
  plant: WireUuid;
  triangleNodes: number;
  voxelNodes: number;
  nodes: PlantHierarchyNodeDto[];
}

export interface PlantSeasonPhenotypeParams {
  plant: AssetSelector;
  seasonMille: number;
  lifecycle?: PlantLifecycleDto;
}

export interface PlantSeasonPhenotypeResult {
  plant: WireUuid;
  phenotype: number;
  variation: number;
}

export type PlantCollisionShapeDto = "box" | "sphere" | "capsule" | "convexHull";

export interface PlantCollisionProxyDto {
  shape: PlantCollisionShapeDto;
  centerM: [number, number, number];
  dimensionsM: [number, number, number];
  breakable: boolean;
}

export interface PlantNavigationProxyDto {
  footprintM: [number, number][];
  heightM: number;
  cost: number;
}

export interface PlantProxiesParams {
  plant: AssetSelector;
}

export interface PlantProxiesResult {
  plant: WireUuid;
  collision: PlantCollisionProxyDto[];
  navigation: PlantNavigationProxyDto[];
}

export interface VegetationMechanicsDto {
  stiffness: number;
  drag: number;
  flutter: number;
  damping: number;
  bendLimit: number;
}

export interface VegetationVerifyParams {
  repair: boolean;
}

export interface VegetationVerifyResult {
  checked: string;
  repaired: string;
  faults: VegetationArtifactFaultDto[];
}

export interface VegetationStateBaselineResult {
  manifestIdentity: string;
  bytes: string;
  cells: string;
}

export interface VegetationCookQueueDto {
  live: string;
  submitted: string;
  completed: string;
  cancelled: string;
  superseded: string;
  failed: string;
  latencyUs: string;
}

export interface VegetationTelemetryResult {
  last: VegetationStageTimesDto;
  average: VegetationStageTimesDto;
  work: VegetationWorkCountersDto;
  residentBytes: VegetationRuntimeFacetBytesDto;
  collisionBodies: string;
  navigationContributions: string;
  promoted: string;
  cookQueue: VegetationCookQueueDto;
}

export interface VegetationImportPointsParams {
  map: AssetSelector;
  layer: VegetationGuid;
  path: string;
  prototypes: VegetationPointPrototypeDto[];
  expectedGeneration: string;
}

export interface VegetationImportPointsResult {
  anchors: number;
  tiles: number;
  prototypes: number;
  unsupported: string[];
  generation: string;
}

export interface VegetationExportPointsParams {
  map: AssetSelector;
  layer: VegetationGuid;
  path: string;
}

export interface VegetationExportPointsResult {
  instances: number;
  prototypes: number;
  path: string;
}

export interface BotanicalVariationDto {
  seed: string;
  age: number;
  name: string;
}

export interface BotanicalGraphDto {
  variations: BotanicalVariationDto[];
  nodes: BotanicalNodeDto[];
  edges: BotanicalEdgeDto[];
  edits: BotanicalManualEditDto[];
}

export interface PlantGraphSetParams {
  plant: AssetSelector;
  graph: BotanicalGraphDto;
  grafts: PlantGraftSourceDto[];
}

export interface PlantGraphResult {
  plant: WireUuid;
  graph: BotanicalGraphDto;
  grafts: PlantGraftSourceDto[];
  growth: BotanicalGrowthDto;
}

export interface PlantCreateParams {
  name: string;
  folder: string;
  seed: string;
  materials: WireUuid[];
}

export interface BotanicalGrowthDto {
  graph: string;
  variation: number;
  seed: string;
  age: number;
  variations: number;
  axes: number;
  frames: number;
  shells: number;
  elements: number;
  vertices: number;
  triangles: number;
  truncated: boolean;
  parts: number;
  spines: number;
  heightBits: number;
  grafts: number;
  appliedEdits: number;
  orphans: BotanicalEditOrphanDto[];
}

export interface PlantCreateResult {
  plant: WireUuid;
  growth: BotanicalGrowthDto;
}

export interface PlantGrowthParams {
  plant: AssetSelector;
  variation: number;
  maxAxes?: number;
  maxElements?: number;
}

export interface BotanicalAxisDto {
  id: string;
  parent?: string;
  frame?: string;
  element: BotanicalElementDto;
  baseBits: [number, number, number];
  tipBits: [number, number, number];
  baseRadiusBits: number;
  points: number;
}

export interface BotanicalPlacementDto {
  id: string;
  frame: string;
  element: BotanicalElementDto;
  materialSlot: number;
  positionBits: [number, number, number];
  sizeBits: number;
  roll: number;
}

export interface PlantElementsResult {
  plant: WireUuid;
  axes: BotanicalAxisDto[];
  elements: BotanicalPlacementDto[];
}

export interface VegetationAdvanceEcologyParams {
  targetTick: string;
  maxTicks: number;
}

export interface VegetationEcologyClockParams {
  running?: boolean;
  tickMilliseconds?: number;
  maxTicksPerSync?: number;
  workers?: number;
  water?: number;
  warmth?: number;
}

export interface VegetationEcologyClockDto {
  running: boolean;
  tickMilliseconds: number;
  pendingMilliseconds: string;
  maxTicksPerSync: number;
  workers: number;
  water: number;
  warmth: number;
  ticksOwed: string;
}

export interface VegetationEcologyRegionDto {
  cells: WorldCellDto[];
  tick: string;
  caughtUp: boolean;
  resident: boolean;
}

export interface VegetationEcologyCellDto {
  cell: WorldCellDto;
  tick: string;
  plants: number;
  canopy: number;
  health: number;
  moisture: number;
  fuel: number;
}

export interface VegetationEcologyStatusDto {
  worldTick: string;
  simulationVersion: number;
  checkpoint: string;
  regionRadiusCells: number;
  clock: VegetationEcologyClockDto;
  regions: VegetationEcologyRegionDto[];
  cells: VegetationEcologyCellDto[];
}

export interface VegetationEcologyReportDto {
  worldTick: string;
  regions: number;
  regionsCaughtUp: number;
  regionsAwaitingResidency: number;
  ticksRun: string;
  ticksOwed: string;
  ticksAwaitingResidency: string;
  workers: number;
  checkpoint: string;
}

export interface VegetationCombustionParams {
  bounds: WorldBoundsDto;
  filter: VegetationRuntimeQueryFilterDto;
}

export interface VegetationCombustionDto {
  plants: number;
  ignited: number;
  fuel: number;
  moisture: number;
  health: number;
  occupancy: number;
}

export interface PlantValidateParams {
  plant: AssetSelector;
}

export interface PlantValidationResult {
  plant: WireUuid;
  validation: VegetationValidationSummaryDto;
  diagnostics: PlantCompileDiagnosticDto[];
  sources: PlantSourceReferenceDto[];
  dependencies: VegetationManifestDependencyDto[];
  conflicts: ReimportConflictEntryDto[];
  sourceUpdates: PlantSourceHashUpdateDto[];
  familyHash?: string;
  statistics: PlantCompileStatisticsDto;
}

export interface PlantRecookParams {
  plant: AssetSelector;
  platformProfile?: string;
}

export interface PlantRecookResult {
  plant: WireUuid;
  familyHash: string;
  artifactHash: string;
  cacheHit: boolean;
  validation: VegetationValidationSummaryDto;
  diagnostics: PlantCompileDiagnosticDto[];
  sources: PlantSourceReferenceDto[];
  dependencies: VegetationManifestDependencyDto[];
  sourceUpdates: PlantSourceHashUpdateDto[];
  statistics: PlantCompileStatisticsDto;
}

export interface VegetationMutationHeaderDto {
  cell: WorldCellDto;
  transaction: VegetationGuid;
  authority: VegetationGuid;
  logicalTick: string;
  idempotencyKey: VegetationGuid;
  baseRevision?: string;
}

export interface PlantTransformDto {
  globalTicks: [string, string, string];
  orientation: [number, number, number, number];
  scaleBits: [number, number, number];
}

export type VegetationMutationDto = { "kind": "field-tile-patch", layer: VegetationGuid, channel: FieldChannelDto, tile: VegetationGuid, dimensions: [number, number, number], quantumBits: number, values: Array<number>, } | { "kind": "anchor-addition", point: PlantPointDto, } | { "kind": "tombstone", plant: PlantId, } | { "kind": "transform-override", plant: PlantId, transform: PlantTransformDto, } | { "kind": "state-override", plant: PlantId, lifecycle: PlantLifecycleDto | null, phenotype: number | null, health: number | null, moisture: number | null, fuel: number | null, interactionPolicy: InteractionPolicyDto | null, } | { "kind": "planting", point: PlantPointDto, } | { "kind": "damage", plant: PlantId, amount: number, phenotype: number | null, } | { "kind": "moisture-fuel", plant: PlantId, moisture: number, fuel: number, } | { "kind": "lifecycle-transition", plant: PlantId, from: PlantLifecycleDto | null, to: PlantLifecycleDto, ecologyTick: string, } | { "kind": "harvest", plant: PlantId, phenotype: number, } | { "kind": "burn", plant: PlantId, phenotype: number, remainingFuel: number, } | { "kind": "ignite", plant: PlantId, } | { "kind": "extinguish", plant: PlantId, } | { "kind": "regrow", plant: PlantId, lifecycle: PlantLifecycleDto, phenotype: number, ecologyTick: string, } | { "kind": "promotion-origin-state", plant: PlantId, transform: PlantTransformDto, linearVelocityBits: [number, number, number], angularVelocityBits: [number, number, number], } | { "kind": "disturbance-mask", categories: number, tile: VegetationGuid, values: Array<number>, };

export interface VegetationMutationRecordDto {
  header: VegetationMutationHeaderDto;
  mutation: VegetationMutationDto;
}

export interface VegetationAssetSummaryParams {
  asset: AssetSelector;
}

export interface VegetationAssetSummaryResult {
  type: AssetTypeDto;
  summary: VegetationAssetSummaryDto;
  layers: VegetationLayerDto[];
}

export interface ImportVegetationAssetParams {
  path: string;
  folder?: string;
}

export interface ImportVegetationAssetResult {
  id: WireUuid;
  name: string;
  type: AssetTypeDto;
}

export interface PointExtensionColumnDto {
  id: number;
  elementType: string;
  stride: number;
  bytes: number[];
}

export interface VegetationGestureMetadataDto {
  gesture: VegetationGuid;
  presentation: Record<string, unknown>;
}

export type ProfilerModeDto = "off" | "timestamps" | "pipeline-stats";

export type ProfileLaneDto = "cpu" | "gpu";

export type CaptureModeDto = "single" | "frames" | "rolling";

export type CaptureStateDto = "idle" | "arming" | "recording" | "ready";

export type AlarmSeverityDto = "info" | "warning" | "critical";

export type AlarmStateDto = "firing" | "resolved";

export interface PingParams {

}

export interface EmptyParams {

}

export interface PingResult {
  pong: boolean;
  engine: string;
  version: string;
  pid: number;
}

export interface VsmStatsDto {
  requested: number;
  hits: number;
  allocated: number;
  rendered: number;
  dirtied: number;
  evicted: number;
  overflow: number;
}

export interface RenderStatsDto {
  drawCalls: number;
  batches: number;
  instances: number;
  sceneGatherMs: number;
  sceneGatherEntities: number;
  instanceUploadBytes: number;
  retainedMeshCpuBytes: number;
  shadowDrawCalls: number;
  vsm: VsmStatsDto;
  rtInstances: number;
  rtAggregateInstances: number;
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
  profilerMode: ProfilerModeDto;
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
  meshShader: boolean;
  meshExecutor: boolean;
  sdfInstancesDropped: number;
  sdfInstancesCulled: number;
  rtInstancesCulled: number;
  ommSupported: boolean;
  blasCount: number;
  skinnedBlasCount: number;
  tessellatedBlasCount: number;
  clusterAsSupported: boolean;
  clusterBlasCount: number;
  clasCount: number;
  ptlasSupported: boolean;
  ptlasPartitions: number;
  ptlasWrites: number;
  ptlasUpdates: number;
  accelBuildUs: string;
  ommMicromaps: number;
  ommOpaque: string;
  ommTransparent: string;
  ommUnknown: string;
  blasBytes: string;
  blasBuiltBytes: string;
  tlasBytes: string;
  rtScratchBytes: string;
  pipelines: number;
  bindlessTextures: number;
  bindlessFree: number;
  hdr: boolean;
  exposureEv: number;
  colorGrading: SetColorGradingParams;
  creativeLut?: CreativeLutStat;
  bloomEnabled: boolean;
  bloomIntensity: number;
  bloomScatter: number;
  bloomTint: [number, number, number];
  bloomThreshold: number;
  bloomDirtTexture: WireUuid;
  bloomDirtIntensity: number;
  bloomDirtTint: [number, number, number];
  bloomAnamorphic: AnamorphicParams;
  bloomPerMipTint: [number, number, number][];
  aa: AaModeDto;
  viewMode: ViewModeDto;
}

export interface GpuSceneMirrorStatsDto {
  historyInvalidation: string;
  meshes: number;
  materials: number;
  textures: number;
  instances: number;
  lights: number;
  unresolvedInstances: number;
  retainedMeshBytes: number;
  sharedRebuilds: number;
  worldRebuilds: number;
  microPredicted: number;
  pageResidency: PageResidencyStatsDto;
  visibility: SceneVisibilityStatsDto;
}

export interface VegetationRenderStatsDto {
  families: VegetationFamilyRenderDto[];
  cells: VegetationCellRenderDto[];
  pageFaults: string;
}

export interface VegetationMapLayerCommitParams {
  map: AssetSelector;
  expectedGeneration: string;
  upserts: VegetationLayerDto[];
  removals: VegetationGuid[];
}

export interface VegetationMapChunkCommitParams {
  map: AssetSelector;
  expectedGeneration: string;
  upserts: VegetationMapChunkDto[];
  removals: VegetationMapChunkKeyDto[];
}

export interface VegetationMapChunkCommitResult {
  generation: string;
}

export interface VegetationMapChunkReadParams {
  map: AssetSelector;
  keys: VegetationMapChunkKeyDto[];
}

export interface VegetationMapChunkReadResult {
  generation: string;
  chunks: VegetationMapChunkDto[];
}

export interface VegetationBiomeInstanceRefDto {
  instance: VegetationGuid;
  biome: WireUuid;
}

export interface VegetationRejectionsParams {
  map: AssetSelector;
  cell: WorldCellDto;
  manifest?: string;
  limit?: number;
}

export interface VegetationRejectionDto {
  reason: VegetationCandidateRejectionReasonDto;
  positionTicks: [string, string, string];
  ordinal: string;
}

export interface VegetationRejectionsResult {
  candidates: string;
  accepted: string;
  totalRejected: string;
  rows: VegetationRejectionDto[];
}

export interface VegetationTopologyDiffParams {
  map: AssetSelector;
  from: string;
  to?: string;
  cells?: WorldCellDto[];
}

export interface VegetationOverrideConflictDto {
  kind: string;
  plant: PlantId;
  layer: VegetationGuid;
}

export interface VegetationTopologyCellDiffDto {
  cell: WorldCellDto;
  added: string;
  removed: string;
  moved: string;
  addedIds: PlantId[];
  removedIds: PlantId[];
  movedIds: PlantId[];
  conflicts: VegetationOverrideConflictDto[];
}

export interface VegetationTopologyDiffResult {
  from: string;
  to: string;
  cells: VegetationTopologyCellDiffDto[];
}

export interface VegetationMapChunkDto {
  key: VegetationMapChunkKeyDto;
  revision: string;
  payload: VegetationMapChunkPayloadDto;
}

export type VegetationMapChunkPayloadDto = { "kind": "field", fields: Array<AuthoredFieldTileDto>, blockers: Array<AuthoredFieldTileDto>, } | { "kind": "anchor-override", explicitPlants: Array<ExplicitPlantAnchorDto>, pins: Array<PlantId>, transformOverrides: Array<PlantTransformOverrideDto>, stateOverrides: Array<PlantStateOverrideDto>, };

export interface AuthoredFieldTileDto {
  channel: FieldChannelDto;
  layer: VegetationGuid;
  dimensions: [number, number, number];
  quantumBits: number;
  values: number[];
}

export interface ExplicitPlantAnchorDto {
  id: PlantId;
  layer: VegetationGuid;
  family: WireUuid;
  point: PlantPointDto;
}

export interface VegetationMapLayerCommitResult {
  generation: string;
}

export interface VegetationMutateParams {
  records: VegetationMutationRecordDto[];
}

export interface VegetationMutateResult {
  applied: number;
}

export interface VegetationFamilyRenderDto {
  family: WireUuid;
  instances: number;
  fieldTiles: number;
  microPredicted: number;
}

export interface VegetationCellRenderDto {
  cell: WorldCellDto;
  plants: number;
  fieldTiles: number;
}

export interface PageResidencyStatsDto {
  registered: number;
  resident: number;
  residentBytes: number;
  budgetBytes: number;
  requested: number;
  loading: number;
  ready: number;
  evictions: number;
  faults: number;
  faultLatencyUs: number;
  requestsDropped: number;
  requestOverflowClasses: number;
}

export interface SceneVisibilityStatsDto {
  visible: number;
  retested: number;
  records: number;
  transparent: number;
  microCandidates: number;
  transitioning: number;
  voxelRecords: number;
  maxCutDepth: number;
  culledFrustum: number;
  culledOcclusion: number;
  subQuadTriangles: number;
  visitedNodes: number;
  bins: number;
  deformed: number;
  interactionResets: number;
  coveredSamples: number;
  culledNodes: number;
  giReachVisible: number;
  giReachCulled: number;
  overflowFlags: number;
  pressureFlags: number;
}

export interface RenderPassTimingDto {
  name: string;
  gpuMs: number;
}

export interface RenderPassTimingsDto {
  passes: RenderPassTimingDto[];
  gpuTotalMs: number;
  softwareGpu: boolean;
  profilerMode: ProfilerModeDto;
}

export interface ProfilerSetModeParams {
  mode?: ProfilerModeDto;
}

export interface ProfilerModeResult {
  mode: ProfilerModeDto;
  timestampsSupported: boolean;
  pipelineStatsSupported: boolean;
  softwareGpu: boolean;
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

export interface ProfileSpanDto {
  name: string;
  lane: ProfileLaneDto;
  startNs: number;
  endNs: number;
  parentIndex: number;
  depth: number;
  pipelineStats?: PipelineStatsDto;
}

export interface ProfileCaptureMetadataDto {
  softwareGpu: boolean;
  correlated: boolean;
  deviceName: string;
  timestampPeriod: number;
  targetFps: number;
  mode: ProfilerModeDto;
  filter: string;
  frameCount: number;
}

export interface ProfileCaptureDto {
  spans: ProfileSpanDto[];
  metadata: ProfileCaptureMetadataDto;
}

export interface CaptureStartParams {
  mode?: CaptureModeDto;
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
  mode: CaptureModeDto;
  frameCount: number;
  inlined: boolean;
  capture: ProfileCaptureDto;
  chromeTrace: string;
  path: string;
  pending: boolean;
}

export interface CaptureStatusResult {
  state: CaptureStateDto;
  capturedFrames: number;
  targetFrames: number;
  mode: CaptureModeDto;
  pipelineStatsSupported: boolean;
}

export interface FrameSampleDto {
  frameIndex: number;
  cpuMs: number;
  gpuMs: number;
  cpuWaitMs: number;
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

export interface UpscaleDto {
  ratio: number;
  dynamic: boolean;
  targetMs: number;
  inputWidth: number;
  inputHeight: number;
  displayWidth: number;
  displayHeight: number;
}

export interface GetUpscaleResult {
  upscale: UpscaleDto;
}

export interface SetUpscaleParams {
  ratio?: number;
  dynamic?: boolean;
  targetMs?: number;
}

export interface SetUpscaleResult {
  upscale: UpscaleDto;
}

export interface AlarmEventDto {
  seq: number;
  fingerprint: string;
  metric: string;
  pass: string;
  owner: string;
  severity: AlarmSeverityDto;
  state: AlarmStateDto;
  value: number;
  threshold: number;
  sinceFrame: number;
  count: number;
  durationMs: number;
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

export interface FitColliderParams {
  entity: EntitySelector;
}

export interface FitColliderResult {
  entity: WireUuid;
  shape: string;
  halfExtents: Vec3;
  offset: Vec3;
}

export interface ContactEventDto {
  seq: number;
  kind: string;
  targetA?: WorldHitTargetDto;
  targetB?: WorldHitTargetDto;
  sensor: boolean;
  point: Vec3;
  normal: Vec3;
  tick: number;
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

export interface PhysicsBodyDto {
  target?: WorldHitTargetDto;
  motion: string;
  active: boolean;
  position: Vec3;
}

export interface PhysicsBodiesResult {
  bodies: PhysicsBodyDto[];
}

export interface ApplyImpulseParams {
  entity: EntitySelector;
  impulse: Vec3;
}

export interface ApplyImpulseResult {
  velocity: Vec3;
}

export interface SetKinematicBonesParams {
  entity: EntitySelector;
  enabled?: boolean;
}

export interface KinematicBonesResult {
  entity: WireUuid;
  enabled: boolean;
  boneCount: number;
}

export interface MoveCharacterParams {
  entity: EntitySelector;
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

export interface ShapecastParams {
  origin: Vec3;
  dir: Vec3;
  radius: number;
  maxDist?: number;
}

export type WorldHitTargetDto = { "kind": "scene-entity", 
/**
 * The entity's stable uuid.
 */
id: WireUuid, } | { "kind": "vegetation", 
/**
 * The plant's canonical 32-hex identity.
 */
plant: PlantId, };

export interface RaycastResult {
  hit: boolean;
  target?: WorldHitTargetDto;
  point: Vec3;
  normal: Vec3;
  distance: number;
}

export interface EnableRagdollParams {
  entity: EntitySelector;
  enabled?: boolean;
}

export interface RagdollResult {
  present: boolean;
  active: boolean;
  bodyWeight: number;
  bones: number;
}

export interface SetRagdollParams {
  entity: EntitySelector;
  active?: boolean;
  bodyWeight?: number;
  bone?: number;
  weight?: number;
}

export interface GetRagdollParams {
  entity: EntitySelector;
}

export interface ScriptErrorDto {
  seq: number;
  entity: WireUuid;
  script: string;
  message: string;
  tick: number;
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

export interface ScriptLogDto {
  seq: number;
  entity: WireUuid;
  message: string;
  epochMs: number;
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

export interface GetScriptSchemaParams {
  path: string;
}

export interface ScriptFieldDto {
  name: string;
  type: string;
  defaultValue: unknown;
}

export interface GetScriptSchemaResult {
  fields: ScriptFieldDto[];
}

export interface SetScriptOverrideParams {
  entity: EntitySelector;
  slot: number;
  name: string;
  value: unknown;
}

export interface SetScriptOverrideResult {
  scriptPath: string;
  overrides: unknown;
}

export interface CreateScriptParams {
  name: string;
}

export interface CreateScriptResult {
  path: string;
}

export interface ActiveAlarmDto {
  fingerprint: string;
  metric: string;
  pass: string;
  owner: string;
  severity: AlarmSeverityDto;
  value: number;
  threshold: number;
  sinceFrame: number;
  count: number;
}

export interface ActiveAlarmsDto {
  alarms: ActiveAlarmDto[];
}

export interface SetAaParams {
  mode?: AaModeDto;
}

export interface SetAaResult {
  aa: AaModeDto;
}

export interface TaaParamsDto {
  feedbackMin: number;
  feedbackMax: number;
  velocityRejection: number;
  clipGamma: number;
  sharpness: number;
}

export interface GetTaaParamsResult {
  params: TaaParamsDto;
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
  mode?: ViewModeDto;
}

export interface SetViewModeResult {
  viewMode: ViewModeDto;
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

export interface VsmPageBudgetParams {
  pages?: number;
}

export interface VsmPageBudgetResult {
  pages: number;
}

export interface PageRequestBudgetParams {
  entries?: number;
}

export interface PageRequestBudgetResult {
  entries: number;
  capacity: number;
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
  mode: GiModeDto;
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

export interface SetActiveViewParams {
  view: string;
}

export interface SetActiveViewResult {
  view: string;
}

export interface ProjectInfoDto {
  loaded: boolean;
  root: string;
  path: string;
  name: string;
  displayName: string;
}

export type ProjectPhaseDto = "unloaded" | "loading" | "ready" | "failed";

export type BootStageDto = "manifest" | "catalog" | "scene" | "install" | "assets" | "skybox" | "accel" | "ready" | "failed";

export interface ProjectStatusDto {
  phase: ProjectPhaseDto;
  stage: BootStageDto;
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

export interface PathParams {
  path: string;
}

export interface ProjectStoresDto {
  enabled: string[];
}

export interface OptionalPathParams {
  path?: string;
}

export interface AssetAttributionDto {
  licenseId: string;
  requiresAttribution: boolean;
  licenseUrl: string;
  author: string;
  sourceUrl: string;
  storeId: string;
}

export interface ImportModelParams {
  path: string;
  attribution?: AssetAttributionDto;
}

export interface ImportModelResult {
  id: WireUuid;
  name: string;
  type: string;
}

export interface InstantiateModelParams {
  asset: AssetSelector;
  name?: string;
}

export type AssetPlacementPhaseDto = "preview" | "commit" | "clear";

export interface AssetPlacementParams {
  phase: AssetPlacementPhaseDto;
  asset?: AssetSelector;
  u?: number;
  v?: number;
}

export interface PlacementTransformDto {
  translation: Vec3;
  rotation: Vec3;
  scale: Vec3;
}

export interface AssetPlacementResult {
  active: boolean;
  valid: boolean;
  transform?: PlacementTransformDto;
  entity?: EntityRef;
  reason?: string;
}

export interface ExtractSubAssetParams {
  asset: AssetSelector;
  subAsset: WireUuid;
  dest?: string;
}

export interface ClearExtractionParams {
  asset: AssetSelector;
  subAsset: WireUuid;
}

export interface ImportTextureParams {
  path: string;
  colorspace?: string;
  role?: string;
}

export interface ImportTextureResult {
  texture: WireUuid;
}

export interface ImportLutParams {
  path: string;
}

export interface ImportLutResult {
  lut: WireUuid;
}

export interface AssetEntryDto {
  id: WireUuid;
  name: string;
  type: AssetTypeDto;
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

export interface AssetList {
  assets: AssetEntryDto[];
  folders: string[];
}

export interface ScanAssetsResult {
  added: number;
  removed: number;
}

export interface ReimportModelResult {
  updated: number;
  added: number;
  removedFromSource: number;
  skipped: boolean;
}

export interface ReimportModelParams {
  asset: AssetSelector;
}

export interface ModelInfoParams {
  asset: AssetSelector;
}

export interface ModelSubAssetDto {
  id: WireUuid;
  name: string;
  type: string;
  bytes: number;
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

export interface AssetReferencesParams {
  asset: AssetSelector;
}

export interface AssetReferencesResult {
  referencedBy: string[];
  references: string[];
  footprint: number;
}

export interface CleanCandidateDto {
  id: WireUuid;
  path: string;
  category: string;
  bytes: number;
  reason: string;
}

export interface CleanReport {
  candidates: CleanCandidateDto[];
  reclaimableBytes: number;
}

export interface CleanAssetsParams {
  dryRun?: boolean;
  exclude?: string[];
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
  asset: AssetSelector;
  name: string;
}

export interface AssetRef {
  id: WireUuid;
  name: string;
  folder?: string;
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
  asset: AssetSelector;
  folder?: string;
}

export interface AssetUsagesParams {
  asset: AssetSelector;
}

export interface AssetUsageDto {
  entity?: WireUuid;
  entityName?: string;
  slot: string;
}

export interface AssetUsagesResult {
  usages: AssetUsageDto[];
}

export interface AssetMetadataParams {
  asset: AssetSelector;
}

export interface AssetMetadataDto {
  id: WireUuid;
  name: string;
  type: AssetTypeDto;
  path: string;
  folder?: string;
  sizeBytes: number;
  vertexCount?: number;
  triangleCount?: number;
  createdAt: number;
}

export interface DeleteAssetParams {
  asset: AssetSelector;
}

export interface DeleteAssetResult {
  id: WireUuid;
  name: string;
  cleared: AssetUsageDto[];
  fileDeleted: boolean;
}

export interface AssignAssetParams {
  entity: EntitySelector;
  slot: AssetSlotDto;
  asset: AssetSelector;
}

export interface MaterialCreateParams {
  name: string;
}

export interface MaterialCreateResult {
  id: WireUuid;
  name: string;
}

export interface MaterialAssignParams {
  entity: EntitySelector;
  material: AssetSelector;
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

export interface MaterialRefDto {
  id: WireUuid;
  name: string;
  folder: string;
}

export interface MaterialListResult {
  materials: MaterialRefDto[];
}

export interface MaterialGetParams {
  material: AssetSelector;
}

export interface MaterialGetResult {
  id: WireUuid;
  surface: MaterialSurfaceDto;
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

export interface MaterialSchemaParams {
  material: AssetSelector;
}

export interface ExposedParamDto {
  name: string;
  kind: string;
  default: unknown;
}

export interface MaterialSchemaResult {
  params: ExposedParamDto[];
}

export interface MaterialUpdateParams {
  material: AssetSelector;
  surface?: MaterialSurfaceDto;
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
  material: AssetSelector;
  size?: number;
}

export interface PreviewRenderResult {
  png: string;
}

export interface MaterialSetGraphParams {
  material: AssetSelector;
  graph: unknown;
}

export interface MaterialSetGraphResult {
  id: WireUuid;
  foldable: boolean;
}

export interface MaterialCreateInstanceParams {
  parent: AssetSelector;
  name: string;
}

export interface MaterialSetOverrideParams {
  material: AssetSelector;
  field: string;
  value: unknown;
}

export interface MaterialSetOverrideResult {
  id: WireUuid;
}

export interface MaterialCompileParams {
  material: AssetSelector;
}

export interface MaterialCompileResult {
  id: WireUuid;
  ok: boolean;
}

export interface MaterialCookResult {
  compiled: number;
  failed: number;
}

export interface AppManifest {
  title: string;
  width: number;
  height: number;
  fullscreen: boolean;
  vsync: boolean;
}

export interface ExportAppParams {
  outputDir: string;
  app: AppManifest;
}

export interface ExportVegetationFacetDto {
  facet: string;
  cells: string;
  bytes: string;
}

export interface ExportVegetationMapDto {
  map: WireUuid;
  manifestIdentity: string;
  plants: string;
  cells: string;
  missing: string;
  baseline: boolean;
  macroPlants: string;
  facets: ExportVegetationFacetDto[];
}

export interface ExportAppResult {
  path: string;
  warnings: string[];
  vegetation: ExportVegetationMapDto[];
  vegetationBytes: string;
  attributions: number;
}

export interface AssignAssetResult {
  id: WireUuid;
  name: string;
  slot: AssetSlotDto;
}

export interface PathResult {
  path: string;
}

export interface ScreenshotParams {
  target?: ScreenshotTargetDto;
  path: string;
}

export interface ScreenshotResult {
  target: ScreenshotTargetDto;
  path: string;
  pending: boolean;
}

export interface ThumbnailParams {
  asset: AssetSelector;
  size?: number;
}

export interface ThumbnailResult {
  id: WireUuid;
  format: ThumbnailFormatDto;
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

export interface QuitResult {
  quitting: boolean;
}

export interface CreateEntityParams {
  name: string;
}

export interface EntityParams {
  entity: EntitySelector;
}

export interface SetParentParams {
  entity: EntitySelector;
  parent?: EntitySelector;
}

export interface DestroyEntityResult {
  destroyed: WireUuid;
}

export interface EntityListEntry {
  id: WireUuid;
  name: string;
  parentId?: WireUuid;
  bone?: boolean;
}

export interface EntityList {
  entities: EntityListEntry[];
}

export interface ComponentList {
  components: string[];
}

export interface ComponentParams {
  entity: EntitySelector;
  component: string;
}

export interface AddComponentResult {
  added: string;
}

export interface RemoveComponentResult {
  removed: string;
}

export interface SetComponentParams {
  entity: EntitySelector;
  component: string;
  json: ComponentBody;
}

export interface SetComponentResult {
  set: string;
}

export interface SetComponentOrderParams {
  entity: EntitySelector;
  components: string[];
}

export interface SetComponentOrderResult {
  components: string[];
}

export interface SetTransformParams {
  entity: EntitySelector;
  translation?: Vec3;
  rotation?: Vec3;
  scale?: Vec3;
  smooth?: boolean;
}

export interface SetLightParams {
  entity?: EntitySelector;
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
  kind?: PickKind;
  plant?: PlantId;
  position?: [number, number, number];
  normal?: [number, number, number];
}

export interface QuerySurfaceRayParams {
  originM: [number, number, number];
  direction: [number, number, number];
  maxDistanceM?: number;
}

export interface SurfaceRayResult {
  hit: boolean;
  position?: [number, number, number];
  normal?: [number, number, number];
}

export interface SpatialTicksDto {
  x: string;
  y: string;
  z: string;
}

export interface WorldCellKeyDto {
  x: string;
  y: string;
  z: string;
  level: number;
  canonicalHex: string;
}

export interface SpatialLocalPositionDto {
  x: number;
  y: number;
  z: number;
}

export interface SpatialWorldPositionDto {
  cell: WorldCellKeyDto;
  local: SpatialLocalPositionDto;
  globalTicks: SpatialTicksDto;
}

export interface SpatialCellParams {
  world?: Vec3;
  ticks?: SpatialTicksDto;
  level?: number;
}

export interface SpatialCellResult {
  position: SpatialWorldPositionDto;
  selectedCell: WorldCellKeyDto;
}

export interface SurfaceCapabilitiesDto {
  ray: boolean;
  project: boolean;
  nearest: boolean;
  uv: boolean;
  authoritativeAttachments: boolean;
  authoritativeFields: boolean;
}

export interface SpatialBoundsDto {
  minTicks: SpatialTicksDto;
  maxTicksExclusive: SpatialTicksDto;
}

export interface SurfaceProviderDto {
  id: WireUuid;
  entity: WireUuid;
  name: string;
  revision: string;
  bounds: SpatialBoundsDto;
  primitiveCount: string;
  maxTagsPerHit: number;
  capabilities: SurfaceCapabilitiesDto;
}

export interface SurfaceProvidersResult {
  providers: SurfaceProviderDto[];
}

export type SpatialFieldChannelDto = "altitude" | "slope" | "curvature" | "concavity" | "drainage" | "moisture" | "temperature" | "precipitation" | "sunlight" | "exposure" | "water-distance" | "water-depth" | "signed-blocker" | "spline-distance" | "user";

export type SpatialFieldDerivativeDto = "value" | "gradient" | "hessian";

export interface SpatialSampleParams {
  provider: WireUuid;
  channel: SpatialFieldChannelDto;
  userChannel?: string;
  position: Vec3;
  derivative?: SpatialFieldDerivativeDto;
}

export interface SpatialSampleResult {
  provider: WireUuid;
  channel: SpatialFieldChannelDto;
  userChannel?: string;
  derivative: SpatialFieldDerivativeDto;
  valueBits: number;
  value: number;
  revision: string;
}

export type ResidencyFacetDto = "render" | "physics" | "simulation" | "editing" | "navigation" | "network";

export interface SpatialSourceLevelDto {
  level: number;
  loadRadiusCells: number;
  cleanupRadiusCells: number;
}

export interface SpatialSourceDto {
  id: string;
  revision: string;
  position: SpatialWorldPositionDto;
  velocityMps: Vec3;
  predictionSeconds: number;
  levels: SpatialSourceLevelDto[];
  facets: ResidencyFacetDto[];
  priority: number;
}

export interface ResidencyCountsDto {
  render: number;
  physics: number;
  simulation: number;
  editing: number;
  navigation: number;
  network: number;
}

export interface SpatialResidencyCellDto {
  cell: WorldCellKeyDto;
  referenceCounts: ResidencyCountsDto;
  priority: number;
}

export interface SpatialResidencyResult {
  sources: SpatialSourceDto[];
  cells: SpatialResidencyCellDto[];
}

export interface InspectResult {
  id: WireUuid;
  name: string;
  components: Components;
  componentOrder: string[];
}

export interface EnvironmentDto {
  skyMode: SkyModeDto;
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
  fog: FogSettingsDto;
  cloud: CloudSettingsDto;
  wind: WindSettingsDto;
  timeOfDay: TimeOfDaySettingsDto;
}

export type BuiltinEnvironmentProfileDto = "neutral" | "clear-day" | "golden-hour" | "overcast" | "night";

export type EnvironmentProfileRefDto = { "kind": "builtin", profile: BuiltinEnvironmentProfileDto, } | { "kind": "asset", id: WireUuid, };

export interface EnvironmentProfileSummaryDto {
  reference: EnvironmentProfileRefDto;
  name: string;
}

export interface EnvironmentProfileListDto {
  profiles: EnvironmentProfileSummaryDto[];
}

export interface SaveEnvironmentProfileParams {
  name: string;
  folder?: string;
}

export interface UpdateEnvironmentProfileParams {
  profile: WireUuid;
}

export interface ApplyEnvironmentProfileParams {
  profile: EnvironmentProfileRefDto;
}

export interface SetEnvironmentParams {
  json?: unknown;
  skyMode?: SkyModeDto;
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
  moonDiskAngularRadius?: number;
  moonDiskIntensity?: number;
  moonEarthshine?: number;
  perPixelTransmittance?: boolean;
  skyCaptureCadence?: number;
}

export interface SetFogParams {
  json?: unknown;
  enabled?: boolean;
  mode?: FogMode;
  quality?: FogQuality;
  historyBlend?: number;
  neighborhoodClamp?: boolean;
  lightClamp?: number;
  baseDensity?: number;
  scatterAlbedo?: number;
  phaseG?: number;
  density?: number;
  albedo?: Vec3;
  height?: number;
  heightFalloff?: number;
  startDistance?: number;
  maxOpacity?: number;
  emissive?: Vec3;
  directionalColor?: Vec3;
  directionalExponent?: number;
  layer2Density?: number;
  layer2Falloff?: number;
  layer2Height?: number;
  aerialPerspective?: boolean;
  aerialIntensity?: number;
}

export interface SetCloudsParams {
  json?: unknown;
  enabled?: boolean;
  coverage?: number;
  cloudType?: number;
  precipitation?: number;
  anvilBias?: number;
  layerAltitude?: number;
  layerHeight?: number;
  baseScale?: number;
  detailScale?: number;
  detailStrength?: number;
  curlStrength?: number;
  weatherScale?: number;
  weatherOffset?: Vec3;
  weatherTexture?: WireUuid;
  primarySteps?: number;
  lightSteps?: number;
  dropletDiameter?: number;
  temporalFactor?: number;
  castCloudShadows?: boolean;
  cloudShadowStrength?: number;
  cloudShadowOnSurfaceStrength?: number;
}

export interface SetWindParams {
  json?: unknown;
  orientation?: number;
  speed?: number;
  gust?: number;
}

export interface SampleWindParams {
  positionM: [number, number, number];
  timeS?: number;
}

export interface EmitInteractionImpulseParams {
  positionM: [number, number];
  radiusM: number;
  strength: number;
  direction?: [number, number];
  depress?: number;
}

export interface EmitInteractionImpulseResult {
  accepted: boolean;
}

export interface SampleWindResult {
  velocityMps: [number, number, number];
  gustFront: number;
  timeS: number;
}

export interface TodTintCurveDto {
  master: [number, number][];
  red: [number, number][];
  green: [number, number][];
  blue: [number, number][];
}

export interface SetTimeOfDayParams {
  json?: unknown;
  enabled?: boolean;
  manualOverride?: boolean;
  timeOfDay?: number;
  year?: number;
  month?: number;
  day?: number;
  latitude?: number;
  longitude?: number;
  dayLengthSeconds?: number;
  exposureCurve?: [number, number][];
  tintCurve?: TodTintCurveDto;
  coverageCurve?: [number, number][];
  cloudTypeCurve?: [number, number][];
}

export interface SelectionResult {
  selectionVersion: number;
  sceneVersion: number;
  entity?: EntityRef;
  playState: string;
  playVersion: number;
  animationVersion: number;
}

export interface PlayStateResult {
  state: string;
  playVersion: number;
  sceneVersion: number;
  hasPrimaryCamera: boolean;
  animationVersion: number;
  previewAsset: WireUuid;
}

export interface AnimationChannelDto {
  kind: string;
  label: string;
  targetName: string;
  times: number[];
  width: number;
  values: number[];
}

export interface AnimationClipDto {
  id: WireUuid;
  name: string;
  duration: number;
  channels: AnimationChannelDto[];
}

export interface BoneDto {
  index: number;
  name: string;
  parent: number;
  joint: boolean;
}

export interface AssetCapabilitiesDto {
  meshCount: number;
  materialCount: number;
  nodeCount: number;
  hasRig: boolean;
  boneCount: number;
  clipCount: number;
}

export interface GetAssetModelParams {
  asset: AssetSelector;
}

export interface AssetModelResult {
  mesh: WireUuid;
  name: string;
  capabilities: AssetCapabilitiesDto;
  bones: BoneDto[];
  clips: AnimationClipDto[];
}

export interface EnterAssetPreviewParams {
  asset: AssetSelector;
}

export interface BoneEntityDto {
  index: number;
  entity: WireUuid;
}

export interface AssetPreviewResult {
  rootEntity: WireUuid;
  bones: BoneEntityDto[];
  target: Vec3;
  distance: number;
  plantCombinations: PlantCombinationDto[];
}

export interface ListClipsParams {
  asset?: AssetSelector;
}

export interface ListClipsResult {
  clips: AnimationClipDto[];
}

export interface PlayAnimationParams {
  entity: EntitySelector;
  clip: AssetSelector;
  speed?: number;
  loop?: boolean;
  blend?: number;
  paused?: boolean;
}

export interface SeekAnimationParams {
  entity: EntitySelector;
  time: number;
  seekBlend?: number;
}

export interface SetAnimationLoopParams {
  entity: EntitySelector;
  wrap: string;
}

export interface SetAnimationPlayingParams {
  entity: EntitySelector;
  playing: boolean;
}

export interface AnimationStateParams {
  entity: EntitySelector;
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

export interface SetSkeletonOverlayParams {
  show?: boolean;
  axes?: boolean;
  jointSize?: number;
}

export interface SkeletonOverlayResult {
  show: boolean;
  axes: boolean;
  jointSize: number;
  highlightJoint: number;
}

export interface DebugOverlaysParams {
  bounds?: boolean;
  sceneAabb?: boolean;
  lightVolumes?: boolean;
  grid?: boolean;
  colliders?: boolean;
  vegetationCells?: boolean;
  vegetationBounds?: boolean;
  vegetationRejections?: boolean;
  vegetationHeatmap?: boolean;
  vegetationNavigation?: boolean;
  windVectors?: boolean;
}

export interface DebugOverlaysResult {
  bounds: boolean;
  sceneAabb: boolean;
  lightVolumes: boolean;
  grid: boolean;
  colliders: boolean;
  vegetationCells: boolean;
  vegetationBounds: boolean;
  vegetationRejections: boolean;
  vegetationHeatmap: boolean;
  vegetationNavigation: boolean;
  windVectors: boolean;
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
  variation?: number;
  phenotype?: number;
}

export interface PlantCombinationDto {
  variation: number;
  phenotype: number;
}

export interface AssetPreviewOptionsResult {
  floor: boolean;
}

export interface SetFootIkParams {
  entity: EntitySelector;
  enabled?: boolean;
  groundHeight?: number;
}

export interface GetFootIkParams {
  entity: EntitySelector;
}

export interface FootIkResult {
  enabled: boolean;
  groundHeight: number;
  chains: number;
}

export interface SetMorphWeightsParams {
  entity: EntitySelector;
  weights: number[];
}

export interface GetMorphWeightsParams {
  entity: EntitySelector;
}

export interface MorphWeightsResult {
  weights: number[];
  names: string[];
}

export interface ListClipBindingsParams {
  entity: EntitySelector;
  clip: AssetSelector;
}

export interface ClipBindingsResult {
  channels: AnimationChannelDto[];
}

export interface WorldTransformResult {
  translation: Vec3;
  scale: Vec3;
}

export interface StepParams {
  frames?: number;
}

export interface DeselectResult {
  selectionVersion: number;
}

export interface AddEntityParams {
  preset?: AddEntityPreset;
}

export interface RenameEntityParams {
  entity: EntitySelector;
  name: string;
}

export interface SetComponentFieldParams {
  entity: EntitySelector;
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
  op: GizmoOpDto;
  space: GizmoSpaceDto;
  preserveChildren: boolean;
}

export interface SetGizmoParams {
  op?: GizmoOpDto;
  space?: GizmoSpaceDto;
  preserveChildren?: boolean;
}

export interface GizmoPointerParams {
  phase?: GizmoPointerPhase;
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

export interface ListProbesResult {
  enabled: boolean;
  count: number;
  probes: ProbeRef[];
}

export interface SetExposureParams {
  ev: number;
}

export interface SetExposureResult {
  exposureEv: number;
}

export interface AnamorphicParams {
  enabled: boolean;
  ratio: number;
  tint: [number, number, number];
  intensity: number;
}

export interface SetBloomParams {
  enabled: boolean;
  intensity: number;
  scatter: number;
  tint: [number, number, number];
  threshold: number;
  dirtTexture?: WireUuid;
  dirtIntensity?: number;
  dirtTint?: [number, number, number];
  anamorphic?: AnamorphicParams;
  perMipTint?: [number, number, number][];
}

export interface SetBloomResult {
  enabled: boolean;
  intensity: number;
  scatter: number;
  tint: [number, number, number];
  threshold: number;
  dirtTexture: WireUuid;
  dirtIntensity: number;
  dirtTint: [number, number, number];
  anamorphic: AnamorphicParams;
  perMipTint: [number, number, number][];
}

export interface GradeRangeDto {
  slope: [number, number, number];
  offset: [number, number, number];
  power: [number, number, number];
  saturation: number;
  contrast: number;
}

export interface SplitToneDto {
  shadow: [number, number, number];
  highlight: [number, number, number];
  balance: number;
}

export interface SetColorGradingParams {
  temperature: number;
  tint: number;
  contrast: number;
  pivot: number;
  saturation: number;
  slope: [number, number, number];
  offset: [number, number, number];
  power: [number, number, number];
  shadows: GradeRangeDto;
  midtones: GradeRangeDto;
  highlights: GradeRangeDto;
  shadowsMax: number;
  highlightsMin: number;
  channelMixer: [number, number, number, number, number, number, number, number, number];
  splitTone: SplitToneDto;
  creativeLutAsset: WireUuid;
  creativeLutIntensity: number;
}

export interface SetColorGradingResult {
  temperature: number;
  tint: number;
  contrast: number;
  pivot: number;
  saturation: number;
  slope: [number, number, number];
  offset: [number, number, number];
  power: [number, number, number];
  shadows: GradeRangeDto;
  midtones: GradeRangeDto;
  highlights: GradeRangeDto;
  shadowsMax: number;
  highlightsMin: number;
  channelMixer: [number, number, number, number, number, number, number, number, number];
  splitTone: SplitToneDto;
  creativeLutAsset: WireUuid;
  creativeLutIntensity: number;
}

export interface CreativeLutStat {
  asset: WireUuid;
  intensity: number;
  size: number;
}

export interface BakeLookParams {
  name?: string;
}

export interface BakeLookResult {
  asset: WireUuid;
  path: string;
  size: number;
}

export interface SetTessellationQualityParams {
  factorCap?: number;
  minFactor?: number;
  edgeLengthTarget?: number;
}

export interface SetTessellationQualityResult {
  factorCap: number;
  minFactor: number;
  edgeLengthTarget: number;
}

export interface CommandParamsMap {
  "ping": PingParams;
  "render-stats": EmptyParams;
  "gpu-scene-stats": EmptyParams;
  "vegetation-mutate": VegetationMutateParams;
  "vegetation-render-stats": EmptyParams;
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
  "set-hierarchy-cut": SetHierarchyCutParams;
  "vsm-page-budget": VsmPageBudgetParams;
  "page-request-budget": PageRequestBudgetParams;
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
  "query-surface-ray": QuerySurfaceRayParams;
  "spatial-cell": SpatialCellParams;
  "spatial-providers": EmptyParams;
  "spatial-sample": SpatialSampleParams;
  "spatial-residency": EmptyParams;
  "inspect": EntityParams;
  "focus": EntityParams;
  "get-world-transform": EntityParams;
  "get-environment": EmptyParams;
  "get-environment-defaults": EmptyParams;
  "list-environment-profiles": EmptyParams;
  "save-environment-profile": SaveEnvironmentProfileParams;
  "update-environment-profile": UpdateEnvironmentProfileParams;
  "apply-environment-profile": ApplyEnvironmentProfileParams;
  "set-environment": SetEnvironmentParams;
  "set-atmosphere": SetAtmosphereParams;
  "set-fog": SetFogParams;
  "set-clouds": SetCloudsParams;
  "set-wind": SetWindParams;
  "sample-wind": SampleWindParams;
  "emit-interaction-impulse": EmitInteractionImpulseParams;
  "set-time-of-day": SetTimeOfDayParams;
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
  "set-bloom": SetBloomParams;
  "set-color-grading": SetColorGradingParams;
  "bake-look": BakeLookParams;
  "set-tessellation-quality": SetTessellationQualityParams;
  "vegetation-compile-biome": VegetationCompileBiomeParams;
  "vegetation-node-schema": VegetationNodeSchemaParams;
  "vegetation-preflight-region": VegetationPreflightRegionParams;
  "vegetation-start-evaluation": VegetationEvaluationJobParams;
  "vegetation-evaluation-status": VegetationEvaluationJobParams;
  "vegetation-cancel-evaluation": VegetationEvaluationJobParams;
  "vegetation-explain-point": VegetationExplainPointParams;
  "vegetation-cook": VegetationCookParams;
  "vegetation-cook-status": VegetationCookJobParams;
  "vegetation-cancel-cook": VegetationCookJobParams;
  "vegetation-cell-inspect": VegetationCellInspectParams;
  "vegetation-rejections": VegetationRejectionsParams;
  "vegetation-topology-diff": VegetationTopologyDiffParams;
  "vegetation-manifest": VegetationManifestParams;
  "vegetation-runtime-status": EmptyParams;
  "vegetation-runtime-cell": VegetationRuntimeCellParams;
  "vegetation-runtime-query": VegetationRuntimeQueryParams;
  "vegetation-runtime-inspect": VegetationRuntimePlantParams;
  "vegetation-nav-contributions": VegetationNavigationParams;
  "vegetation-drain-events": VegetationDrainEventsParams;
  "vegetation-promote": VegetationRuntimePlantParams;
  "vegetation-fell": VegetationRuntimePlantParams;
  "vegetation-demote": VegetationRuntimePlantParams;
  "vegetation-state-export": EmptyParams;
  "vegetation-state-import": VegetationStateImportParams;
  "vegetation-advance-ecology": VegetationAdvanceEcologyParams;
  "vegetation-ecology-status": EmptyParams;
  "vegetation-ecology-clock": VegetationEcologyClockParams;
  "vegetation-combustion": VegetationCombustionParams;
  "vegetation-usd-skeletons": UsdSkeletonsParams;
  "vegetation-wind-record": VegetationWindRecordParams;
  "vegetation-budgets": VegetationBudgetsParams;
  "vegetation-verify-artifacts": VegetationVerifyParams;
  "vegetation-state-baseline": EmptyParams;
  "vegetation-telemetry": EmptyParams;
  "vegetation-import-points": VegetationImportPointsParams;
  "vegetation-export-points": VegetationExportPointsParams;
  "plant-create": PlantCreateParams;
  "plant-graph": PlantGrowthParams;
  "plant-graph-set": PlantGraphSetParams;
  "plant-growth": PlantGrowthParams;
  "plant-proxies": PlantProxiesParams;
  "plant-season-phenotype": PlantSeasonPhenotypeParams;
  "plant-hierarchy": PlantHierarchyParams;
  "plant-atlas": PlantAtlasParams;
  "plant-phenotypes": PlantPhenotypesParams;
  "plant-elements": PlantGrowthParams;
  "plant-validate": PlantValidateParams;
  "plant-recook": PlantRecookParams;
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
  "import-lut": ImportLutParams;
  "import-vegetation-asset": ImportVegetationAssetParams;
  "list-assets": EmptyParams;
  "vegetation-map-layer-commit": VegetationMapLayerCommitParams;
  "vegetation-map-chunk-commit": VegetationMapChunkCommitParams;
  "vegetation-map-chunk-read": VegetationMapChunkReadParams;
  "vegetation-asset-summary": VegetationAssetSummaryParams;
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
  "gpu-scene-stats": GpuSceneMirrorStatsDto;
  "vegetation-mutate": VegetationMutateResult;
  "vegetation-render-stats": VegetationRenderStatsDto;
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
  "set-hierarchy-cut": HierarchyCutResult;
  "vsm-page-budget": VsmPageBudgetResult;
  "page-request-budget": PageRequestBudgetResult;
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
  "query-surface-ray": SurfaceRayResult;
  "spatial-cell": SpatialCellResult;
  "spatial-providers": SurfaceProvidersResult;
  "spatial-sample": SpatialSampleResult;
  "spatial-residency": SpatialResidencyResult;
  "inspect": InspectResult;
  "focus": EntityRef;
  "get-world-transform": WorldTransformResult;
  "get-environment": EnvironmentDto;
  "get-environment-defaults": EnvironmentDto;
  "list-environment-profiles": EnvironmentProfileListDto;
  "save-environment-profile": EnvironmentProfileSummaryDto;
  "update-environment-profile": EnvironmentProfileSummaryDto;
  "apply-environment-profile": EnvironmentDto;
  "set-environment": EnvironmentDto;
  "set-atmosphere": EnvironmentDto;
  "set-fog": EnvironmentDto;
  "set-clouds": EnvironmentDto;
  "set-wind": EnvironmentDto;
  "sample-wind": SampleWindResult;
  "emit-interaction-impulse": EmitInteractionImpulseResult;
  "set-time-of-day": EnvironmentDto;
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
  "set-bloom": SetBloomResult;
  "set-color-grading": SetColorGradingResult;
  "bake-look": BakeLookResult;
  "set-tessellation-quality": SetTessellationQualityResult;
  "vegetation-compile-biome": VegetationCompileBiomeResult;
  "vegetation-node-schema": VegetationNodeSchemaResult;
  "vegetation-preflight-region": VegetationEvaluationJobDto;
  "vegetation-start-evaluation": VegetationEvaluationJobDto;
  "vegetation-evaluation-status": VegetationEvaluationStatusDto;
  "vegetation-cancel-evaluation": VegetationEvaluationStatusDto;
  "vegetation-explain-point": ProvenanceExplanationDto;
  "vegetation-cook": VegetationCookJobDto;
  "vegetation-cook-status": VegetationCookStatusDto;
  "vegetation-cancel-cook": VegetationCookStatusDto;
  "vegetation-cell-inspect": VegetationCellInspectResult;
  "vegetation-rejections": VegetationRejectionsResult;
  "vegetation-topology-diff": VegetationTopologyDiffResult;
  "vegetation-manifest": VegetationManifestResult;
  "vegetation-runtime-status": VegetationRuntimeStatusDto;
  "vegetation-runtime-cell": VegetationRuntimeCellResult;
  "vegetation-runtime-query": VegetationRuntimeQueryResult;
  "vegetation-runtime-inspect": VegetationRuntimePlantInspectResult;
  "vegetation-nav-contributions": VegetationNavigationResult;
  "vegetation-drain-events": VegetationDrainEventsResult;
  "vegetation-promote": VegetationPromotionResult;
  "vegetation-fell": VegetationPromotionResult;
  "vegetation-demote": VegetationPromotionResult;
  "vegetation-state-export": VegetationStateSnapshotDto;
  "vegetation-state-import": VegetationStateSnapshotDto;
  "vegetation-advance-ecology": VegetationEcologyReportDto;
  "vegetation-ecology-status": VegetationEcologyStatusDto;
  "vegetation-ecology-clock": VegetationEcologyClockDto;
  "vegetation-combustion": VegetationCombustionDto;
  "vegetation-usd-skeletons": UsdSkeletonsResult;
  "vegetation-wind-record": VegetationWindRecordResult;
  "vegetation-budgets": VegetationBudgetsResult;
  "vegetation-verify-artifacts": VegetationVerifyResult;
  "vegetation-state-baseline": VegetationStateBaselineResult;
  "vegetation-telemetry": VegetationTelemetryResult;
  "vegetation-import-points": VegetationImportPointsResult;
  "vegetation-export-points": VegetationExportPointsResult;
  "plant-create": PlantCreateResult;
  "plant-graph": PlantGraphResult;
  "plant-graph-set": PlantGraphResult;
  "plant-growth": BotanicalGrowthDto;
  "plant-proxies": PlantProxiesResult;
  "plant-season-phenotype": PlantSeasonPhenotypeResult;
  "plant-hierarchy": PlantHierarchyResult;
  "plant-atlas": PlantAtlasResult;
  "plant-phenotypes": PlantPhenotypesResult;
  "plant-elements": PlantElementsResult;
  "plant-validate": PlantValidationResult;
  "plant-recook": PlantRecookResult;
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
  "import-lut": ImportLutResult;
  "import-vegetation-asset": ImportVegetationAssetResult;
  "list-assets": AssetList;
  "vegetation-map-layer-commit": VegetationMapLayerCommitResult;
  "vegetation-map-chunk-commit": VegetationMapChunkCommitResult;
  "vegetation-map-chunk-read": VegetationMapChunkReadResult;
  "vegetation-asset-summary": VegetationAssetSummaryResult;
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
