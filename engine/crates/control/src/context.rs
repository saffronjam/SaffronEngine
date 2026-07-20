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
use saffron_window::Window;

use crate::error::Result;
use crate::project_loader::ProjectLoader;
use crate::registry::{
    CommandRegistry, ControlRenderer, EngineContext, VegetationComputeExecutor,
    register_builtin_commands,
};
use crate::server::{ControlServer, control_socket_path, start_control_server};
use crate::vegetation_jobs::VegetationEvaluationJobs;

/// Owns the command registry and the listening socket. The registry is built
/// once at startup (it has no per-frame mutation); the `EngineContext` is rebuilt
/// each frame in [`ControlContext::poll`].
///
/// A bind failure is non-fatal: the context is constructed with `server: None`
/// and runs inactive, so the engine still runs without a control socket.
pub struct ControlContext {
    registry: CommandRegistry,
    server: Option<ControlServer>,
    /// The once-per-frame non-blocking project loader, advanced from the host each frame.
    loader: ProjectLoader,
    vegetation_jobs: VegetationEvaluationJobs,
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
            vegetation_compute: None,
        }
    }

    /// Whether the socket bound successfully and the context is serving.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.server.is_some()
    }

    /// Closes the listening socket (dropping its [`ControlServer`]) so it stops serving.
    ///
    /// The host calls this during teardown to release the socket promptly, before the
    /// renderer is dropped; the registry stays so a late palette/manifest read still
    /// resolves. Idempotent.
    pub fn shutdown(&mut self) {
        self.server = None;
        self.vegetation_jobs.shutdown();
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

    /// Seeds the project-load inbox from the editor-set environment once at startup: `SAFFRON_PROJECT`
    /// opens/creates a named project, else `SAFFRON_SCRATCH_PROJECT` makes a per-shell scratch
    /// project, else a working-directory `project.json` opens; otherwise nothing is seeded and the
    /// host waits for the editor's picker. The load itself runs non-blocking through
    /// [`Self::advance_project_load`] on the first frames — startup never blocks.
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
        self.loader.advance(renderer, scene_edit, assets)
    }

    /// Drains and runs any pending control requests on the calling (main) thread.
    /// Call once per frame with the live subsystem borrows. A no-op when the socket
    /// failed to bind.
    ///
    /// `physics` is the live play world (non-owning) or `None` in Edit.
    ///
    /// Returns `true` when at least one **mutating** command ran this drain (anything not
    /// [`is_read_only_command`] that completed `ok`), so the host can request a viewport redraw
    /// while a static scene's read-only pollers leave the GPU idle.
    pub fn poll(
        &mut self,
        window: &mut Window,
        renderer: &mut dyn ControlRenderer,
        scene_edit: &mut SceneEditContext,
        assets: &mut AssetServer,
        spatial: &mut ResidencyManager,
        physics: Option<&mut World>,
    ) -> bool {
        let Some(server) = self.server.as_mut() else {
            return false;
        };
        let mut ctx = EngineContext {
            window,
            renderer,
            scene_edit,
            assets,
            spatial,
            physics,
            vegetation_jobs: &mut self.vegetation_jobs,
            vegetation_compute: &mut self.vegetation_compute,
        };
        let registry = &self.registry;
        let mut mutated = false;
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
                // A non-JSON line gets the frozen invalid-request envelope, with
                // no `id` to echo.
                r#"{"ok":false,"error":"invalid JSON request"}"#.to_owned()
            }
        });
        mutated
    }
}
