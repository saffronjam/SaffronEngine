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