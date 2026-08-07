//! The owned control context: the command registry plus the (optional) socket
//! server, and the once-per-frame `poll` entry the host calls.

use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use saffron_assets::AssetServer;
use saffron_physics::World;
use saffron_sceneedit::SceneEditContext;
use saffron_spatial::ResidencyManager;
use saffron_vegetation::VegetationWorld;
use saffron_window::Window;

use crate::error::Result;
use crate::project_loader::ProjectLoader;
use crate::registry::{
    CommandRegistry, ControlRenderer, EngineContext, VegetationComputeExecutor,
    register_builtin_commands,
};
use crate::server::{ControlServer, control_socket_path, start_control_server};
use crate::vegetation_cook_jobs::VegetationCookJobs;
use crate::vegetation_jobs::VegetationEvaluationJobs;

/// Live subsystem borrows and vegetation state consumed by one control-plane drain.
pub struct ControlPollContext<'a> {
    /// Window command target.
    pub window: &'a mut Window,
    /// Renderer command target and GPU upload seam.
    pub renderer: &'a mut dyn ControlRenderer,
    /// Editor scene and selection state.
    pub scene_edit: &'a mut SceneEditContext,
    /// Project asset server.
    pub assets: &'a mut AssetServer,
    /// Shared spatial residency manager.
    pub spatial: &'a mut ResidencyManager,
    /// Published vegetation world, when one is bound.
    pub vegetation: &'a mut Option<VegetationWorld>,
    /// Runtime vegetation binding state.
    pub vegetation_status: saffron_runtime::VegetationRuntimeBindingStatus,
    /// Cells whose derived vegetation needs regeneration.
    pub vegetation_regeneration_cells: Vec<saffron_spatial::WorldCellKey>,
    /// Collision-facet residency counters, present only with a live play world.
    pub vegetation_collision: Option<saffron_runtime::VegetationCollisionReport>,
    /// The promotion authority, present only with a live play world.
    pub vegetation_promotion: Option<&'a mut saffron_runtime::VegetationPromotion>,
    /// The navigation contribution seam.
    pub vegetation_navigation: Option<&'a mut saffron_runtime::VegetationNavigationSeam>,
    /// The world simulation clock biology advances on.
    pub vegetation_ecology: Option<&'a mut saffron_runtime::VegetationEcologyClock>,
    /// The vegetation runtime's compact telemetry.
    pub vegetation_telemetry: Option<&'a mut saffron_runtime::VegetationTelemetry>,
    /// Live play-mode physics world, absent in edit mode.
    pub physics: Option<&'a mut World>,
}

/// Owns the command registry and the listening socket. The registry is built once at startup; the
/// `EngineContext` is rebuilt each frame in [`ControlContext::poll`].
///
/// A bind failure is non-fatal: the context runs inactive with `server: None`.
pub struct ControlContext {
    registry: CommandRegistry,
    server: Option<ControlServer>,
    /// The once-per-frame non-blocking project loader, advanced from the host each frame.
    loader: ProjectLoader,
    vegetation_jobs: VegetationEvaluationJobs,
    vegetation_cook_jobs: VegetationCookJobs,
    vegetation_compute: Option<Option<VegetationComputeExecutor>>,
}

impl Default for ControlContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlContext {
    /// Registers the builtin commands and binds the control socket. If the bind
    /// fails, the context is still returned (inactive) so the host keeps running.
    #[must_use]
    pub fn new() -> Self {
        let mut registry = CommandRegistry::new();
        register_builtin_commands(&mut registry);

        let server = match start_control_server(control_socket_path()) {
            Ok(server) => {
                tracing::info!("control socket listening on {}", server.path());
                Some(server)
            }
            Err(error) => {
                tracing::warn!("control socket disabled: {error}");
                None
            }
        };

        Self {
            registry,
            server,
            loader: ProjectLoader::default(),
            vegetation_jobs: VegetationEvaluationJobs::default(),
            vegetation_cook_jobs: VegetationCookJobs::default(),
            vegetation_compute: None,
        }
    }

    /// Whether the socket bound successfully and the context is serving.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.server.is_some()
    }

    /// Closes the listening socket so it stops serving, keeping the registry so a late
    /// palette/manifest read still resolves. Idempotent.
    pub fn shutdown(&mut self) {
        self.server = None;
        self.vegetation_jobs.shutdown();
        self.vegetation_cook_jobs.shutdown();
    }

    /// The command registry (for the manifest / command-palette generators).
    #[must_use]
    pub fn registry(&self) -> &CommandRegistry {
        &self.registry
    }

    /// Registers a typed command after the builtins, for the one host-owned command
    /// (`get-script-schema`) the control crate must not carry itself (it needs the Lua
    /// schema reader, and only the host may depend on `saffron-script`). Mirrors
    /// [`CommandRegistry::register`]; the wire encoding is applied there.
    pub fn register<P, R>(
        &mut self,
        name: &'static str,
        help: &'static str,
        handler: impl Fn(&mut EngineContext<'_>, P) -> Result<R> + 'static,
    ) where
        P: DeserializeOwned + JsonSchema + 'static,
        R: Serialize,
    {
        self.registry.register(name, help, handler);
    }

    /// Seeds the project-load inbox from the editor-set environment once at startup. The load runs
    /// non-blocking through [`Self::advance_project_load`] on the first frames, so startup never
    /// blocks.
    pub fn bootstrap_project_from_env(&mut self, scene_edit: &mut SceneEditContext) {
        crate::commands_asset::bootstrap_project_from_env(scene_edit);
    }

    /// Advances the non-blocking project loader one bounded step. Called every frame from the host
    /// (right after [`Self::poll`]). Returns `true` when it did redraw-worthy work.
    pub fn advance_project_load(
        &mut self,
        renderer: &mut dyn ControlRenderer,
        scene_edit: &mut SceneEditContext,
        assets: &mut AssetServer,
    ) -> bool {
        if scene_edit.project_load_inbox.is_some() || !scene_edit.project_ready() {
            self.vegetation_cook_jobs.shutdown();
        }
        self.loader.advance(renderer, scene_edit, assets)
    }

    /// Drains and runs any pending control requests on the calling (main) thread. Call once per
    /// frame with the live subsystem borrows; a no-op when the socket failed to bind.
    ///
    /// `true` when at least one mutating command completed `ok` this drain, so the host can request
    /// a viewport redraw while a static scene's read-only pollers leave the GPU idle.
    pub fn poll(&mut self, context: ControlPollContext<'_>) -> bool {
        let ControlPollContext {
            window,
            renderer,
            scene_edit,
            assets,
            spatial,
            vegetation,
            vegetation_status,
            vegetation_regeneration_cells,
            vegetation_collision,
            vegetation_promotion,
            vegetation_navigation,
            vegetation_ecology,
            vegetation_telemetry,
            physics,
        } = context;
        let mut mutated = false;
        let ready = match self.vegetation_cook_jobs.poll_ready() {
            Ok(ready) => ready,
            Err(error) => {
                tracing::error!("vegetation cook manager failed: {error}");
                Vec::new()
            }
        };
        for ready_cook in ready {
            let mut snapshots = None;
            renderer.with_gpu_uploader(&mut |gpu| {
                snapshots = Some(saffron_assets::scene_surface_field_snapshots(
                    gpu,
                    scene_edit.active_scene(),
                    assets,
                ));
            });
            let result = match snapshots {
                Some(Ok(surfaces)) => saffron_assets::commit_staged_vegetation_cook(
                    assets,
                    &surfaces,
                    ready_cook.staged,
                    &ready_cook.cancellation,
                ),
                Some(Err(error)) => Err(error),
                None => Err(saffron_assets::Error::Io(
                    "renderer did not provide a vegetation surface snapshot".to_owned(),
                )),
            };
            mutated |= result.is_ok();
            if let Err(error) = self
                .vegetation_cook_jobs
                .complete_commit(ready_cook.job, result)
            {
                tracing::error!("vegetation cook commit state failed: {error}");
            }
        }
        let Some(server) = self.server.as_mut() else {
            return mutated;
        };
        let mut ctx = EngineContext {
            window,
            renderer,
            scene_edit,
            assets,
            spatial,
            vegetation,
            vegetation_status,
            vegetation_regeneration_cells,
            vegetation_collision,
            vegetation_promotion,
            vegetation_navigation,
            vegetation_ecology,
            vegetation_telemetry,
            physics,
            vegetation_jobs: &mut self.vegetation_jobs,
            vegetation_cook_jobs: &mut self.vegetation_cook_jobs,
            vegetation_compute: &mut self.vegetation_compute,
        };
        let registry = &self.registry;
        server.drain(|line| match saffron_json::parse_json(line) {
            Ok(request) => {
                let reply = registry.dispatch(&mut ctx, &request);
                // A command that changed rendered state requests a redraw; reads (the editor's
                // per-frame pollers) are allow-listed so a static viewport stays GPU-quiet. Only a
                // successful (`ok`) non-read-only command counts — a failed mutation changed nothing.
                if reply.get("ok").and_then(Value::as_bool) == Some(true) {
                    let cmd = request.get("cmd").and_then(Value::as_str).unwrap_or("");
                    if !crate::registry::is_read_only_command(cmd) {
                        mutated = true;
                    }
                }
                saffron_json::dump_json(&reply, -1)
            }
            Err(_) => {
                let reply = crate::registry::failure_reply(
                    Value::Null,
                    crate::Error::InvalidRequest("invalid JSON request".to_owned()),
                );
                saffron_json::dump_json(&reply, -1)
            }
        });
        mutated
    }
}
