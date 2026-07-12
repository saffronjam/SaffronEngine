//! The ECS world, components, and the JSON project serde format.
//!
//! The world model is built on an internal ECS (`hecs` by default). That choice is
//! *wrapped, never exposed*: the public surface is [`Scene`], [`Entity`], the
//! component-access methods, and the `for_each` family. No downstream crate names
//! `hecs::` (or a future `bevy_ecs::`) directly, so swapping the ECS is a one-crate
//! change. `Entity` is a bare handle, but every consumer goes through the `Scene` methods.
//!
//! Depends on `saffron-core`, `saffron-json`.

#![deny(unsafe_code)]

#[macro_use]
mod macros;

mod component;
mod document;
mod environment;
mod error;
mod hierarchy;
mod registry;
mod scene;
mod script_input;
mod serde;
mod starter;

pub use component::{
    AnimationPlayer, Bone, BonePhysics, BonePhysicsComponent, Camera, CharacterController,
    Collider, ComponentOrder, DirectionalLight, FogShape, FogVolume, FootChain, FootIk,
    IdComponent, Joint, KinematicBones, MaterialSet, MaterialSlot, Mesh, ModelInstance,
    MorphComponent, MorphWeightOverride, Motion, Name, PhysicsMaterial, PointLight, PoseOverride,
    PreviewGhost, ReflectionProbe, Relationship, Rigidbody, Script, ScriptSlot, Shape, SkinnedMesh,
    SpotLight, Transform, Transition, WorldTransform, Wrap,
};
pub use document::SCENE_VERSION;
pub use environment::{
    AssetCatalog, AssetEntry, AssetType, AtmosphereSettings, Attribution, Colorspace, FogMode,
    FogQuality, FogSettings, SceneEnvironment, SkyMode, TextureRole,
};
pub use error::{Error, Result};
pub use hierarchy::{
    CameraView, camera_projection, quat_from_euler_xyz, quat_to_euler_zyx, transform_matrix,
};
pub use registry::{
    BUILTIN_COMPONENT_NAMES, ComponentRegistry, ComponentTraits, SceneSerialize,
    register_builtin_components,
};
pub use scene::{Component, Entity, Query, Scene};
pub use script_input::{ScriptInputState, derive_script_input_edges};
pub use serde::{environment_from_json, environment_to_json};
pub use starter::seed_starter_scene;
