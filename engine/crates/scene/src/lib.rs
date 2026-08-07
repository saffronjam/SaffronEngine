//! The ECS world, components, and the JSON project serde format.
//!
//! `hecs` is wrapped, never exposed: the public surface is [`Scene`], [`Entity`], the
//! component-access methods, and the `for_each` family. No downstream crate names `hecs::`
//! directly, so swapping the ECS stays a one-crate change.

#![deny(unsafe_code)]

#[macro_use]
mod macros;

mod component;
mod document;
mod environment;
mod error;
mod hierarchy;
mod journal;
mod registry;
mod scene;
mod script_input;
mod serde;
mod starter;

pub use component::{
    AnimationPlayer, AtmosphereRole, Bone, BonePhysics, BonePhysicsComponent, Camera,
    CharacterController, Collider, ComponentOrder, DirectionalLight, FogShape, FogVolume,
    FootChain, FootIk, IdComponent, Joint, KinematicBones, MaterialSet, MaterialSlot, Mesh,
    ModelInstance, MorphComponent, MorphWeightOverride, Motion, Name, PhysicsMaterial, PlantOrigin,
    PlantVariant, PlantVitals, PointLight, PoseOverride, PreviewGhost, ReflectionProbe,
    Relationship, Rigidbody, Script, ScriptSlot, Shape, SkinnedMesh, SpotLight, Transform,
    Transition, VegetationField, WindSource, WorldTransform, Wrap,
};
pub use document::SCENE_VERSION;
pub use environment::{
    AssetCatalog, AssetEntry, AssetType, AtmosphereSettings, Attribution, CloudSettings,
    Colorspace, FogMode, FogQuality, FogSettings, SceneEnvironment, SkyMode, TextureRole,
    TimeOfDaySettings, TodCurve, TodTintCurve, WindSettings,
};
pub use error::{Error, Result};
pub use hierarchy::{
    CameraView, camera_projection, quat_from_euler_xyz, quat_to_euler_zyx, transform_matrix,
};
pub use journal::{
    SceneEntityRevisions, SceneJournalCursor, SceneJournalRead, SceneMutation, SceneMutationKind,
    SceneRevision, SceneWorldTransformState,
};
pub use registry::{
    BUILTIN_COMPONENT_NAMES, ComponentRegistry, ComponentTraits, SceneSerialize,
    register_builtin_components,
};
pub use scene::{Component, Entity, PlacedWindSource, Query, Scene};
pub use script_input::{ScriptInputState, derive_script_input_edges};
pub use serde::{environment_from_json, environment_to_json};
pub use starter::seed_starter_scene;
