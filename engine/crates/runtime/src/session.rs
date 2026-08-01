//! [`RuntimeSession`]: build the Jolt world from a scene, then each frame advance animation, step
//! physics, dispatch contacts, and tick scripts.
//!
//! The live Jolt [`World`] sits behind an `Rc<RefCell<Option<…>>>` cell shared with the script
//! bridge, which is what lets an `sa.raycast` re-enter the world mid-tick. Everything else is a
//! plain owned field.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use saffron_animation::{AnimMode, AnimationRuntime};
use saffron_assets::AssetServer;
use saffron_core::Uuid;
use saffron_physics::{ContactKind, PoseTarget, World};
use saffron_scene::{
    CharacterController, ComponentRegistry, Entity, Scene, ScriptInputState,
    derive_script_input_edges, register_builtin_components,
};
use saffron_script::{ContactInfo, ScriptHost, ScriptHostBridge, ScriptRunError};
use saffron_spatial::ResidencyManager;

use crate::bridge::{
    RuntimeScriptBridge, ScriptLogLine, SharedPhysics, SharedScene, SharedScriptSink,
    SharedVegetation, script_vegetation_event,
};
use crate::vegetation::VegetationRuntimeScheduler;
use crate::vegetation_collision::{VegetationCollisionReport, VegetationCollisionResidency};
use crate::vegetation_ecology::VegetationEcologyClock;
use crate::vegetation_family::PlantFamilyCache;
use crate::vegetation_navigation::VegetationNavigationSeam;
use crate::vegetation_promotion::VegetationPromotion;
use crate::{
    VegetationRuntimeBindingStatus, VegetationRuntimeError, VegetationRuntimeUnavailableReason,
};

/// The shared play-mode simulation spine.
///
/// Lifecycle: [`start`](Self::start) builds the world + script VM from a scene; each frame the
/// consumer calls [`tick_animation`](Self::tick_animation) and (while simulating)
/// [`step`](Self::step); [`stop`](Self::stop) ends the session. Buffered script logs/errors are
/// drained by the consumer via [`take_logs`](Self::take_logs) / [`take_errors`](Self::take_errors)
/// so the host routes them into the editor rings and the player to its console.
pub struct RuntimeSession {
    /// The per-session animation player (clip cache + transitions + IK).
    animation: AnimationRuntime,
    /// The play session's script VM + instances.
    script: ScriptHost,
    /// The component reflection table the script start/tick/contact calls bind, built once.
    registry: Arc<ComponentRegistry>,
    /// The live Jolt world, present between [`start`](Self::start) and [`stop`](Self::stop)
    /// (`None` otherwise). Behind the shared cell so the bridge's `sa.raycast`/impulse bindings
    /// reach it without owning it.
    physics: SharedPhysics,
    /// The host bridge, kept alive across sessions so the same `Rc` re-installs on each start.
    bridge: Rc<dyn ScriptHostBridge>,
    /// The shared `sa.log` buffer the bridge appends to; drained by [`take_logs`](Self::take_logs).
    log_sink: SharedScriptSink,
    /// Errors a tick recorded; drained by [`take_errors`](Self::take_errors).
    error_sink: Vec<ScriptRunError>,
    /// This frame's ragdoll pose targets, snapshotted from the animation runtime before the
    /// physics step so active ragdolls motor toward the animated pose.
    pose_targets: Vec<PoseTarget>,
    /// The per-tick contact → script dispatch high-water cursor.
    contact_cursor: u64,
    /// The per-tick vegetation-transition → script dispatch high-water cursor. It resets with
    /// the session and with the bound world, whose ring restarts its numbering at one.
    vegetation_event_cursor: u64,
    /// Whether a script VM is live (set by [`start`](Self::start), cleared by [`stop`](Self::stop)).
    script_vm_active: bool,
    /// Whether the Jolt process globals are installed — set true the first time a world is built.
    /// They outlive every world, so teardown shuts them down once, after the last world drops.
    physics_init: bool,
    /// The sole authoritative vegetation runtime, bound to one exact cooked manifest. Behind the
    /// shared cell so a script's `sa.vegetation_*` call reaches the same world the host publishes.
    vegetation: SharedVegetation,
    vegetation_scheduler: VegetationRuntimeScheduler,
    vegetation_status: VegetationRuntimeBindingStatus,
    /// Generation-tagged batched Jolt proxies for physics-resident vegetation cells.
    vegetation_collision: VegetationCollisionResidency,
    /// The promotion authority owning every transient macro-plant entity view.
    vegetation_promotion: VegetationPromotion,
    /// Resolved `.splant` family assets shared by collision residency and promotion.
    vegetation_families: PlantFamilyCache,
    /// Published navigation contributions and the dirty world regions they moved.
    vegetation_navigation: VegetationNavigationSeam,
    /// The world simulation clock biology advances on, fed by the play step.
    vegetation_ecology: VegetationEcologyClock,
    vegetation_telemetry: crate::VegetationTelemetry,
}

impl Default for RuntimeSession {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeSession {
    /// Builds an idle session: the animation runtime, an empty script VM, the component
    /// registry, and the script bridge over the shared world / log cells. No world exists until
    /// [`start`](Self::start).
    #[must_use]
    pub fn new() -> Self {
        let physics: SharedPhysics = Rc::new(RefCell::new(None));
        let log_sink: SharedScriptSink = Rc::new(RefCell::new(Vec::new()));
        // The bridge's scene cell backs only the script-driven `sa.set_ragdoll_enabled(rig)`
        // off-gate path; control/runtime-driven ragdoll enabling goes straight through the
        // world, so it stays empty here.
        let bridge_scene: SharedScene = Rc::new(RefCell::new(Scene::new()));
        let vegetation: SharedVegetation = Rc::new(RefCell::new(None));
        let bridge: Rc<dyn ScriptHostBridge> = Rc::new(RuntimeScriptBridge::new(
            Rc::clone(&physics),
            bridge_scene,
            Rc::clone(&vegetation),
            Rc::clone(&log_sink),
        ));
        Self {
            animation: AnimationRuntime::new(),
            script: ScriptHost::new(),
            registry: Arc::new(register_builtin_components()),
            physics,
            bridge,
            log_sink,
            error_sink: Vec::new(),
            pose_targets: Vec::new(),
            contact_cursor: 0,
            vegetation_event_cursor: 0,
            script_vm_active: false,
            physics_init: false,
            vegetation,
            vegetation_scheduler: VegetationRuntimeScheduler::default(),
            vegetation_status: VegetationRuntimeBindingStatus::default(),
            vegetation_collision: VegetationCollisionResidency::default(),
            vegetation_promotion: VegetationPromotion::default(),
            vegetation_families: PlantFamilyCache::default(),
            vegetation_navigation: VegetationNavigationSeam::default(),
            vegetation_ecology: VegetationEcologyClock::default(),
            vegetation_telemetry: crate::VegetationTelemetry::default(),
        }
    }

    /// Starts a play session against `scene`: builds the Jolt world (cooking collider/rigidbody
    /// shapes through `assets`, adding per-bone kinematic bodies + a `CharacterVirtual` per
    /// controller), then starts the script VM (loading `<project_root>/src` classes + injecting
    /// fields). A world-create or script-start failure is logged and leaves that part inactive;
    /// physics still steps without scripts. Drain any buffered `on_create` logs afterward with
    /// [`take_logs`](Self::take_logs).
    pub fn start(&mut self, scene: &mut Scene, assets: &mut AssetServer, project_root: &Path) {
        let world = match World::new() {
            Ok(world) => world,
            Err(err) => {
                tracing::error!("physics world create failed: {err}");
                return;
            }
        };
        self.physics_init = true; // globals installed by `World::new`; teardown shuts them down once.
        *self.physics.borrow_mut() = Some(world);
        self.contact_cursor = 0;
        self.populate_world(scene, assets);
        self.start_scripts(scene, project_root);
    }

    /// Populates the live world from the scene's components: collider/rigidbody bodies (cooking
    /// convex-hull/mesh shapes through the asset reader), per-bone kinematic bodies, and a
    /// `CharacterVirtual` per controller entity.
    fn populate_world(&mut self, scene: &mut Scene, assets: &mut AssetServer) {
        let mut world_ref = self.physics.borrow_mut();
        let Some(world) = world_ref.as_mut() else {
            return;
        };
        let mut cook = |id: Uuid| {
            assets
                .load_mesh_cpu_asset(id)
                .map_err(|err| err.to_string())
        };
        world.populate(scene, &mut cook);
        world.build_bone_bodies(scene);

        let mut characters: Vec<Entity> = Vec::new();
        scene.for_each::<&CharacterController, _>(|entity, _| {
            characters.push(entity);
        });
        for entity in characters {
            if let Err(err) = world.add_character(entity, scene) {
                tracing::warn!("character controller setup failed: {err}");
            }
        }
    }

    /// Starts the script VM and installs the bridge. A start failure leaves the VM inactive.
    fn start_scripts(&mut self, scene: &mut Scene, project_root: &Path) {
        self.script.install_bridge(Rc::clone(&self.bridge));
        let src_dir = project_root.join("src");
        let registry = Arc::clone(&self.registry);
        match self.script.start_scripts(scene, registry, &src_dir) {
            Ok(()) => self.script_vm_active = true,
            Err(err) => {
                tracing::error!("script start failed: {err}");
                self.script_vm_active = false;
            }
        }
    }

    /// Advances the animation runtime over `scene` for this frame. Runs in both `Edit` (the
    /// editor's preview) and `Play`; the simulation [`step`](Self::step) is separate (play only),
    /// so a script can still override a bone the same frame physics settles it. Resolves clips
    /// through `assets`.
    pub fn tick_animation(
        &mut self,
        scene: &mut Scene,
        assets: &mut AssetServer,
        dt: f32,
        mode: AnimMode,
    ) {
        let mut load = |id: Uuid| {
            assets
                .load_anim_clip(id)
                .map_err(|err| saffron_animation::Error::ClipLoad(err.to_string()))
        };
        saffron_animation::tick_animation(&mut self.animation, scene, dt, mode, &mut load);
    }

    /// Steps the simulation one tick over `scene`: snapshots the animated pose for ragdoll
    /// motors, steps physics (writing dynamic/ragdoll poses back into `scene`), dispatches the
    /// tick's new contacts to scripts, derives this tick's input edges from `input`, and runs
    /// every instance's `on_update`. Physics-then-scripts, so a script reads this frame's
    /// settled transforms. Buffers contained script failures for [`take_errors`](Self::take_errors).
    ///
    /// The world borrow is scoped and released before any script runs, so a contact /
    /// `on_update` handler may `sa.raycast` back into the world through the bridge.
    pub fn step(&mut self, scene: &mut Scene, dt: f32, input: &mut ScriptInputState) {
        // The world simulation clock biology runs on: this step is what "the world advanced" means,
        // so it is what earns ecology ticks. The ticks themselves execute at the vegetation
        // synchronization point, where the bound world and its residency are in hand.
        self.vegetation_ecology.accumulate(dt);

        // Snapshot this frame's animated poses for the ragdoll motors (cheap when no rig is
        // driven; meaningful only with a live VM + rigs).
        if self.script_vm_active {
            self.pose_targets.clear();
            for (rig, pose) in self.animation.last_poses() {
                self.pose_targets.push(PoseTarget {
                    rig: Uuid(rig),
                    local: pose.to_vec(),
                });
            }
        }

        let events = {
            let mut world_ref = self.physics.borrow_mut();
            let Some(world) = world_ref.as_mut() else {
                return;
            };
            // Drive active ragdolls toward the animated pose, ease the per-bone weight, then
            // step — drive before the step so the motors are read in the solve.
            world.drive_ragdolls_to_pose(&self.pose_targets);
            world.advance_ragdoll_blend(dt);
            world.step(scene, dt);
            // Physics wins the frame: write each ragdoll part's pose into the bone override.
            world.write_ragdoll_poses(scene);

            if self.script_vm_active {
                let drain = world.drain_contacts(self.contact_cursor);
                self.contact_cursor = drain.high_water_seq;
                drain.events
            } else {
                Vec::new()
            }
        };

        if !self.script_vm_active {
            return;
        }

        // Dispatch the drained contacts before `on_update`, so a trigger/contact handler runs
        // the same frame the contact fired.
        for event in events {
            let contact = ContactInfo {
                target_a: event.target_a.map(crate::bridge::script_target),
                target_b: event.target_b.map(crate::bridge::script_target),
                begin: event.kind == ContactKind::Begin,
                sensor: event.sensor,
                point: event.point,
                normal: event.normal,
            };
            if let Some(err) =
                self.script
                    .dispatch_contact(scene, Arc::clone(&self.registry), contact)
            {
                tracing::error!(
                    "script contact handler in '{}': {}",
                    err.script,
                    err.message
                );
                self.error_sink.push(err);
                return;
            }
        }

        // Then the vegetation transitions committed since the last tick. A reducer commit is the
        // one place a vegetation change becomes observable, so scripts see it the frame it lands
        // and in commit order. The world borrow is released before dispatch, so a handler may
        // query or mutate vegetation back through the bridge.
        for event in self.drain_vegetation_events() {
            if let Some(err) =
                self.script
                    .dispatch_vegetation_event(scene, Arc::clone(&self.registry), &event)
            {
                tracing::error!(
                    "script vegetation handler in '{}': {}",
                    err.script,
                    err.message
                );
                self.error_sink.push(err);
                return;
            }
        }

        // Derive this tick's input edges, then run every instance's `on_update`.
        derive_script_input_edges(input);
        if let Some(err) =
            self.script
                .tick_scripts(scene, Arc::clone(&self.registry), Some(input), dt)
        {
            tracing::error!("script error in '{}': {}", err.script, err.message);
            self.error_sink.push(err);
        }
    }

    /// Reads the committed vegetation transitions newer than the dispatch cursor and advances it.
    /// A cursor left behind the ring's retained tail resyncs to the tail rather than replaying a
    /// gap it cannot fill.
    fn drain_vegetation_events(&mut self) -> Vec<saffron_script::VegetationEventInfo> {
        let vegetation = self.vegetation.borrow();
        let Some(world) = vegetation.as_ref() else {
            return Vec::new();
        };
        let drain = world.drain_events(self.vegetation_event_cursor);
        if drain.overflowed {
            tracing::warn!(
                "script vegetation events: the dispatch cursor fell behind the retained ring"
            );
        }
        self.vegetation_event_cursor = drain.high_water_seq;
        drain.events.iter().map(script_vegetation_event).collect()
    }

    /// Advances the world one frame in play: ticks animation in `Play` mode then steps the
    /// simulation. The convenience entry for an always-playing consumer (the standalone
    /// player); the editor host instead calls [`tick_animation`](Self::tick_animation) (with its
    /// Edit/Play mode) and the gated [`step`](Self::step) separately.
    pub fn advance(
        &mut self,
        scene: &mut Scene,
        assets: &mut AssetServer,
        dt: f32,
        input: &mut ScriptInputState,
    ) {
        self.tick_animation(scene, assets, dt, AnimMode::Play);
        self.step(scene, dt, input);
    }

    /// Ends the play session: stops the VM, drops the world, and clears the transient buffers.
    /// Leaves the Jolt process globals installed (they persist across sessions); call
    /// [`shutdown_physics_globals`](Self::shutdown_physics_globals) once at final teardown.
    pub fn stop(&mut self) {
        self.script.stop_scripts();
        self.script_vm_active = false;
        *self.physics.borrow_mut() = None;
        self.vegetation_collision.reset();
        self.vegetation_promotion.reset();
        self.vegetation_families.clear();
        // Simulated time the ended session accumulated is not the next one's.
        self.vegetation_ecology.rebind();
        self.pose_targets.clear();
        self.log_sink.borrow_mut().clear();
        self.error_sink.clear();
        self.contact_cursor = 0;
        self.vegetation_event_cursor = 0;
    }

    /// Stops the script VM (a teardown step; it never touches the scene, so it tears down before
    /// the world).
    pub fn stop_scripts(&mut self) {
        self.script.stop_scripts();
        self.script_vm_active = false;
    }

    /// Drops the live world (a teardown step; RAII frees its Jolt bodies before the globals
    /// shut down).
    pub fn drop_physics_world(&mut self) {
        *self.physics.borrow_mut() = None;
        self.vegetation_collision.reset();
        self.vegetation_promotion.reset();
    }

    /// Shuts down the Jolt process globals — only after the last world is gone (a live world
    /// holds Jolt bodies). Idempotent.
    pub fn shutdown_physics_globals(&mut self) {
        if self.physics_init {
            saffron_physics::shutdown_physics();
            self.physics_init = false;
        }
    }

    /// Drops the runtime's per-session animation transition/pose entries (the host calls this on
    /// an asset-preview transition edge so a re-entered preview starts clean).
    pub fn prune_animation(&mut self) {
        self.animation.prune_session();
    }

    /// Drains the buffered `sa.log` lines (the consumer routes them: editor ring / player stdout).
    pub fn take_logs(&mut self) -> Vec<ScriptLogLine> {
        self.log_sink.borrow_mut().drain(..).collect()
    }

    /// Drains the script errors a tick recorded (non-empty means the consumer should surface
    /// them — the host pauses play, the player logs).
    pub fn take_errors(&mut self) -> Vec<ScriptRunError> {
        std::mem::take(&mut self.error_sink)
    }

    /// An owned clone of the shared world cell, for a consumer that must lend the live world
    /// elsewhere mid-frame (the host hands `borrow_mut().as_mut()` into its control plane). The
    /// clone is cheap (`Rc`) and borrowing it does not alias the session's other state.
    #[must_use]
    pub fn physics_cell(&self) -> SharedPhysics {
        Rc::clone(&self.physics)
    }

    /// An owned clone of the shared vegetation cell. A consumer borrows it for the span it needs
    /// the authority — the render mirror and the overlays for a read, the control plane for a
    /// mutable drain. The clone is cheap (`Rc`) and borrowing it does not alias the session's
    /// other state.
    #[must_use]
    pub fn vegetation_cell(&self) -> SharedVegetation {
        Rc::clone(&self.vegetation)
    }

    /// The vegetation runtime's compact telemetry.
    #[must_use]
    pub fn vegetation_telemetry(&self) -> &crate::VegetationTelemetry {
        &self.vegetation_telemetry
    }

    /// The vegetation runtime's telemetry, for the seams that record into it.
    pub fn vegetation_telemetry_mut(&mut self) -> &mut crate::VegetationTelemetry {
        &mut self.vegetation_telemetry
    }

    /// Reconciles the exact cooked generation, shared spatial demand, and bounded cell-load workers,
    /// then synchronizes the collision facet: every physics-resident cell generation's batched Jolt
    /// proxies are created and removed against the live play world at this one point.
    pub fn synchronize_vegetation(
        &mut self,
        scene: &mut Scene,
        assets: &AssetServer,
        spatial: &ResidencyManager,
    ) -> Result<(), VegetationRuntimeError> {
        let vegetation_cell = Rc::clone(&self.vegetation);
        let mut vegetation_ref = vegetation_cell.borrow_mut();
        let bound_identity = vegetation_ref
            .as_ref()
            .map(|world| world.manifest_identity());
        let scheduled = {
            let scheduler = &mut self.vegetation_scheduler;
            let vegetation = &mut vegetation_ref;
            self.vegetation_telemetry
                .stage(crate::VegetationStage::Residency, || {
                    scheduler.advance(vegetation, scene, assets, spatial)
                })
        };
        match scheduled {
            Ok(status) => {
                self.vegetation_status = status;
                let bound_now = vegetation_ref
                    .as_ref()
                    .map(|world| world.manifest_identity());
                if bound_now != bound_identity {
                    // A different generation is bound, so the accumulated simulated time and the
                    // ticks the last catch-up left owed belong to a world that is gone — and so
                    // does the sequence the incoming ring restarts from.
                    self.vegetation_ecology.rebind();
                    self.vegetation_event_cursor = 0;
                }
                // Biology advances before the facets derive from it, so a tick's committed
                // generation is the one collision, navigation, and promotion see this frame.
                let mut ecology_fault = None;
                if let Some(vegetation) = vegetation_ref.as_mut()
                    && self
                        .vegetation_ecology
                        .wants_advance(vegetation.ecology_ground_revision())
                {
                    let clock = &mut self.vegetation_ecology;
                    let telemetry = &mut self.vegetation_telemetry;
                    match telemetry.stage(crate::VegetationStage::Ecology, || {
                        clock.advance(vegetation)
                    }) {
                        Ok(report) => telemetry.record_ecology_ticks(report.ticks_run),
                        Err(error) => ecology_fault = Some(error),
                    }
                }
                let mut world_ref = self.physics.borrow_mut();
                // A rebind replaced the bound generation, so any entity view describes plants of
                // a world that no longer exists.
                if bound_identity.is_some() && bound_now != bound_identity {
                    self.vegetation_promotion.abandon(scene, world_ref.as_mut());
                }
                // The clock owns the declared influence, so the facets read region readiness at the
                // same radius a tick reads its halo at.
                let influence = self.vegetation_ecology.influence();
                match (world_ref.as_mut(), vegetation_ref.as_mut()) {
                    (mut world, Some(vegetation)) => {
                        // Promotion commits first: the collision pass below then sees the
                        // suppression it just applied, so a plant never has two owners.
                        let promotion = &mut self.vegetation_promotion;
                        let families = &mut self.vegetation_families;
                        let telemetry = &mut self.vegetation_telemetry;
                        telemetry.stage(crate::VegetationStage::Promotion, || {
                            promotion.advance(
                                vegetation,
                                scene,
                                assets,
                                families,
                                world.as_deref_mut(),
                            );
                        });
                        if let Some(world) = world {
                            let collision = &mut self.vegetation_collision;
                            telemetry.stage(crate::VegetationStage::Collision, || {
                                collision.advance(vegetation, world, assets, families, influence);
                            });
                        }
                        // Navigation publishes from the same committed state, after promotion has
                        // decided which plants are moving.
                        let navigation = &mut self.vegetation_navigation;
                        telemetry.stage(crate::VegetationStage::Navigation, || {
                            navigation.advance(vegetation, assets, families, influence);
                        });
                    }
                    (Some(world), None) => {
                        self.vegetation_promotion.abandon(scene, Some(world));
                        self.vegetation_collision.remove_all(world);
                        self.vegetation_navigation.clear();
                    }
                    (None, None) => {
                        self.vegetation_promotion.reset();
                        self.vegetation_collision.reset();
                        self.vegetation_navigation.clear();
                    }
                }
                self.vegetation_telemetry.commit();
                // The facets reconciled against the state that is committed; the fault surfaces
                // after them so a stuck catch-up does not also strand collision bodies.
                match ecology_fault {
                    Some(error) => Err(VegetationRuntimeError::Vegetation(error)),
                    None => Ok(()),
                }
            }
            Err(error) => {
                self.vegetation_status = VegetationRuntimeBindingStatus::Unavailable {
                    reason: VegetationRuntimeUnavailableReason::Fault,
                    detail: Some(error.to_string()),
                };
                self.vegetation_telemetry.commit();
                Err(error)
            }
        }
    }

    /// The promotion authority, the navigation seam, the ecology clock, and the telemetry, borrowed
    /// disjointly for one control-plane drain (a command may touch any of them). The vegetation
    /// authority itself comes from [`vegetation_cell`](Self::vegetation_cell), so they all borrow
    /// independently.
    pub fn vegetation_control_authorities(
        &mut self,
    ) -> (
        &mut VegetationPromotion,
        &mut VegetationNavigationSeam,
        &mut VegetationEcologyClock,
        &mut crate::VegetationTelemetry,
    ) {
        (
            &mut self.vegetation_promotion,
            &mut self.vegetation_navigation,
            &mut self.vegetation_ecology,
            &mut self.vegetation_telemetry,
        )
    }

    /// The world simulation clock biology advances on.
    #[must_use]
    pub fn vegetation_ecology_clock(&self) -> &VegetationEcologyClock {
        &self.vegetation_ecology
    }

    /// Current collision-facet counters, present only while a live play world can carry the bodies.
    #[must_use]
    pub fn vegetation_collision_report(&self) -> Option<VegetationCollisionReport> {
        self.physics.borrow().as_ref()?;
        Some(self.vegetation_collision.report())
    }

    /// Clears vegetation authority and joins every pending cell-load worker. Every promoted
    /// plant demotes first — with its state written back through the reducer while the authority
    /// is still live — and a still-live play world then sheds every vegetation collision body.
    pub fn clear_vegetation(&mut self, scene: &mut Scene) -> Result<(), VegetationRuntimeError> {
        let mut world_ref = self.physics.borrow_mut();
        let vegetation_cell = Rc::clone(&self.vegetation);
        let mut vegetation_ref = vegetation_cell.borrow_mut();
        if let Some(vegetation) = vegetation_ref.as_mut() {
            self.vegetation_promotion
                .demote_all(vegetation, scene, world_ref.as_mut());
        } else {
            self.vegetation_promotion.abandon(scene, world_ref.as_mut());
        }
        if let Some(world) = world_ref.as_mut() {
            self.vegetation_collision.remove_all(world);
        }
        drop(world_ref);
        self.vegetation_navigation.clear();
        self.vegetation_families.clear();
        self.vegetation_event_cursor = 0;
        self.vegetation_scheduler.clear(&mut vegetation_ref)?;
        self.vegetation_status = VegetationRuntimeBindingStatus::Unavailable {
            reason: VegetationRuntimeUnavailableReason::NoProject,
            detail: None,
        };
        Ok(())
    }

    /// Current closed binding state for control/UI inspection.
    #[must_use]
    pub fn vegetation_status(&self) -> &VegetationRuntimeBindingStatus {
        &self.vegetation_status
    }

    /// Drains cells whose disposable CAS artifact must be regenerated through the shared cooker.
    pub fn missing_vegetation_cells(&self) -> Vec<saffron_spatial::WorldCellKey> {
        self.vegetation_scheduler.missing_cells()
    }

    /// Whether a deleted disposable cell artifact is being rebuilt through the shared cooker.
    #[must_use]
    pub fn vegetation_needs_regeneration(&self) -> bool {
        self.vegetation_scheduler.needs_regeneration()
    }

    /// Advances missing-cell regeneration through the one staged cooker and atomic commit path.
    pub fn regenerate_missing_vegetation(
        &mut self,
        assets: &mut AssetServer,
        surface_providers: &[Arc<dyn saffron_spatial::SurfaceField>],
    ) -> Result<(), VegetationRuntimeError> {
        let vegetation_cell = Rc::clone(&self.vegetation);
        let mut vegetation_ref = vegetation_cell.borrow_mut();
        self.vegetation_scheduler
            .regenerate_missing(&mut vegetation_ref, assets, surface_providers)
    }

    /// Whether a live world is present.
    #[must_use]
    pub fn has_physics(&self) -> bool {
        self.physics.borrow().is_some()
    }

    /// Whether a script VM is live.
    #[must_use]
    pub fn script_vm_active(&self) -> bool {
        self.script_vm_active
    }

    /// Whether the Jolt process globals are still flagged installed.
    #[must_use]
    pub fn physics_init(&self) -> bool {
        self.physics_init
    }

    /// The live script instance count.
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.script.instance_count()
    }

    /// The per-tick contact dispatch high-water cursor.
    #[must_use]
    pub fn contact_cursor(&self) -> u64 {
        self.contact_cursor
    }

    /// The per-tick vegetation-transition dispatch high-water cursor.
    #[must_use]
    pub fn vegetation_event_cursor(&self) -> u64 {
        self.vegetation_event_cursor
    }

    /// The animation runtime (for tests / the host's preview-prune assertions).
    #[must_use]
    pub fn animation(&self) -> &AnimationRuntime {
        &self.animation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_scene::{Collider, Rigidbody, Transform};
    use std::sync::Mutex;

    // The Jolt `Factory::sInstance` is a process global; serialize the tests that build/drop a
    // world so they never race it. Recover from a poisoned lock (it only guards the global init).
    static JOLT_GLOBAL: Mutex<()> = Mutex::new(());
    fn jolt_guard() -> std::sync::MutexGuard<'static, ()> {
        JOLT_GLOBAL.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn scratch_root(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("saffron-runtime-{tag}-{}", std::process::id()))
    }

    #[test]
    fn idle_session_has_no_world_or_vm() {
        let session = RuntimeSession::new();
        assert!(!session.has_physics());
        assert!(!session.script_vm_active());
        assert!(!session.physics_init());
        assert_eq!(session.instance_count(), 0);
    }

    /// Start builds a world from the scene and steps it; the dynamic box falls under gravity and
    /// settles on the static floor, and stop drops the world. The CPU-only mirror of
    /// `physics-falling-box.test.ts`.
    #[test]
    fn start_builds_a_world_and_steps_the_box() {
        let _guard = jolt_guard();
        match World::new() {
            Ok(_) => {}
            Err(err) => {
                eprintln!("skipping: World::new failed: {err}");
                return;
            }
        }

        let mut scene = Scene::new();
        let floor = scene.create_entity("Floor");
        scene
            .add_component(
                floor,
                Collider {
                    half_extents: glam::Vec3::new(10.0, 0.1, 10.0),
                    ..Collider::default()
                },
            )
            .expect("floor collider");
        let cube = scene.create_entity("Box");
        scene
            .add_component(
                cube,
                Transform {
                    translation: glam::Vec3::new(0.0, 5.0, 0.0),
                    scale: glam::Vec3::ONE,
                    rotation: glam::Vec3::ZERO,
                },
            )
            .expect("box transform");
        scene
            .add_component(cube, Collider::default())
            .expect("box collider");
        scene
            .add_component(cube, Rigidbody::default())
            .expect("box rigidbody");

        let mut assets = AssetServer::new(scratch_root("falling-box"));
        let mut session = RuntimeSession::new();
        session.start(
            &mut scene,
            &mut assets,
            std::path::Path::new("/nonexistent-project"),
        );
        assert!(session.has_physics(), "the world is live after start");

        let mut input = ScriptInputState::default();
        for _ in 0..200 {
            session.step(&mut scene, 0.016, &mut input);
        }
        let settled_y = scene.world_matrix(cube).w_axis.y;
        assert!(settled_y < 5.0, "the box fell from 5: now {settled_y}");
        assert!(
            (0.4..1.0).contains(&settled_y),
            "the box settled at ~floor-top + half-extent: {settled_y}"
        );

        session.stop();
        assert!(!session.has_physics(), "the world dropped on stop");
        session.shutdown_physics_globals();
        assert!(!session.physics_init(), "the Jolt globals shut down");
    }
}
