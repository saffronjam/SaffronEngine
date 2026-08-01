//! The play-session runtime: one VM, a class-table cache, and an ordered instance vector driven
//! through start / tick / stop.
//!
//! Instances are created in scene `for_each` order and ticked in that order with pause-on-error;
//! `stop_scripts` runs `on_destroy` with no scene bound before dropping the VM.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use mlua::{RegistryKey, Table, Value as LuaValue};
use serde_json::Value as JsonValue;

use saffron_core::Uuid;
use saffron_scene::{ComponentRegistry, Entity, IdComponent, Scene, Script};

use crate::bridge::{NoopBridge, ScriptHostBridge};
use crate::convert::json_to_lua;
use crate::entity::EntityHandle;
use crate::error::{Error, Result};
use crate::scheduler;
use crate::session::{self, DeferredOps};
use crate::value::SaVec3;
use crate::vm::ScriptVm;

/// One live script instance: one slot of one entity's `Script` component, holding a
/// registry ref to its `self` table. Within an entity, instances keep slot order; the
/// vector order across entities is load-bearing (instances run top-to-bottom).
struct ScriptInstance {
    /// The entity's uuid, cached so a contact/message dispatch can match by id.
    entity_uuid: Uuid,
    /// The slot's script path relative to the project `src/`, for error reporting.
    script_path: String,
    /// The registry ref to the instance's `self` table.
    self_ref: RegistryKey,
}

/// One contact/overlap transition surfaced to scripts, the POD input to
/// [`ScriptHost::dispatch_contact`].
///
/// The two entity uuids, the `begin`/`sensor` flags that pick the handler, and the
/// world-space contact manifold. The host fills it from a drained `saffron-physics`
/// `ContactEvent` — `saffron-script` carries no physics edge, so this is the plain shape
/// the binding sees.
/// The `other` argument a contact handler receives: the touching entity's handle, or
/// a macro plant's canonical hex identity.
enum ContactOther {
    /// The other body's scene entity (the null handle when unowned).
    Entity(Entity),
    /// The other body's plant identity as canonical hex.
    Plant(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactInfo {
    /// One body's tagged owner (`None` when the body has no owner).
    pub target_a: Option<crate::bridge::ScriptHitTarget>,
    /// The other body's tagged owner (`None` when none).
    pub target_b: Option<crate::bridge::ScriptHitTarget>,
    /// Whether the contact began (`true`) or ended (`false`).
    pub begin: bool,
    /// Whether either body is a sensor — a trigger overlap, not a solid touch.
    pub sensor: bool,
    /// A representative world-space contact point (zero for an `End` event).
    pub point: glam::Vec3,
    /// The world-space contact normal (`entity_a` → `entity_b`; zero for an `End` event).
    pub normal: glam::Vec3,
}

/// One committed vegetation transition surfaced to scripts, the POD input to
/// [`ScriptHost::dispatch_vegetation_event`].
///
/// The kind and the value names are the same text the control plane reports, so a script and an
/// `sa vegetation-drain-events` call describe one transition identically. The host fills it from a
/// drained `saffron-vegetation` transition — `saffron-script` carries no vegetation edge, so this
/// is the plain shape the handler sees.
#[derive(Clone, Debug, PartialEq)]
pub struct VegetationEventInfo {
    /// Monotonic sequence number within the bound world.
    pub seq: u64,
    /// The transition kind (`damaged`, `harvested`, `burned`, `removed`, `planted`, `regrew`,
    /// `lifecycle-changed`, `ignited`, `extinguished`, `wetted`, `state-replaced`, `moved`,
    /// `disturbed`).
    pub kind: &'static str,
    /// The plant the transition names as canonical hex, absent for a cell-wide change.
    pub plant: Option<String>,
    /// The cell whose persistent state changed, and its hierarchy level.
    pub cell: [i64; 3],
    /// The cell's hierarchy level (`0` is the base level).
    pub cell_level: u8,
    /// The lifecycle the transition settled at, when it names one.
    pub lifecycle: Option<&'static str>,
    /// The prior lifecycle a transition declared as its precondition, when it declared one.
    pub previous_lifecycle: Option<&'static str>,
    /// The species phenotype the transition settled at, when it names one.
    pub phenotype: Option<u32>,
    /// The damage the transition applied, in 0..1.
    pub amount: Option<f32>,
    /// The health it settled at, in 0..1.
    pub health: Option<f32>,
    /// The persistent moisture it settled at, in 0..1.
    pub moisture: Option<f32>,
    /// The combustible fuel it settled at, in 0..1.
    pub fuel: Option<f32>,
    /// Disturbance class bits for a `disturbed` transition.
    pub categories: Option<u32>,
}

/// A contained per-instance failure from a start/tick call, traceback included.
///
/// The first failing instance halts the loop and is returned; the VM and every instance
/// survive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRunError {
    /// The uuid of the entity whose handler faulted.
    pub entity_uuid: Uuid,
    /// The script path of the faulting slot.
    pub script: String,
    /// The Luau error message with its stack traceback.
    pub message: String,
}

/// The per-entity script runtime: one VM for the whole play session, class tables
/// cached by path, instances in deterministic creation order.
///
/// The scene and the registry are *not* stored — they are lent per call by the [session
/// guard](crate::session). The deferred-op queues live in the session guard too, scoped
/// to a call. The physics/log bridges are one [`ScriptHostBridge`], held here and lent to
/// the session for the duration of each call.
pub struct ScriptHost {
    /// The VM for the active session, or `None` between [`ScriptHost::stop_scripts`]
    /// and the next [`ScriptHost::start_scripts`].
    vm: Option<ScriptVm>,
    /// The class table per resolved script path, cached for the VM's lifetime. The key
    /// is the full resolved path.
    class_ref_by_path: HashMap<PathBuf, RegistryKey>,
    /// The ordered instance vector; top-to-bottom run order is load-bearing.
    instances: Vec<ScriptInstance>,
    /// The host-installed callback bridge. A fresh host carries [`NoopBridge`]; the host
    /// installs its real impl with [`ScriptHost::install_bridge`]. Cloned into the
    /// session slot per call so the physics-reaching bindings reach it.
    bridge: Rc<dyn ScriptHostBridge>,
}

impl Default for ScriptHost {
    fn default() -> Self {
        Self {
            vm: None,
            class_ref_by_path: HashMap::new(),
            instances: Vec::new(),
            bridge: Rc::new(NoopBridge),
        }
    }
}

impl ScriptHost {
    /// A fresh host with no VM and the [`NoopBridge`] (the between-sessions state).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs the host's [`ScriptHostBridge`] so the physics-reaching bindings
    /// (`sa.raycast`/`apply_impulse`/ragdoll control) and the `sa.log` sink reach the
    /// live world / script-log ring. The bridge survives across sessions until replaced;
    /// it is lent to each start/tick/contact call and cleared on scope exit.
    pub fn install_bridge(&mut self, bridge: Rc<dyn ScriptHostBridge>) {
        self.bridge = bridge;
    }

    /// Whether a play session is live (a VM is bound and at least one instance loaded).
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.vm.is_some() && !self.instances.is_empty()
    }

    /// The number of live script instances.
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// Creates the VM and instantiates every `Script` slot in `scene`, then runs
    /// `on_create` per instance in order.
    ///
    /// Each slot's script file (under `src_dir`, by the slot's relative path) must
    /// return a class table carrying `on_update`; a slot that fails to load is a logged
    /// skip, not a fatal — the session continues. `self.entity` is bound to the owning
    /// entity and the declared `properties` are injected with the slot's overrides. The
    /// scene is lent for the `on_create` loop and the post-loop structural flush, then
    /// released. `Err` only when no VM could be created at all.
    pub fn start_scripts(
        &mut self,
        scene: &mut Scene,
        registry: Arc<ComponentRegistry>,
        src_dir: &Path,
    ) -> Result<()> {
        self.stop_scripts();

        let vm = ScriptVm::new()?;
        vm.register_no_scene_globals()?;
        vm.register_scene_globals()?;
        self.vm = Some(vm);

        // Collect every non-empty slot in deterministic forEach order before opening
        // the session — `for_each` needs `&mut scene`, which the session then moves in.
        let slots = collect_slots(scene, src_dir);

        // Build every instance (load the class, build the self table, inject fields).
        // The build needs no scene — an `EntityHandle` is just an id resolved later.
        for slot in slots {
            match self.build_instance(&slot) {
                Ok(instance) => self.instances.push(instance),
                Err(err) => {
                    tracing::error!("script: skipping '{}': {err}", slot.script_path);
                }
            }
        }

        // Install the coroutine scheduler prelude onto the now-bound `sa` table, before
        // any instance runs (so an `on_create` may already `sa.spawn_task`).
        if let Some(vm) = self.vm.as_ref()
            && let Err(err) = scheduler::install(vm.lua())
        {
            tracing::error!("script: scheduler prelude failed: {err}");
        }

        // Run on_create per instance, then flush structural ops + dispatch messages, with
        // the scene lent (no input — on_create predates the first tick's input snapshot).
        let guard = session::enter_session(scene, registry, None);
        session::set_bridge(Rc::clone(&self.bridge));
        for index in 0..self.instances.len() {
            let instance = &self.instances[index];
            session::set_sender(instance.entity_uuid);
            if let Err(err) = self.call_instance_method(&instance.self_ref, "on_create", None) {
                tracing::error!("script: on_create '{}': {}", instance.script_path, err);
            }
        }
        session::set_sender(Uuid(0));
        flush_structural_ops();
        self.dispatch_messages();
        drop(guard);

        tracing::info!("scripts started: {} instance(s)", self.instances.len());
        Ok(())
    }

    /// Runs every instance's `on_update(dt)` in order, halting on the first error
    /// (pause-on-error), then flushes the deferred structural ops, dispatches the queued
    /// messages, and advances the coroutine scheduler by `dt`.
    ///
    /// `input` is the host's per-tick gameplay-input snapshot, lent to the input
    /// bindings for the call's duration (its edges must already be derived by the host
    /// before this call); `None` runs the tick with the bindings reading their defaults.
    /// The first faulting instance becomes the returned [`ScriptRunError`] and breaks
    /// the loop; the VM and all instances survive a subsequent tick. `None` (no error)
    /// when there is no VM or no instance.
    pub fn tick_scripts(
        &mut self,
        scene: &mut Scene,
        registry: Arc<ComponentRegistry>,
        input: Option<&mut saffron_scene::ScriptInputState>,
        dt: f32,
    ) -> Option<ScriptRunError> {
        if self.vm.is_none() || self.instances.is_empty() {
            return None;
        }

        let guard = session::enter_session(scene, registry, input);
        session::set_bridge(Rc::clone(&self.bridge));
        let mut failure = None;
        for index in 0..self.instances.len() {
            let instance = &self.instances[index];
            session::set_sender(instance.entity_uuid);
            if let Err(err) = self.call_instance_method(&instance.self_ref, "on_update", Some(dt)) {
                failure = Some(ScriptRunError {
                    entity_uuid: instance.entity_uuid,
                    script: instance.script_path.clone(),
                    message: err.to_string(),
                });
                break;
            }
        }
        session::set_sender(Uuid(0));
        flush_structural_ops();
        self.dispatch_messages();
        self.advance_scheduler(dt);
        drop(guard);
        failure
    }

    /// Dispatches a contact transition to both entities' scripts.
    ///
    /// A sensor Begin invokes `on_trigger_enter(self, other)`, a sensor End
    /// `on_trigger_exit(self, other)`, a solid Begin `on_contact(self, other, point,
    /// normal)` in world space as `sa.Vec3`. A solid End is emitted but routes to no handler.
    ///
    /// The transition dispatches in both directions, A then B; a missing handler is a silent skip;
    /// the first failing handler halts the dispatch and is returned, matching
    /// [`ScriptHost::tick_scripts`]. `None` when there is no VM, no instance, or no handler.
    ///
    /// The host drains the contact ring before `on_update` each tick and drives this per event. After
    /// the dispatch the deferred structural ops flush and the queued messages dispatch, exactly as a
    /// tick does, because a contact handler may `destroy` or `send`.
    pub fn dispatch_contact(
        &mut self,
        scene: &mut Scene,
        registry: Arc<ComponentRegistry>,
        contact: ContactInfo,
    ) -> Option<ScriptRunError> {
        if self.vm.is_none() || self.instances.is_empty() {
            return None;
        }
        // Sensor enter/exit and solid Begin route to a handler; a solid End does not.
        let (handler, with_manifold) = if contact.sensor {
            (
                if contact.begin {
                    "on_trigger_enter"
                } else {
                    "on_trigger_exit"
                },
                false,
            )
        } else if contact.begin {
            ("on_contact", true)
        } else {
            return None;
        };

        let guard = session::enter_session(scene, registry, None);
        session::set_bridge(Rc::clone(&self.bridge));
        let mut failure = self.dispatch_contact_one(
            handler,
            with_manifold,
            contact.target_a,
            contact.target_b,
            contact.point,
            contact.normal,
        );
        if failure.is_none() {
            failure = self.dispatch_contact_one(
                handler,
                with_manifold,
                contact.target_b,
                contact.target_a,
                contact.point,
                contact.normal,
            );
        }
        session::set_sender(Uuid(0));
        flush_structural_ops();
        self.dispatch_messages();
        drop(guard);
        failure
    }

    /// Dispatches one committed vegetation transition to every instance declaring
    /// `on_vegetation_event(self, event)`.
    ///
    /// A vegetation transition names a plant, not an entity, so it broadcasts: a plant is world
    /// state that any script may be watching, and the reducer commit is the one place a change
    /// becomes observable. The event arrives as a table shaped by
    /// [`VegetationEventInfo`]; the first failing handler halts the dispatch and is returned,
    /// matching [`ScriptHost::tick_scripts`]. `None` when there is no VM, no instance, or no
    /// handler.
    ///
    /// The host drains the transition ring before `on_update` each tick and drives this per event.
    /// Structural ops flush and queued messages dispatch afterwards, exactly as a tick does.
    pub fn dispatch_vegetation_event(
        &mut self,
        scene: &mut Scene,
        registry: Arc<ComponentRegistry>,
        event: &VegetationEventInfo,
    ) -> Option<ScriptRunError> {
        if self.vm.is_none() || self.instances.is_empty() {
            return None;
        }
        let guard = session::enter_session(scene, registry, None);
        session::set_bridge(Rc::clone(&self.bridge));
        let mut failure = None;
        for index in 0..self.instances.len() {
            let instance = &self.instances[index];
            session::set_sender(instance.entity_uuid);
            if let Err(err) = self.call_vegetation_handler(&instance.self_ref, event) {
                failure = Some(ScriptRunError {
                    entity_uuid: instance.entity_uuid,
                    script: instance.script_path.clone(),
                    message: err.to_string(),
                });
                break;
            }
        }
        session::set_sender(Uuid(0));
        flush_structural_ops();
        self.dispatch_messages();
        drop(guard);
        failure
    }

    /// Invokes `self:on_vegetation_event(event)` for one instance, resetting the per-call budget
    /// first. An absent handler is a successful no-op.
    fn call_vegetation_handler(
        &self,
        self_ref: &RegistryKey,
        event: &VegetationEventInfo,
    ) -> Result<()> {
        let vm = self
            .vm
            .as_ref()
            .expect("a VM is bound during vegetation dispatch");
        let lua = vm.lua();
        let self_table: Table = lua
            .registry_value(self_ref)
            .map_err(|e| Error::Runtime(e.to_string()))?;
        let method: LuaValue = self_table
            .get("on_vegetation_event")
            .map_err(|e| vm.classify_run_error(&e))?;
        let LuaValue::Function(method) = method else {
            return Ok(());
        };
        vm.reset_budget();
        let table = crate::bindings::vegetation_event_table(lua, event)
            .map_err(|e| Error::Runtime(e.to_string()))?;
        let result: mlua::Result<()> = method.call((self_table, table));
        result.map_err(|e| vm.classify_run_error(&e))
    }

    /// Dispatches one direction of a contact transition: every instance whose
    /// `entity_uuid` matches `self_uuid` runs `self:<handler>(other[, point, normal])`,
    /// halting on the first error. `self_uuid == Uuid(0)`
    /// (a body with no owning entity) is a no-op. `other` is resolved on the lent scene
    /// to an [`EntityHandle`] (an invalid handle when the other body has no entity).
    fn dispatch_contact_one(
        &self,
        handler: &str,
        with_manifold: bool,
        self_target: Option<crate::bridge::ScriptHitTarget>,
        other_target: Option<crate::bridge::ScriptHitTarget>,
        point: glam::Vec3,
        normal: glam::Vec3,
    ) -> Option<ScriptRunError> {
        // Handlers run on scene entities; a plant (or an unowned body) has no script
        // instance until promotion turns it into one.
        let Some(crate::bridge::ScriptHitTarget::SceneEntity(self_uuid)) = self_target else {
            return None;
        };
        let other = match other_target {
            Some(crate::bridge::ScriptHitTarget::SceneEntity(uuid)) => ContactOther::Entity(
                session::with_scene(|scene| scene.find_entity_by_uuid(uuid))
                    .flatten()
                    .unwrap_or(Entity::NULL),
            ),
            Some(crate::bridge::ScriptHitTarget::Vegetation(plant)) => {
                ContactOther::Plant(plant.canonical_hex())
            }
            None => ContactOther::Entity(Entity::NULL),
        };
        for instance in &self.instances {
            if instance.entity_uuid != self_uuid {
                continue;
            }
            session::set_sender(instance.entity_uuid);
            if let Err(err) = self.call_contact_handler(
                &instance.self_ref,
                handler,
                &other,
                with_manifold,
                point,
                normal,
            ) {
                return Some(ScriptRunError {
                    entity_uuid: instance.entity_uuid,
                    script: instance.script_path.clone(),
                    message: err.to_string(),
                });
            }
        }
        None
    }

    /// Invokes `self:<handler>(other[, point, normal])` for one instance, resetting the
    /// per-call budget first.
    ///
    /// An absent handler is a successful no-op (every contact handler is optional). The
    /// manifold args (`point`/`normal` as `sa.Vec3`) are passed only for the solid
    /// `on_contact`; the trigger handlers receive `other` alone. A raised error becomes
    /// the typed [`Error`], classified via the VM's budget trip flag.
    fn call_contact_handler(
        &self,
        self_ref: &RegistryKey,
        name: &str,
        other: &ContactOther,
        with_manifold: bool,
        point: glam::Vec3,
        normal: glam::Vec3,
    ) -> Result<()> {
        let vm = self
            .vm
            .as_ref()
            .expect("a VM is bound during contact dispatch");
        let lua = vm.lua();
        let self_table: Table = lua
            .registry_value(self_ref)
            .map_err(|e| Error::Runtime(e.to_string()))?;
        let method: LuaValue = self_table
            .get(name)
            .map_err(|e| vm.classify_run_error(&e))?;
        let LuaValue::Function(method) = method else {
            // No such handler — a successful no-op.
            return Ok(());
        };

        vm.reset_budget();
        let other_value = match other {
            ContactOther::Entity(entity) => LuaValue::UserData(
                lua.create_userdata(EntityHandle::new(*entity))
                    .map_err(|e| Error::Runtime(e.to_string()))?,
            ),
            ContactOther::Plant(hex) => LuaValue::String(
                lua.create_string(hex)
                    .map_err(|e| Error::Runtime(e.to_string()))?,
            ),
        };
        let result: mlua::Result<()> = if with_manifold {
            method.call((self_table, other_value, SaVec3(point), SaVec3(normal)))
        } else {
            method.call((self_table, other_value))
        };
        result.map_err(|e| vm.classify_run_error(&e))
    }

    /// Runs `on_destroy` per instance with no scene bound, then drops every instance,
    /// the class cache, and the VM.
    ///
    /// `on_destroy` runs outside a session (the play duplicate may already be gone), so
    /// any entity access in it degrades to a logged no-op. After this the host is back
    /// to the fresh state — a second `start_scripts` builds a clean session.
    pub fn stop_scripts(&mut self) {
        if self.vm.is_some() {
            // No session is opened: on_destroy runs with the guard inactive, so entity
            // access degrades to no-ops (the play duplicate may already be discarded).
            for index in 0..self.instances.len() {
                let instance = &self.instances[index];
                if let Err(err) = self.call_instance_method(&instance.self_ref, "on_destroy", None)
                {
                    tracing::warn!("script: on_destroy '{}': {}", instance.script_path, err);
                }
            }
        }
        // Drop the registry keys before the VM so they release into a live state, then
        // drop the VM (mlua frees the VM on Drop).
        self.instances.clear();
        self.class_ref_by_path.clear();
        self.vm = None;
    }

    /// Loads + runs a script file, caching the returned class table per path. The file
    /// must return a table carrying `on_update`; the ref is cached for the VM's
    /// lifetime. A cached path is a no-op.
    fn load_class(&mut self, full_path: &Path) -> Result<()> {
        if self.class_ref_by_path.contains_key(full_path) {
            return Ok(());
        }
        let vm = self.vm.as_ref().expect("a VM is bound during load");
        let source = std::fs::read_to_string(full_path)
            .map_err(|e| Error::Load(format!("{}: {e}", full_path.display())))?;

        vm.reset_budget();
        let lua = vm.lua();
        let chunk_name = full_path.display().to_string();
        let function = lua
            .load(&source)
            .set_name(chunk_name)
            .into_function()
            .map_err(|e| Error::Load(e.to_string()))?;
        let returned: LuaValue = function.call(()).map_err(|e| vm.classify_run_error(&e))?;

        let LuaValue::Table(class) = returned else {
            return Err(Error::Load(format!(
                "'{}' must return a class table",
                full_path.display()
            )));
        };
        let has_update = matches!(
            class.get::<LuaValue>("on_update"),
            Ok(LuaValue::Function(_))
        );
        if !has_update {
            return Err(Error::Load(format!(
                "'{}' class table has no on_update(self, dt)",
                full_path.display()
            )));
        }
        let key = lua
            .create_registry_value(class)
            .map_err(|e| Error::Runtime(e.to_string()))?;
        self.class_ref_by_path.insert(full_path.to_path_buf(), key);
        Ok(())
    }

    /// Builds one [`ScriptInstance`]: load the class (cached), build the `self` table
    /// with `entity` + injected fields + the `__index = Class` metatable, store it in
    /// the registry.
    fn build_instance(&mut self, slot: &CollectedSlot) -> Result<ScriptInstance> {
        let class_key_path = slot.full_path.clone();
        self.load_class(&class_key_path)?;

        let class_key = &self.class_ref_by_path[&class_key_path];
        let vm = self.vm.as_ref().expect("a VM is bound during build");
        let lua = vm.lua();
        let class: Table = lua
            .registry_value(class_key)
            .map_err(|e| Error::Runtime(e.to_string()))?;

        let self_table = lua
            .create_table()
            .map_err(|e| Error::Runtime(e.to_string()))?;
        self_table
            .set("entity", EntityHandle::new(slot.entity))
            .map_err(|e| Error::Runtime(e.to_string()))?;
        inject_fields(lua, &self_table, &class, &slot.overrides)?;

        let metatable = lua
            .create_table()
            .map_err(|e| Error::Runtime(e.to_string()))?;
        metatable
            .set("__index", class)
            .map_err(|e| Error::Runtime(e.to_string()))?;
        self_table
            .set_metatable(Some(metatable))
            .map_err(|e| Error::Runtime(e.to_string()))?;

        let self_ref = lua
            .create_registry_value(self_table)
            .map_err(|e| Error::Runtime(e.to_string()))?;
        Ok(ScriptInstance {
            entity_uuid: slot.entity_uuid,
            script_path: slot.script_path.clone(),
            self_ref,
        })
    }

    /// Calls `self:<name>(dt?)` for the instance at `self_ref`, resetting the per-call
    /// budget first.
    ///
    /// An absent method is a successful no-op — only `on_update` is required (enforced
    /// at load), so `on_create`/`on_destroy` may be missing. A raised error or a budget
    /// trip becomes the typed [`Error`], classified via the VM's budget trip flag.
    fn call_instance_method(
        &self,
        self_ref: &RegistryKey,
        name: &str,
        dt: Option<f32>,
    ) -> Result<()> {
        // Tag every log emitted under a script handler with its entity, so script lines
        // read `… script  [entity=42] …`. The sender is set per instance before each call.
        let _span = tracing::info_span!("script", entity = session::current_sender().0).entered();
        let vm = self
            .vm
            .as_ref()
            .expect("a VM is bound when calling a method");
        let lua = vm.lua();
        let self_table: Table = lua
            .registry_value(self_ref)
            .map_err(|e| Error::Runtime(e.to_string()))?;
        let method: LuaValue = self_table
            .get(name)
            .map_err(|e| vm.classify_run_error(&e))?;
        let LuaValue::Function(method) = method else {
            // No such handler — a successful no-op.
            return Ok(());
        };

        vm.reset_budget();
        let result: mlua::Result<()> = match dt {
            Some(dt) => method.call((self_table, dt)),
            None => method.call(self_table),
        };
        result.map_err(|e| vm.classify_run_error(&e))
    }

    /// Drains the messages queued during the instance loop and dispatches each to its
    /// matching instances, then releases each payload registry ref.
    ///
    /// A targeted message (`entity:send`) reaches only the instance whose
    /// `entity_uuid` matches; a broadcast (`target = Uuid(0)`) reaches every instance.
    /// Runs after the loop on the lent scene, never mid-loop. A faulting handler logs
    /// and the dispatch continues (contained fault).
    fn dispatch_messages(&self) {
        let pending = session::take_messages();
        if pending.is_empty() {
            return;
        }
        for message in pending {
            let sender = if message.sender == Uuid(0) {
                None
            } else {
                session::with_scene(|scene| scene.find_entity_by_uuid(message.sender)).flatten()
            };
            for instance in &self.instances {
                if message.target != Uuid(0) && instance.entity_uuid != message.target {
                    continue;
                }
                self.call_message_handler(&instance.self_ref, &message.handler, sender, &message);
            }
            if let Some(payload) = message.payload {
                self.release_registry_value(payload);
            }
        }
    }

    /// Invokes `self:<handler>(sender, payload)` for one instance, contained: a faulting
    /// handler logs and the dispatch moves on. An absent handler is a silent no-op.
    ///
    /// `sender` is resolved to an [`EntityHandle`] (an invalid handle when the sender is
    /// gone or it was a sender-less send); the payload is read back from its registry
    /// ref, or `nil` when the message carried none.
    fn call_message_handler(
        &self,
        self_ref: &RegistryKey,
        handler: &str,
        sender: Option<Entity>,
        message: &session::ScriptMessage,
    ) {
        let Some(vm) = self.vm.as_ref() else { return };
        let lua = vm.lua();
        let Ok(self_table) = lua.registry_value::<Table>(self_ref) else {
            return;
        };
        let method: LuaValue = match self_table.get(handler) {
            Ok(value) => value,
            Err(_) => return,
        };
        let LuaValue::Function(method) = method else {
            return;
        };

        let sender_handle = EntityHandle::new(sender.unwrap_or(Entity::NULL));
        let payload: LuaValue = match &message.payload {
            Some(key) => lua.registry_value(key).unwrap_or(LuaValue::Nil),
            None => LuaValue::Nil,
        };

        vm.reset_budget();
        let result: mlua::Result<()> = method.call((self_table, sender_handle, payload));
        if let Err(err) = result {
            tracing::warn!("sa: message '{handler}': {}", vm.classify_run_error(&err));
        }
    }

    /// Resumes ready coroutines by `dt` through the global `_sa_advance` the scheduler
    /// prelude installs, contained under the budget/error guard.
    ///
    /// A faulting coroutine logs and the VM survives; an absent `_sa_advance` (the
    /// prelude failed to install) is a silent skip. The accumulation is dt-driven inside
    /// the prelude, so the timing is deterministic (never wall-clock).
    fn advance_scheduler(&self, dt: f32) {
        let Some(vm) = self.vm.as_ref() else { return };
        let lua = vm.lua();
        let advance: LuaValue = match lua.globals().get("_sa_advance") {
            Ok(value) => value,
            Err(_) => return,
        };
        let LuaValue::Function(advance) = advance else {
            return;
        };
        vm.reset_budget();
        if let Err(err) = advance.call::<()>(dt) {
            tracing::warn!("sa: scheduler: {}", vm.classify_run_error(&err));
        }
    }

    /// Removes a payload registry ref from the VM after its message is dispatched, so a
    /// per-tick stream of messages does not leak refs.
    fn release_registry_value(&self, key: RegistryKey) {
        if let Some(vm) = self.vm.as_ref() {
            let _ = vm.lua().remove_registry_value(key);
        }
    }
}

/// Applies the deferred structural ops after an instance loop, on the scene lent by
/// the open session: destroy each queued entity, then relink the hierarchy once if
/// anything changed.
///
/// Operates on the scene through [`session::with_scene_mut`] because the scene is moved
/// into the session slot for the call's duration; the destroy is by *uuid* (resolved
/// back to a live handle here) so it cannot reference a stale handle, and `relink` runs
/// at most once. A no-op when nothing was queued or no session is open.
fn flush_structural_ops() {
    let DeferredOps {
        pending_destroy,
        hierarchy_dirty,
    } = session::take_deferred();
    if pending_destroy.is_empty() && !hierarchy_dirty {
        return;
    }
    session::with_scene_mut(|scene| {
        for uuid in pending_destroy {
            if let Some(entity) = scene.find_entity_by_uuid(uuid)
                && scene.valid(entity)
            {
                scene.destroy_entity(entity);
            }
        }
        if hierarchy_dirty {
            scene.relink_hierarchy();
        }
    });
}

/// One slot collected from the scene before the session opens.
struct CollectedSlot {
    entity: Entity,
    entity_uuid: Uuid,
    script_path: String,
    full_path: PathBuf,
    overrides: JsonValue,
}

/// Walks every `Script` component in `scene` (deterministic `for_each` order) and
/// collects each non-empty slot, resolving its path against `src_dir`.
fn collect_slots(scene: &mut Scene, src_dir: &Path) -> Vec<CollectedSlot> {
    let mut slots = Vec::new();
    scene.for_each::<(&Script, Option<&IdComponent>), _>(|entity, (script, id)| {
        let entity_uuid = id.map_or(Uuid(0), |id| id.id);
        for slot in &script.scripts {
            if slot.script_path.is_empty() {
                continue;
            }
            slots.push(CollectedSlot {
                entity,
                entity_uuid,
                script_path: slot.script_path.clone(),
                full_path: src_dir.join(&slot.script_path),
                overrides: slot.overrides.clone(),
            });
        }
    });
    slots
}

/// Sets every declared `properties` key on `self_table`: the slot's override when
/// present (JSON→Lua), else the declared default.
///
/// A `sa.Vec3` field injects a fresh per-instance value — from the override's 3-number
/// array when present, else a value-copy of the default (`SaVec3` is `Copy`, so it
/// never aliases across instances). A table default is shallow-copied; a scalar default
/// is set directly. Unknown override keys are never visited — a renamed/removed field's
/// stale override is dropped, never an error.
fn inject_fields(
    lua: &mlua::Lua,
    self_table: &Table,
    class: &Table,
    overrides: &JsonValue,
) -> Result<()> {
    let properties: LuaValue = class
        .get("properties")
        .map_err(|e| Error::Runtime(e.to_string()))?;
    let LuaValue::Table(properties) = properties else {
        return Ok(());
    };

    for pair in properties.pairs::<LuaValue, LuaValue>() {
        let (key, default) = pair.map_err(|e| Error::Runtime(e.to_string()))?;
        let LuaValue::String(key) = key else {
            continue;
        };
        let Ok(name) = key.to_str() else {
            continue;
        };
        let name = name.to_owned();

        let vec3_default = vec3_of(&default);
        let override_value = overrides.as_object().and_then(|map| map.get(&name));

        let value: LuaValue = if let Some(over) = override_value {
            if let (Some(_), Some(triple)) = (vec3_default, vec3_array(over)) {
                LuaValue::UserData(
                    lua.create_userdata(SaVec3::new(triple))
                        .map_err(|e| Error::Runtime(e.to_string()))?,
                )
            } else {
                json_to_lua(lua, over).map_err(|e| Error::Runtime(e.to_string()))?
            }
        } else if let Some(default_vec) = vec3_default {
            LuaValue::UserData(
                lua.create_userdata(SaVec3::new(default_vec))
                    .map_err(|e| Error::Runtime(e.to_string()))?,
            )
        } else if let LuaValue::Table(table) = &default {
            LuaValue::Table(shallow_copy(lua, table)?)
        } else {
            default.clone()
        };

        self_table
            .set(name, value)
            .map_err(|e| Error::Runtime(e.to_string()))?;
    }
    Ok(())
}

/// The `glam::Vec3` of a value when it is an `sa.Vec3` userdata, else `None`.
fn vec3_of(value: &LuaValue) -> Option<glam::Vec3> {
    match value {
        LuaValue::UserData(ud) => ud.borrow::<SaVec3>().ok().map(|v| v.0),
        _ => None,
    }
}

/// Reads a 3-number JSON array (the override encoding for a vec3 field) into a
/// `glam::Vec3`, else `None`.
fn vec3_array(value: &JsonValue) -> Option<glam::Vec3> {
    let array = value.as_array()?;
    if array.len() != 3 {
        return None;
    }
    let x = array[0].as_f64()? as f32;
    let y = array[1].as_f64()? as f32;
    let z = array[2].as_f64()? as f32;
    Some(glam::Vec3::new(x, y, z))
}

/// Shallow-copies a Lua table so a table default is never shared between instances —
/// mutating one instance's field must not bleed across.
fn shallow_copy(lua: &mlua::Lua, source: &Table) -> Result<Table> {
    let copy = lua
        .create_table()
        .map_err(|e| Error::Runtime(e.to_string()))?;
    for pair in source.clone().pairs::<LuaValue, LuaValue>() {
        let (key, value) = pair.map_err(|e| Error::Runtime(e.to_string()))?;
        copy.set(key, value)
            .map_err(|e| Error::Runtime(e.to_string()))?;
    }
    Ok(copy)
}
