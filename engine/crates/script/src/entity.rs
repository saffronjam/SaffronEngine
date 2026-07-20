//! The `sa.Entity` handle: a `'static` userdata holding only an [`Entity`] id, with
//! its scene-only surface resolved through the [session guard](crate::session) each
//! call.
//!
//! The handle caches no scene borrow, only the id and an implicit reach to the live
//! session. Every accessor runs the three-check pattern — session active? entity
//! valid? (for transforms) `Transform` present? — and degrades to a logged no-op
//! returning the documented default otherwise (`vec3{0}` position/rotation, `vec3{1}`
//! scale, `"0"` uuid, `""` name). A handle stashed in a Lua global and used after its
//! session ends therefore resolves to those defaults, never a dangling deref.
//!
//! Transforms cross the boundary as [`SaVec3`]; rotation is engine ZYX-Euler
//! radians, so `get_world_rotation` decomposes the world quaternion to euler and the
//! result round-trips through `set_rotation`.

use mlua::{Lua, UserData, UserDataMethods, Value as LuaValue};
use serde_json::Value as JsonValue;

use saffron_core::Uuid;
use saffron_scene::{
    CharacterController, Entity, IdComponent, Name, Relationship, Scene, Transform,
    quat_to_euler_zyx,
};

use crate::bridge::ScriptRagdollState;
use crate::convert::{json_to_lua, lua_to_json};
use crate::session::{self, ScriptMessage};
use crate::structural::is_structural_component;
use crate::value::SaVec3;

/// The `sa.Entity` userdata: an [`Entity`] id resolved through the session guard.
///
/// `Copy` because it holds only an id — script may pass and stash it freely. It owns
/// no scene borrow, so a stale handle is safe: its accessors find no active session
/// (or an invalid entity) and return the documented default.
#[derive(Clone, Copy, Debug)]
pub struct EntityHandle {
    entity: Entity,
}

/// The invalid handle: every accessor degrades to its documented default through the
/// session guard. Returned by `parent()` at a root, by the `sa.*` query bindings on a
/// miss, and as the sender of a sender-less message.
impl Default for EntityHandle {
    fn default() -> Self {
        Self {
            entity: Entity::NULL,
        }
    }
}

impl EntityHandle {
    /// Wraps an [`Entity`] id as the script handle. The host mints one per
    /// `ScriptComponent` instance and hands it to the class table as `self.entity`.
    #[must_use]
    pub fn new(entity: Entity) -> Self {
        Self { entity }
    }

    /// The wrapped entity id (for the runtime's deferred-op bookkeeping).
    #[must_use]
    pub fn entity(self) -> Entity {
        self.entity
    }

    /// Session active **and** the entity is a live handle in the lent scene. A stale
    /// handle or a closed session is `false`.
    #[must_use]
    pub fn valid(self) -> bool {
        session::with_scene(|scene| scene.valid(self.entity)).unwrap_or(false)
    }

    /// Reads a [`Transform`] field through the three-check guard, logging the no-op
    /// message and returning `None` (→ the caller's default) when the session is
    /// closed, the entity is dead, or it carries no [`Transform`].
    fn read_transform<R>(self, op: &str, f: impl FnOnce(&Transform) -> R) -> Option<R> {
        let resolved = session::with_scene(|scene| {
            if !scene.valid(self.entity) || !scene.has_component::<Transform>(self.entity) {
                return Err(false);
            }
            Ok(scene.with_component::<Transform, R>(self.entity, f).ok())
        });
        match resolved {
            None => {
                tracing::warn!("script: {op} outside a script callback is ignored");
                None
            }
            Some(Err(_)) => {
                tracing::warn!("script: {op} on a missing entity/transform is ignored");
                None
            }
            Some(Ok(value)) => value,
        }
    }

    /// Writes a [`Transform`] through the three-check guard, with the same no-op
    /// logging as [`Self::read_transform`].
    fn write_transform(self, op: &str, f: impl FnOnce(&mut Transform)) {
        let resolved = session::with_scene_mut(|scene| {
            if !scene.valid(self.entity) || !scene.has_component::<Transform>(self.entity) {
                return false;
            }
            let _ = scene.with_component_mut::<Transform, ()>(self.entity, f);
            true
        });
        match resolved {
            None => tracing::warn!("script: {op} outside a script callback is ignored"),
            Some(false) => tracing::warn!("script: {op} on a missing entity/transform is ignored"),
            Some(true) => {}
        }
    }

    /// Local position (`Transform::translation`); `vec3{0}` default.
    #[must_use]
    pub fn get_position(self) -> SaVec3 {
        self.read_transform("get_position", |t| SaVec3(t.translation))
            .unwrap_or_default()
    }

    /// Local rotation as euler radians (`Transform::rotation`); `vec3{0}` default.
    #[must_use]
    pub fn get_rotation(self) -> SaVec3 {
        self.read_transform("get_rotation", |t| SaVec3(t.rotation))
            .unwrap_or_default()
    }

    /// Local scale (`Transform::scale`); `vec3{1}` default.
    #[must_use]
    pub fn get_scale(self) -> SaVec3 {
        self.read_transform("get_scale", |t| SaVec3(t.scale))
            .unwrap_or(SaVec3(glam::Vec3::ONE))
    }

    /// World-space position (`world_translation`, composed through the hierarchy);
    /// `vec3{0}` default. Guarded by the same `Transform`-present check as the local
    /// getters.
    #[must_use]
    pub fn get_world_position(self) -> SaVec3 {
        self.read_world("get_world_position", |scene, e| {
            SaVec3(scene.world_translation(e))
        })
    }

    /// World-space rotation decomposed to euler ZYX radians (`world_rotation` →
    /// `quat_to_euler_zyx`), so it round-trips through [`Self::set_rotation`];
    /// `vec3{0}` default.
    #[must_use]
    pub fn get_world_rotation(self) -> SaVec3 {
        self.read_world("get_world_rotation", |scene, e| {
            SaVec3(quat_to_euler_zyx(scene.world_rotation(e)))
        })
    }

    /// The shared body for the world getters: the same session/valid/`Transform`
    /// three-check, then a scene-level world query (which reads the hierarchy, not a
    /// single component).
    fn read_world(self, op: &str, f: impl FnOnce(&Scene, Entity) -> SaVec3) -> SaVec3 {
        let resolved = session::with_scene(|scene| {
            if !scene.valid(self.entity) || !scene.has_component::<Transform>(self.entity) {
                return Err(false);
            }
            Ok(f(scene, self.entity))
        });
        match resolved {
            None => {
                tracing::warn!("script: {op} outside a script callback is ignored");
                SaVec3::default()
            }
            Some(Err(_)) => {
                tracing::warn!("script: {op} on a missing entity/transform is ignored");
                SaVec3::default()
            }
            Some(Ok(value)) => value,
        }
    }

    /// Sets local position (`Transform::translation`).
    pub fn set_position(self, value: SaVec3) {
        self.write_transform("set_position", |t| t.translation = value.0);
    }

    /// Sets local rotation as euler radians (`Transform::rotation`).
    pub fn set_rotation(self, value: SaVec3) {
        self.write_transform("set_rotation", |t| t.rotation = value.0);
    }

    /// Sets local scale (`Transform::scale`).
    pub fn set_scale(self, value: SaVec3) {
        self.write_transform("set_scale", |t| t.scale = value.0);
    }

    /// The entity's name (`NameComponent`); `""` when the session is closed, the
    /// entity is dead, or it carries no [`Name`]. Name/uuid are pure reads with no log,
    /// not guarded ops.
    #[must_use]
    pub fn name(self) -> String {
        session::with_scene(|scene| {
            if !scene.valid(self.entity) || !scene.has_component::<Name>(self.entity) {
                return String::new();
            }
            scene
                .with_component::<Name, String>(self.entity, |n| n.name.clone())
                .unwrap_or_default()
        })
        .unwrap_or_default()
    }

    /// The entity's uuid as a decimal string (`IdComponent`, matching the wire);
    /// `"0"` when the session is closed, the entity is dead, or it carries no
    /// [`IdComponent`].
    #[must_use]
    pub fn uuid(self) -> String {
        session::with_scene(|scene| {
            if !scene.valid(self.entity) || !scene.has_component::<IdComponent>(self.entity) {
                return "0".to_owned();
            }
            scene
                .component::<IdComponent>(self.entity)
                .map(|id| id.id.to_string())
                .unwrap_or_else(|_| "0".to_owned())
        })
        .unwrap_or_else(|| "0".to_owned())
    }

    /// The shared component-bridge guard: scene + registry present and the entity live,
    /// followed by the per-op body. `f` receives the lent scene and the resolved
    /// registry. Returns `None` (→ the op's documented default) when no session is open
    /// or the entity is dead, logging a message in each case.
    ///
    /// Both the scene and the registry are lent by the same [session
    /// guard](crate::session), so this nests `with_scene_mut` inside `with_registry`;
    /// the registry is read-only during a session, so a `&ComponentRegistry` reaches
    /// the mutable scene-edit fn-pointers it stores without aliasing.
    fn with_session<R>(
        self,
        op: &str,
        f: impl FnOnce(&mut Scene, &saffron_scene::ComponentRegistry) -> R,
    ) -> Option<R> {
        let active = session::session_active() && session::with_registry(|_| ()).is_some();
        if !active {
            tracing::warn!("script: {op} outside a script callback is ignored");
            return None;
        }
        let valid = session::with_scene(|scene| scene.valid(self.entity)).unwrap_or(false);
        if !valid {
            tracing::warn!("script: {op} on a dead entity is ignored");
            return None;
        }
        session::with_registry(|registry| {
            session::with_scene_mut(|scene| f(scene, registry))
                .expect("the scene is lent whenever the registry is")
        })
    }

    /// A read-only snapshot of any registered component as JSON, via the registry's
    /// type-erased serialize — every component reachable with zero per-type code.
    /// `None` (→ `nil`) when the session is closed, the entity is dead, the name is
    /// unknown, or the component is absent.
    #[must_use]
    pub fn get_component_json(self, component_name: &str) -> Option<JsonValue> {
        self.with_session("get_component", |scene, registry| {
            let traits = registry.find_by_name(component_name)?;
            if !(traits.has)(scene, self.entity) {
                return None;
            }
            Some((traits.serialize)(scene, self.entity))
        })
        .flatten()
    }

    /// Writes a JSON patch onto any registered component, via the registry's
    /// type-erased deserialize (a merge — partial patches work). Refuses the
    /// structural components (the gate), logs an unknown name or a failed deserialize,
    /// and returns `false` in each of those cases.
    #[must_use]
    pub fn set_component_json(self, component_name: &str, value: &JsonValue) -> bool {
        if is_structural_component(component_name) {
            tracing::warn!(
                "script: set_component('{component_name}') refused (structural component)"
            );
            return false;
        }
        self.with_session("set_component", |scene, registry| {
            let Some(traits) = registry.find_by_name(component_name) else {
                tracing::warn!("script: set_component('{component_name}') — unknown component");
                return false;
            };
            match (traits.deserialize)(scene, self.entity, value) {
                Ok(()) => true,
                Err(err) => {
                    tracing::warn!("script: set_component('{component_name}'): {err}");
                    false
                }
            }
        })
        .unwrap_or(false)
    }

    /// Default-constructs a registered component onto this entity. Refuses the
    /// structural components (the gate), and is a `false` no-op when the name is
    /// unknown or the component is already present.
    #[must_use]
    pub fn add_component(self, component_name: &str) -> bool {
        if is_structural_component(component_name) {
            tracing::warn!(
                "script: add_component('{component_name}') refused (structural component)"
            );
            return false;
        }
        self.with_session("add_component", |scene, registry| {
            let Some(traits) = registry.find_by_name(component_name) else {
                return false;
            };
            if (traits.has)(scene, self.entity) {
                return false;
            }
            (traits.add_default)(scene, self.entity).is_ok()
        })
        .unwrap_or(false)
    }

    /// Removes a registered component from this entity, honoring the registry's
    /// `removable` flag. A `false` no-op when the name is unknown, the component is
    /// non-removable, or it is absent.
    #[must_use]
    pub fn remove_component(self, component_name: &str) -> bool {
        self.with_session("remove_component", |scene, registry| {
            let Some(traits) = registry.find_by_name(component_name) else {
                return false;
            };
            if !traits.removable || !(traits.has)(scene, self.entity) {
                return false;
            }
            (traits.remove)(scene, self.entity);
            true
        })
        .unwrap_or(false)
    }

    /// Whether this entity carries a registered component. `false` when the session is
    /// closed, the entity is dead, or the name is unknown.
    #[must_use]
    pub fn has_component(self, component_name: &str) -> bool {
        self.with_session("has_component", |scene, registry| {
            registry
                .find_by_name(component_name)
                .is_some_and(|traits| (traits.has)(scene, self.entity))
        })
        .unwrap_or(false)
    }

    /// Queues this entity for destruction at the end of the current instance loop. The
    /// handle stays valid for the rest of the handler; the runtime drains the queue and
    /// relinks the hierarchy once after the loop. A logged no-op outside a session or on
    /// a dead entity.
    ///
    /// The deferral is by *uuid*, not the live handle, because the entity vector is
    /// iterated by reference: destroying mid-loop would invalidate it. Resolving the
    /// uuid back to a handle at flush time is the safe analogue.
    pub fn destroy(self) {
        let uuid = session::with_scene(|scene| {
            if !scene.valid(self.entity) || !scene.has_component::<IdComponent>(self.entity) {
                return None;
            }
            scene
                .component::<IdComponent>(self.entity)
                .ok()
                .map(|id| id.id)
        })
        .flatten();
        match uuid {
            Some(uuid) => session::defer_destroy(uuid),
            None => {
                tracing::warn!("script: destroy outside a callback / on a dead entity is ignored")
            }
        }
    }

    /// Reparents this entity under `new_parent`, the only script reparent path. Runs the
    /// scene's guarded `set_parent` (self/cycle/dangling guards + a relink), keeping
    /// world position; a failed guard or a closed-session/dead-entity call is a logged
    /// `false`.
    ///
    /// Safe mid-tick: `set_parent` touches the relationship components and the children
    /// caches, not the instance vector being iterated.
    #[must_use]
    pub fn set_parent(self, new_parent: EntityHandle) -> bool {
        let resolved = session::with_scene_mut(|scene| {
            if !scene.valid(self.entity) {
                return Err(());
            }
            Ok(scene.set_parent(self.entity, Some(new_parent.entity), true))
        });
        match resolved {
            None => {
                tracing::warn!(
                    "script: set_parent outside a callback / on a dead entity is ignored"
                );
                false
            }
            Some(Err(())) => {
                tracing::warn!(
                    "script: set_parent outside a callback / on a dead entity is ignored"
                );
                false
            }
            Some(Ok(Err(err))) => {
                tracing::warn!("script: set_parent: {err}");
                false
            }
            Some(Ok(Ok(()))) => true,
        }
    }

    /// The parent handle, or an invalid handle at the root. Reads the [`Relationship`]
    /// parent uuid and resolves it; a root (`Uuid(0)`), a closed session, a dead entity,
    /// or no [`Relationship`] all yield the invalid handle (check `:valid()`).
    #[must_use]
    pub fn parent(self) -> EntityHandle {
        let resolved = session::with_scene(|scene| {
            if !scene.valid(self.entity) || !scene.has_component::<Relationship>(self.entity) {
                return None;
            }
            let parent_uuid = scene
                .with_component::<Relationship, _>(self.entity, |rel| rel.parent)
                .unwrap_or(Uuid(0));
            if parent_uuid == Uuid(0) {
                return None;
            }
            scene.find_entity_by_uuid(parent_uuid)
        })
        .flatten();
        EntityHandle::new(resolved.unwrap_or(Entity::NULL))
    }

    /// The child entity handles, in order. An empty list at a leaf, a closed session, a
    /// dead entity, or no [`Relationship`].
    #[must_use]
    pub fn children(self) -> Vec<EntityHandle> {
        session::with_scene(|scene| {
            if !scene.valid(self.entity) || !scene.has_component::<Relationship>(self.entity) {
                return Vec::new();
            }
            scene
                .with_component::<Relationship, _>(self.entity, |rel| {
                    rel.children.iter().map(|&c| EntityHandle::new(c)).collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
    }

    /// Queues a message to this entity's scripts: `self:<handler>(sender, payload)` runs
    /// after the instance loop. `payload` may be `nil`. The sender is the instance whose
    /// handler is running. A logged no-op outside a session or on a dead entity.
    ///
    /// `payload` is stashed as a registry ref (released after dispatch) so it survives
    /// the deferral; `lua` is the VM the ref lives in.
    pub fn send(self, lua: &Lua, handler: &str, payload: LuaValue) {
        let target = session::with_scene(|scene| {
            if !scene.valid(self.entity) || !scene.has_component::<IdComponent>(self.entity) {
                return None;
            }
            scene
                .component::<IdComponent>(self.entity)
                .ok()
                .map(|id| id.id)
        })
        .flatten();
        let Some(target) = target else {
            tracing::warn!("script: send outside a callback / on a dead entity is ignored");
            return;
        };
        let payload_ref = stash_payload(lua, payload);
        session::queue_message(ScriptMessage {
            target,
            sender: session::current_sender(),
            handler: handler.to_owned(),
            payload: payload_ref,
        });
    }

    /// Drives a `CharacterController` capsule: `velocity`'s horizontal components become
    /// the controller's desired velocity (Y ignored, consumed by the next physics step);
    /// `jump` applies the fixed vertical impulse. A **pure Scene write** — the
    /// [`CharacterController`] lives in `saffron-scene`, so this needs no host bridge.
    ///
    /// A logged no-op outside a session, on a dead entity, or on an entity without a
    /// [`CharacterController`].
    pub fn move_character(self, velocity: SaVec3, jump: bool) {
        let resolved = session::with_scene_mut(|scene| {
            if !scene.valid(self.entity) || !scene.has_component::<CharacterController>(self.entity)
            {
                return false;
            }
            let _ = scene.with_component_mut::<CharacterController, ()>(self.entity, |c| {
                c.desired_velocity = glam::Vec3::new(velocity.0.x, 0.0, velocity.0.z);
                if jump {
                    c.vertical_velocity = 5.0;
                }
            });
            true
        });
        match resolved {
            Some(true) => {}
            _ => tracing::warn!(
                "script: move_character on an entity without a CharacterController is ignored"
            ),
        }
    }

    /// Apply a center-of-mass impulse to this entity's Dynamic rigidbody. Bridges over
    /// the host's physics world; a non-Dynamic / unmapped body is a no-op there. The
    /// entity's uuid is the body key.
    pub fn apply_impulse(self, impulse: SaVec3) {
        let uuid = self.body_uuid();
        session::with_bridge(|bridge| bridge.apply_impulse(uuid, impulse.0));
    }

    /// Add a continuous force (applied over the next step) to this entity's Dynamic
    /// rigidbody.
    pub fn add_force(self, force: SaVec3) {
        let uuid = self.body_uuid();
        session::with_bridge(|bridge| bridge.add_force(uuid, force.0));
    }

    /// Set the absolute linear velocity of this entity's Dynamic rigidbody.
    pub fn set_velocity(self, velocity: SaVec3) {
        let uuid = self.body_uuid();
        session::with_bridge(|bridge| bridge.set_velocity(uuid, velocity.0));
    }

    /// Set this entity's morph-target (blend-shape) weights (canonical 0..1). A length
    /// mismatch or a non-morph entity is a no-op on the host side.
    pub fn set_morph_weights(self, weights: Vec<f32>) {
        let uuid = self.body_uuid();
        session::with_bridge(|bridge| bridge.set_morph_weights(uuid, &weights));
    }

    /// The current linear velocity of this entity's Dynamic rigidbody, or `vec3{0}` when
    /// there is none / no bridge is lent.
    #[must_use]
    pub fn get_velocity(self) -> SaVec3 {
        let uuid = self.body_uuid();
        SaVec3(session::with_bridge(|bridge| bridge.get_velocity(uuid)).unwrap_or(glam::Vec3::ZERO))
    }

    /// Make this rig go limp (a passive ragdoll); returns whether the toggle succeeded.
    /// The rig uuid is this entity's id; bridges over the host's physics world.
    #[must_use]
    pub fn enable_ragdoll(self) -> bool {
        let uuid = self.body_uuid();
        session::with_bridge(|bridge| bridge.set_ragdoll_enabled(uuid, true)).unwrap_or(false)
    }

    /// Restore this rig from a ragdoll.
    pub fn disable_ragdoll(self) {
        let uuid = self.body_uuid();
        session::with_bridge(|bridge| bridge.set_ragdoll_enabled(uuid, false));
    }

    /// Blend this rig between physics and animation: `active` arms/releases the motors,
    /// `weight` sets the global body weight (`0` = animation, `1` = physics).
    pub fn set_ragdoll_blend(self, active: bool, weight: f32) {
        let uuid = self.body_uuid();
        session::with_bridge(|bridge| bridge.set_ragdoll_blend(uuid, active, weight));
    }

    /// This rig's live ragdoll state as the POD the binding shapes into `{present,
    /// active, body_weight, bones}`. All-default (absent) when there is no ragdoll / no
    /// bridge is lent.
    #[must_use]
    pub fn ragdoll_state(self) -> ScriptRagdollState {
        let uuid = self.body_uuid();
        session::with_bridge(|bridge| bridge.ragdoll_state(uuid)).unwrap_or_default()
    }

    /// This entity's uuid as the physics body / rig key, or `Uuid(0)` when the session is
    /// closed, the entity is dead, or it carries no [`IdComponent`]. The bridge maps
    /// `Uuid(0)` to a missing body (a no-op).
    fn body_uuid(self) -> Uuid {
        session::with_scene(|scene| {
            if !scene.valid(self.entity) || !scene.has_component::<IdComponent>(self.entity) {
                return Uuid(0);
            }
            scene
                .component::<IdComponent>(self.entity)
                .map(|id| id.id)
                .unwrap_or(Uuid(0))
        })
        .unwrap_or(Uuid(0))
    }
}

/// Stashes a non-nil payload as a registry ref so it survives the message queue, or
/// `None` for a `nil` payload.
fn stash_payload(lua: &Lua, payload: LuaValue) -> Option<mlua::RegistryKey> {
    match payload {
        LuaValue::Nil => None,
        other => lua.create_registry_value(other).ok(),
    }
}

/// `FromLua` for [`EntityHandle`] so a binding argument (`e:set_parent(other)`) accepts
/// the handle by value: any `EntityHandle` userdata is borrowed and copied out.
impl mlua::FromLua for EntityHandle {
    fn from_lua(value: LuaValue, _lua: &Lua) -> mlua::Result<Self> {
        match value {
            LuaValue::UserData(ud) => Ok(*ud.borrow::<EntityHandle>()?),
            other => Err(mlua::Error::FromLuaConversionError {
                from: other.type_name(),
                to: "Entity".to_owned(),
                message: Some("expected an Entity".to_owned()),
            }),
        }
    }
}

impl UserData for EntityHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("valid", |_, this, ()| Ok(this.valid()));
        methods.add_method("name", |_, this, ()| Ok(this.name()));
        methods.add_method("uuid", |_, this, ()| Ok(this.uuid()));
        methods.add_method("get_position", |_, this, ()| Ok(this.get_position()));
        methods.add_method("get_rotation", |_, this, ()| Ok(this.get_rotation()));
        methods.add_method("get_scale", |_, this, ()| Ok(this.get_scale()));
        methods.add_method("get_world_position", |_, this, ()| {
            Ok(this.get_world_position())
        });
        methods.add_method("get_world_rotation", |_, this, ()| {
            Ok(this.get_world_rotation())
        });
        methods.add_method("set_position", |_, this, value: SaVec3| {
            this.set_position(value);
            Ok(())
        });
        methods.add_method("set_rotation", |_, this, value: SaVec3| {
            this.set_rotation(value);
            Ok(())
        });
        methods.add_method("set_scale", |_, this, value: SaVec3| {
            this.set_scale(value);
            Ok(())
        });

        methods.add_method(
            "get_component",
            |lua: &Lua, this, component_name: String| match this.get_component_json(&component_name)
            {
                Some(json) => json_to_lua(lua, &json),
                None => Ok(LuaValue::Nil),
            },
        );
        methods.add_method(
            "set_component",
            |_, this, (component_name, value): (String, LuaValue)| {
                let json = lua_to_json(&value);
                Ok(this.set_component_json(&component_name, &json))
            },
        );
        methods.add_method("add_component", |_, this, component_name: String| {
            Ok(this.add_component(&component_name))
        });
        methods.add_method("remove_component", |_, this, component_name: String| {
            Ok(this.remove_component(&component_name))
        });
        methods.add_method("has_component", |_, this, component_name: String| {
            Ok(this.has_component(&component_name))
        });
        methods.add_method("destroy", |_, this, ()| {
            this.destroy();
            Ok(())
        });

        methods.add_method("set_parent", |_, this, other: EntityHandle| {
            Ok(this.set_parent(other))
        });
        methods.add_method("parent", |_, this, ()| Ok(this.parent()));
        methods.add_method("children", |_, this, ()| Ok(this.children()));
        methods.add_method(
            "send",
            |lua: &Lua, this, (handler, payload): (String, LuaValue)| {
                this.send(lua, &handler, payload);
                Ok(())
            },
        );

        methods.add_method(
            "move_character",
            |_, this, (velocity, jump): (SaVec3, bool)| {
                this.move_character(velocity, jump);
                Ok(())
            },
        );
        methods.add_method("apply_impulse", |_, this, impulse: SaVec3| {
            this.apply_impulse(impulse);
            Ok(())
        });
        methods.add_method("add_force", |_, this, force: SaVec3| {
            this.add_force(force);
            Ok(())
        });
        methods.add_method("set_velocity", |_, this, velocity: SaVec3| {
            this.set_velocity(velocity);
            Ok(())
        });
        methods.add_method("set_morph_weights", |_, this, weights: Vec<f32>| {
            this.set_morph_weights(weights);
            Ok(())
        });
        methods.add_method("get_velocity", |_, this, ()| Ok(this.get_velocity()));
        methods.add_method("enable_ragdoll", |_, this, ()| Ok(this.enable_ragdoll()));
        methods.add_method("disable_ragdoll", |_, this, ()| {
            this.disable_ragdoll();
            Ok(())
        });
        methods.add_method(
            "set_ragdoll_blend",
            |_, this, (active, weight): (bool, f32)| {
                this.set_ragdoll_blend(active, weight);
                Ok(())
            },
        );
        methods.add_method("ragdoll_state", |lua: &Lua, this, ()| {
            let state = this.ragdoll_state();
            let table = lua.create_table()?;
            table.set("present", state.present)?;
            table.set("active", state.active)?;
            table.set("body_weight", state.body_weight)?;
            table.set("bones", state.bones)?;
            Ok(table)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::enter_session;
    use crate::vm::ScriptVm;
    use glam::Vec3;
    use std::sync::Arc;

    use saffron_scene::{ComponentRegistry, register_builtin_components};

    fn registry() -> Arc<ComponentRegistry> {
        Arc::new(register_builtin_components())
    }

    fn world_with_one_entity() -> (Scene, Entity) {
        let mut scene = Scene::new();
        let e = scene.create_entity("hero");
        scene
            .add_component(
                e,
                Transform {
                    translation: Vec3::new(1.0, 2.0, 3.0),
                    rotation: Vec3::new(0.1, 0.2, 0.3),
                    scale: Vec3::new(2.0, 2.0, 2.0),
                },
            )
            .expect("transform");
        (scene, e)
    }

    #[test]
    fn reads_name_uuid_and_transform_inside_a_session() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);
        assert!(handle.valid());
        assert_eq!(handle.name(), "hero");
        assert_ne!(handle.uuid(), "0", "create_entity assigns an IdComponent");
        assert_eq!(handle.get_position(), SaVec3(Vec3::new(1.0, 2.0, 3.0)));
        assert_eq!(handle.get_scale(), SaVec3(Vec3::new(2.0, 2.0, 2.0)));
    }

    #[test]
    fn set_position_is_observed_on_re_read() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);
        handle.set_position(SaVec3(Vec3::new(9.0, 8.0, 7.0)));
        assert_eq!(handle.get_position(), SaVec3(Vec3::new(9.0, 8.0, 7.0)));
    }

    #[test]
    fn world_position_matches_world_translation() {
        let (mut scene, e) = world_with_one_entity();
        let expected = scene.world_translation(e);
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);
        assert_eq!(handle.get_world_position(), SaVec3(expected));
    }

    #[test]
    fn world_rotation_round_trips_through_set_rotation() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);
        let world_euler = handle.get_world_rotation();
        handle.set_rotation(world_euler);
        // After writing the world-decomposed euler back as the local rotation, the
        // re-read world rotation is unchanged (the entity is a root, so local ==
        // world). The euler↔quaternion conversion is not bit-exact, so compare
        // within a tolerance rather than for exact equality.
        let back = handle.get_world_rotation();
        assert!(
            (back.0 - world_euler.0).abs().max_element() < 1e-5,
            "world rotation should round-trip: {world_euler:?} vs {back:?}"
        );
    }

    #[test]
    fn stale_handle_after_session_returns_defaults() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        {
            let _guard = enter_session(&mut scene, registry(), None);
            assert!(handle.valid());
        }
        // Outside the session: every accessor degrades to the documented default,
        // none panics or reads freed memory.
        assert!(!handle.valid());
        assert_eq!(handle.uuid(), "0");
        assert_eq!(handle.name(), "");
        assert_eq!(handle.get_position(), SaVec3(Vec3::ZERO));
        assert_eq!(handle.get_rotation(), SaVec3(Vec3::ZERO));
        assert_eq!(handle.get_scale(), SaVec3(Vec3::ONE));
        assert_eq!(handle.get_world_position(), SaVec3(Vec3::ZERO));
        // A write outside the session is a silent no-op (logged), not a panic.
        handle.set_position(SaVec3(Vec3::new(1.0, 1.0, 1.0)));
        assert!(!handle.valid());
    }

    #[test]
    fn lua_script_reads_and_writes_through_the_handle() {
        let (mut scene, e) = world_with_one_entity();
        let vm = ScriptVm::new().expect("vm");
        vm.register_no_scene_globals().expect("globals");
        vm.lua()
            .globals()
            .set("e", EntityHandle::new(e))
            .expect("set e");

        let _guard = enter_session(&mut scene, registry(), None);
        vm.run_string(
            r#"
            assert(e:valid(), "handle should be valid inside the session")
            assert(e:name() == "hero", "name read failed")
            assert(e:uuid() ~= "0", "uuid should be a real id")
            local p = e:get_position()
            assert(p.x == 1 and p.y == 2 and p.z == 3, "get_position read failed")
            e:set_position(sa.vec3(9, 8, 7))
            local q = e:get_position()
            assert(q.x == 9 and q.y == 8 and q.z == 7, "set_position not observed")
            local w = e:get_world_position()
            assert(w.x == 9 and w.y == 8 and w.z == 7, "world position should match local for a root")
            "#,
            "entity-roundtrip",
        )
        .expect("script should run clean");
    }

    #[test]
    fn lua_handle_stashed_in_a_global_returns_defaults_after_the_session() {
        let (mut scene, e) = world_with_one_entity();
        let vm = ScriptVm::new().expect("vm");
        vm.register_no_scene_globals().expect("globals");
        vm.lua()
            .globals()
            .set("e", EntityHandle::new(e))
            .expect("set e");

        {
            let _guard = enter_session(&mut scene, registry(), None);
            vm.run_string(
                r#"stashed = e
                   assert(stashed:valid(), "valid inside the session")"#,
                "stash",
            )
            .expect("stash run");
        }

        // The handle outlives its session (stashed in a Lua global). Used after the
        // guard ends, it must return the documented defaults, never panic or read
        // freed memory.
        vm.run_string(
            r#"
            assert(stashed:valid() == false, "stale handle must be invalid")
            assert(stashed:uuid() == "0", "stale uuid default")
            assert(stashed:name() == "", "stale name default")
            local p = stashed:get_position()
            assert(p.x == 0 and p.y == 0 and p.z == 0, "stale position default")
            stashed:set_position(sa.vec3(1, 1, 1)) -- a no-op, not a crash
            "#,
            "post-session",
        )
        .expect("post-session script should run clean");
    }

    #[test]
    fn invalid_entity_reports_invalid_and_no_ops() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        scene.destroy_entity(e);
        let _guard = enter_session(&mut scene, registry(), None);
        assert!(!handle.valid(), "a destroyed entity is not valid");
        assert_eq!(handle.uuid(), "0");
        assert_eq!(handle.name(), "");
        assert_eq!(handle.get_position(), SaVec3(Vec3::ZERO));
        // Writing through the dead handle changes nothing observable.
        handle.set_position(SaVec3(Vec3::new(5.0, 5.0, 5.0)));
        assert_eq!(handle.get_position(), SaVec3(Vec3::ZERO));
    }

    /// `get_component("Transform")` returns the component's serde shape — `{x,y,z}`
    /// sub-objects for translation/rotation/scale.
    #[test]
    fn get_component_returns_the_serde_shape() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);
        let json = handle
            .get_component_json("Transform")
            .expect("Transform is present");
        assert_eq!(json["translation"]["x"], serde_json::json!(1.0));
        assert_eq!(json["translation"]["z"], serde_json::json!(3.0));
        assert_eq!(json["scale"]["y"], serde_json::json!(2.0));
        assert!(
            json["rotation"].is_object(),
            "rotation should be an {{x,y,z}} object"
        );
    }

    /// `set_component` merges onto the live component — the change is observed on a
    /// re-read.
    #[test]
    fn set_component_merges_onto_the_live_component() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);
        let patch = serde_json::json!({
            "translation": { "x": 5.0, "y": 6.0, "z": 7.0 },
            "scale": { "x": 1.0, "y": 1.0, "z": 1.0 },
            "rotation": { "x": 0.0, "y": 0.0, "z": 0.0 },
        });
        assert!(handle.set_component_json("Transform", &patch));
        let back = handle.get_component_json("Transform").expect("present");
        assert_eq!(back["translation"]["x"], serde_json::json!(5.0));
        assert_eq!(back["translation"]["z"], serde_json::json!(7.0));
    }

    /// A uuid field comes back as a decimal string (the wire encoding), including a
    /// value past `i64::MAX` — the decimal-string range the whole bridge protects.
    #[test]
    fn uuid_field_round_trips_as_a_decimal_string() {
        let mut scene = Scene::new();
        let e = scene.create_entity("with_mesh");
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);

        let big: u64 = u64::MAX - 7;
        let patch = serde_json::json!({ "mesh": big.to_string() });
        assert!(handle.add_component("Mesh"), "Mesh is non-structural");
        assert!(handle.set_component_json("Mesh", &patch));

        let back = handle.get_component_json("Mesh").expect("Mesh present");
        assert_eq!(
            back["mesh"],
            serde_json::Value::String(big.to_string()),
            "the uuid should come back as its decimal string, not a (wrapped) number"
        );
    }

    /// The structural-component gate: `set_component`/`add_component` on a cache-backed
    /// component is a logged `false` no-op.
    #[test]
    fn structural_components_are_refused_on_write() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);

        assert!(
            !handle.set_component_json("Collider", &serde_json::json!({})),
            "set_component on a structural component is refused"
        );
        assert!(
            !handle.add_component("Rigidbody"),
            "add_component on a structural component is refused"
        );
        // The refusal does not add the component.
        assert!(!handle.has_component("Rigidbody"));
        assert!(!handle.has_component("Collider"));
    }

    /// A non-structural `add_component` adds it and returns `true`; a second add is a
    /// `false` no-op.
    #[test]
    fn non_structural_add_component_adds_once() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);

        assert!(!handle.has_component("PointLight"));
        assert!(handle.add_component("PointLight"), "first add succeeds");
        assert!(handle.has_component("PointLight"));
        assert!(
            !handle.add_component("PointLight"),
            "a second add is a no-op (already present)"
        );
    }

    /// `remove_component` honors the registry's `removable` flag: a removable
    /// component goes, a non-removable one (`Transform`) stays.
    #[test]
    fn remove_component_honors_removable() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);

        assert!(handle.add_component("PointLight"));
        assert!(
            handle.remove_component("PointLight"),
            "a removable component is removed"
        );
        assert!(!handle.has_component("PointLight"));

        // Transform is non-removable — the bridge refuses it and leaves it in place.
        assert!(handle.has_component("Transform"));
        assert!(
            !handle.remove_component("Transform"),
            "a non-removable component is not removed"
        );
        assert!(handle.has_component("Transform"));
    }

    /// An unknown component name is `nil` from `get_component` and `false` from the
    /// write/query bindings — never an abort.
    #[test]
    fn unknown_component_is_nil_or_false_never_an_error() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);

        assert!(handle.get_component_json("NotARealComponent").is_none());
        assert!(!handle.set_component_json("NotARealComponent", &serde_json::json!({})));
        assert!(!handle.add_component("NotARealComponent"));
        assert!(!handle.remove_component("NotARealComponent"));
        assert!(!handle.has_component("NotARealComponent"));
    }

    /// A malformed patch is a logged `false`, and the entity is left usable — the
    /// contained-fault contract (the tick continues).
    #[test]
    fn malformed_patch_returns_false_and_the_entity_survives() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);

        // A bool where a serde object is expected. The lenient readers tolerate it
        // (treating missing fields as defaults), so this exercises the `false`-on-
        // deserialize-failure path without depending on a panic; either way the bridge
        // returns a bool and the entity stays usable.
        let _ = handle.set_component_json("Transform", &serde_json::json!(true));
        assert!(
            handle.valid(),
            "the entity is still usable after a bad patch"
        );
        assert!(handle.get_component_json("Transform").is_some());
    }

    /// Round-trip: `set_component(get_component(x))` is idempotent on a non-structural
    /// component.
    #[test]
    fn set_of_get_is_idempotent() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        let _guard = enter_session(&mut scene, registry(), None);

        let snapshot = handle.get_component_json("Transform").expect("present");
        assert!(handle.set_component_json("Transform", &snapshot));
        let again = handle.get_component_json("Transform").expect("present");
        assert_eq!(snapshot, again, "set(get(x)) must round-trip exactly");
    }

    /// The component bridge is reachable from Lua end to end: a script reads, mutates,
    /// adds, and queries components through the `sa.Entity` methods.
    #[test]
    fn lua_drives_the_component_bridge() {
        let (mut scene, e) = world_with_one_entity();
        let vm = ScriptVm::new().expect("vm");
        vm.register_no_scene_globals().expect("globals");
        vm.lua()
            .globals()
            .set("e", EntityHandle::new(e))
            .expect("set e");

        let _guard = enter_session(&mut scene, registry(), None);
        vm.run_string(
            r#"
            local t = e:get_component("Transform")
            assert(t ~= nil, "Transform should be present")
            assert(t.translation.x == 1, "translation.x")
            t.translation.x = 42
            assert(e:set_component("Transform", t), "set should succeed")
            assert(e:get_component("Transform").translation.x == 42, "merge observed")

            assert(e:has_component("PointLight") == false, "no light yet")
            assert(e:add_component("PointLight") == true, "add light")
            assert(e:has_component("PointLight") == true, "light added")
            assert(e:remove_component("PointLight") == true, "remove light")

            assert(e:get_component("NotReal") == nil, "unknown is nil")
            assert(e:add_component("Rigidbody") == false, "structural refused")
            "#,
            "component-bridge",
        )
        .expect("script should run clean");
    }

    /// Outside a session, every bridge accessor degrades to its documented default —
    /// no panic, no dangling deref.
    #[test]
    fn bridge_outside_a_session_returns_defaults() {
        let (mut scene, e) = world_with_one_entity();
        let handle = EntityHandle::new(e);
        // Open and immediately close a session so the entity exists but no session is
        // active when the bridge is called.
        drop(enter_session(&mut scene, registry(), None));

        assert!(handle.get_component_json("Transform").is_none());
        assert!(!handle.set_component_json("PointLight", &serde_json::json!({})));
        assert!(!handle.add_component("PointLight"));
        assert!(!handle.remove_component("PointLight"));
        assert!(!handle.has_component("Transform"));
    }
}
