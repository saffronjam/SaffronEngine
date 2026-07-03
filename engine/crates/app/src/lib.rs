//! The run loop and the `Layer` lifecycle — the bare runnable spine of a Saffron
//! Anima host.
//!
//! Provides the [`Layer`] trait (a set of optional lifecycle hooks), the [`App`] /
//! [`AppConfig`] types, and [`run`] — the
//! `poll → on_update → begin_frame → on_render → on_ui → begin_frame_graph →
//! on_render_graph → end_frame` loop with the `SAFFRON_EXIT_AFTER_FRAMES` frame
//! limit and the `wait_gpu_idle`-before-teardown ordering.
//!
//! ## Reactive pacing
//!
//! The loop is **reactive**, not free-running. A [`RedrawController`] on [`App`] decides each
//! iteration whether to render or skip: the host sets the per-frame activity in `on_update`
//! (continuous while an animation/physics sim runs or an edit smooths, a one-shot request when a
//! mutating control command lands), and the loop renders at the renderer's `target_fps`
//! ([`FrameHost::pace_target_fps`]) while active, holds a brief keep-warm window after the last
//! activity, then drops to a poll-only idle (the GPU goes quiet, the last published frame stays on
//! screen). A layer-less app and the GPU-free test host default to *continuous* — they render every
//! frame as before.
//!
//! Two modes share one [`run`]: a **windowed** standalone host (a winit window +
//! a surface-bound renderer that presents through a real swapchain) and a
//! **headless** editor host (no window, a feature-selected device), decided from
//! `SAFFRON_EDITOR_NATIVE_VIEWPORT`. They share the per-frame body ([`step_frame`])
//! and the `start` / `finish` bring-up/teardown; only the loop driver differs — a
//! plain `while` ([`drive`]) headless vs a winit [`ApplicationHandler`]
//! ([`run_windowed`]) windowed, since winit 0.30 owns its own loop. There is no shm
//! publish, no overlay, and no control plane here — those belong to `saffron-host`,
//! which is a single [`Layer`] plus the lifecycle closures this loop invokes.
//!
//! ## Why a [`FrameHost`] trait
//!
//! The loop drives `begin_frame`/`end_frame`/`wait_gpu_idle` through the
//! [`FrameHost`] trait so the loop's hook-dispatch order and frame-limit/fps logic
//! are testable without a GPU (the verification gate runs on software llvmpipe, or
//! on no device at all in the unit tests). The real [`saffron_rendering::Renderer`]
//! implements it; the loop holds it as `Box<dyn FrameHost>` so [`App`] stays a
//! concrete type and [`Layer`] stays object-safe (`Box<dyn Layer>`). This is the
//! same "device selection is a parameter, not a fork" principle the renderer's
//! [`SurfaceSource`] follows — one [`run`], the host swapped behind a trait.
//!
//! DAG: depends on `saffron-core`, `saffron-window`, `saffron-rendering`.

#![deny(unsafe_code)]

use std::time::{Duration, Instant};

use saffron_core::TimeSpan;
use saffron_rendering::{RenderGraph, Renderer, SurfaceSource};
use saffron_window::{
    ActiveEventLoop, ApplicationHandler, ControlFlow, EventLoop, Window, WindowConfig, WindowEvent,
    WindowId,
};

/// Errors from host bring-up and the run loop.
///
/// The failures are typed so a caller can `match`; [`run`] collapses them to a
/// process exit code at the top of the stack.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The OS window could not be created (windowed mode only).
    #[error("failed to create window: {0}")]
    Window(#[from] saffron_window::Error),

    /// The Vulkan renderer could not be brought up.
    #[error("failed to create renderer: {0}")]
    Renderer(#[from] saffron_rendering::Error),

    /// The winit event loop (windowed mode only) failed to build or run.
    #[error("event loop failed: {0}")]
    EventLoop(String),
}

/// A `Result` whose error is this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// The per-frame GPU host the loop drives.
///
/// Abstracting the renderer's frame entry points behind a trait keeps [`run`]
/// GPU-free-testable: the real [`Renderer`] implements it, and a no-op host drives
/// the loop in unit tests. There is exactly one loop; only the host behind it
/// differs.
pub trait FrameHost {
    /// Begins a frame: acquires the swapchain image (or, in publish mode, the
    /// staging slot) and prepares per-frame state. Returns `false` when the frame
    /// must be skipped (e.g. the swapchain went out of date).
    ///
    /// # Errors
    ///
    /// Propagates any host failure that should abort the loop.
    fn begin_frame(&mut self) -> Result<bool>;

    /// Builds the frame graph (cull + scene passes) into `graph` so layers may add
    /// passes against it in [`Layer::on_render_graph`]. The loop owns the graph for
    /// the hook pass and hands it to [`Self::end_frame`].
    fn begin_frame_graph(&mut self, graph: &mut RenderGraph);

    /// Finishes the frame: executes the graph (deriving every barrier) and
    /// presents or publishes the result.
    ///
    /// # Errors
    ///
    /// Propagates any host failure that should abort the loop.
    fn end_frame(&mut self, graph: RenderGraph) -> Result<()>;

    /// The current viewport size in pixels. A zero in either axis means the host
    /// is minimized and the frame is skipped.
    fn viewport_size(&self) -> (u32, u32);

    /// Blocks until the GPU is idle. Called once before any teardown so no
    /// in-flight command buffer references a resource about to drop — the single
    /// most load-bearing teardown ordering fact.
    ///
    /// # Errors
    ///
    /// Propagates a device-lost or wait failure.
    fn wait_gpu_idle(&self) -> Result<()>;

    /// Handles a window resize to `(width, height)` pixels: rebuilds the swapchain
    /// and the active offscreen view so the present path matches the new surface.
    /// The default is a no-op — the headless editor host has no swapchain and never
    /// receives a resize (it has no window).
    fn resized(&mut self, _width: u32, _height: u32) {}

    /// Exposes the concrete [`Renderer`] when this host is the real GPU renderer,
    /// `None` for the GPU-free test host.
    ///
    /// The host (`saffron-host`) reaches it to drive `render_scene` /
    /// `submit_overlay` / `set_viewport_size` and to build the per-frame control +
    /// scene-render seams, which live above this crate in the DAG (so they cannot
    /// be folded into [`FrameHost`]). The provided default is `None`, so a
    /// non-renderer host (the unit-test stubs) opts out for free.
    fn renderer_mut(&mut self) -> Option<&mut Renderer> {
        None
    }

    /// Records this frame's wall-clock delta (seconds) so the host's `render-stats` reports
    /// live frame timing + fps. The default is a no-op for the GPU-free test host; the real
    /// [`Renderer`] folds it into its smoothed frame time.
    fn record_frame_timing(&mut self, _dt_seconds: f32) {}

    /// Finalizes this frame's telemetry once the frame is rendered: folds the CPU busy/wait
    /// split (seconds; the loop's update+render window minus the GPU fence-wait) into the
    /// smoothed `cpuFrameMs`/`cpuWaitMs`, pushes the raw frame into the history ring, runs the
    /// perf-alarm detectors, and advances the profiler-capture state machine. `dt_seconds` is
    /// the wall-clock delta since the prior frame (drives the alarm EMA). The default is a
    /// no-op for the GPU-free test host; the real [`Renderer`] folds it all in.
    fn finalize_frame_telemetry(
        &mut self,
        _busy_seconds: f32,
        _wait_seconds: f32,
        _dt_seconds: f32,
    ) {
    }

    /// The render rate the reactive loop paces to while active, in frames per second, or `None`
    /// to run uncapped. The real [`Renderer`] returns its `target_fps` (the perf-config field the
    /// editor drives over the control plane); the GPU-free test host returns `None` so unit tests
    /// run flat-out. A non-positive target reads as uncapped.
    fn pace_target_fps(&self) -> Option<f64> {
        None
    }
}

impl FrameHost for Renderer {
    fn begin_frame(&mut self) -> Result<bool> {
        // Two modes share one loop. The standalone windowed host acquires the swapchain image
        // here, lets the host render the scene offscreen in `on_ui`, and blits that offscreen
        // onto the acquired image at `end_frame`. The editor / headless host has no swapchain —
        // it renders offscreen and publishes the read-back to shared memory — so `begin_frame`
        // only waits the slot fence and signals the loop to run its hooks.
        if self.swapchain().is_some() {
            // Wait the slot fence + acquire the swapchain image; the offscreen render in the
            // host's `on_ui` then records + submits without re-waiting, and `end_frame` blits +
            // presents.
            Ok(self.begin_present_frame()?)
        } else {
            // Wait the frame slot's fence now, before the host's `on_render`/`on_ui` hooks
            // reset any per-frame GPU state (the skinning descriptor pool), so the slot is
            // idle first. `render_scene_offscreen` then records + submits without re-waiting.
            self.begin_offscreen_frame()?;
            Ok(true)
        }
    }

    fn begin_frame_graph(&mut self, _graph: &mut RenderGraph) {}

    fn end_frame(&mut self, _graph: RenderGraph) -> Result<()> {
        // The windowed standalone host blits the offscreen the host rendered in `on_ui` onto
        // the swapchain image acquired in `begin_frame`, then presents. The editor / headless
        // host published its frame to shared memory in `on_ui` and has no swapchain to present.
        if self.swapchain().is_some() {
            self.present_active_view_to_swapchain()?;
        }
        Ok(())
    }

    fn viewport_size(&self) -> (u32, u32) {
        // The windowed host's viewport is the swapchain extent; the editor/headless host's
        // is the active offscreen view (the editor owns the render size over the control
        // plane). Either way a non-zero size keeps the loop's minimized guard happy.
        (self.viewport_width(), self.viewport_height())
    }

    fn wait_gpu_idle(&self) -> Result<()> {
        Ok(self.device().wait_idle()?)
    }

    fn resized(&mut self, width: u32, height: u32) {
        // Only the windowed standalone host has a swapchain to rebuild; a zero extent
        // is a minimize, which the next frame's `viewport_size` guard skips anyway.
        if self.swapchain().is_none() || width == 0 || height == 0 {
            return;
        }
        if let Err(err) = self.recreate_swapchain(width, height) {
            tracing::error!("swapchain recreate failed: {err}");
        }
        // The present blit's source is the active offscreen view; track the window so
        // the presented image is rendered at native resolution, not scaled.
        let view = self.active_view_id();
        if let Err(err) = self.set_viewport_desired_size(view, width, height) {
            tracing::error!("viewport resize failed: {err}");
        }
    }

    fn renderer_mut(&mut self) -> Option<&mut Renderer> {
        Some(self)
    }

    fn record_frame_timing(&mut self, dt_seconds: f32) {
        self.observe_frame_delta(dt_seconds);
    }

    fn finalize_frame_telemetry(&mut self, busy_seconds: f32, wait_seconds: f32, dt_seconds: f32) {
        self.finalize_frame_telemetry(busy_seconds * 1000.0, wait_seconds * 1000.0, dt_seconds);
    }

    fn pace_target_fps(&self) -> Option<f64> {
        let target = f64::from(self.perf_config().target_fps);
        match self.power_state().pace_fps_cap() {
            Some(cap) => Some(target.min(cap)),
            None => Some(target),
        }
    }
}

/// How long the loop keeps rendering at the full rate after the last activity, before dropping to
/// idle. The anti-downclock-stutter / post-interaction-smoothness window (a locked decision: never
/// hard-stop the loop the instant activity ceases).
const KEEP_WARM: Duration = Duration::from_millis(600);

/// Rendered frames a temporal effect (TAA / SSGI history) needs after an invalidation to converge.
/// While temporal accumulation is active, the loop renders at least this many frames after the last
/// activity so a static viewport settles to its converged image before idling on it — the
/// "converge-then-stop" half of the reactive loop. At a low target fps this outlasts the wall-clock
/// [`KEEP_WARM`]; at a high one [`KEEP_WARM`] dominates. With no temporal effect on, convergence is
/// immediate (the keep-warm alone applies).
const CONVERGE_FRAMES: u32 = 24;

/// The poll interval while fully idle: the loop still drains the control socket this often (so a
/// command wakes the viewport promptly) but issues no GPU work, so the device goes quiet.
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(8);

/// Decides, each loop iteration, whether to render a frame or skip it.
///
/// The host sets the per-frame activity in `on_update`: [`RedrawController::set_continuous`] while
/// something evolves on its own (an animation/physics sim, an edit smoothing),
/// [`RedrawController::request_redraw`] as a one-shot when a mutating control command lands, and
/// [`RedrawController::set_temporal_active`] when TAA / SSGI accumulation is on. The loop then asks
/// [`RedrawController::poll_should_render`]: it renders while active, then keeps rendering until
/// **both** the wall-clock [`KEEP_WARM`] window has elapsed **and** the temporal effects have had
/// [`CONVERGE_FRAMES`] frames to settle, then idles holding the converged frame.
///
/// The default is `continuous` — a layer-less app and the GPU-free test host render every frame, so
/// only a host that opts into reactivity ever idles.
pub struct RedrawController {
    /// Set each frame by the host: `true` while some state evolves without a new command.
    continuous: bool,
    /// One-shot: a mutating command landed this frame; render it (consumed by the decision).
    dirty: bool,
    /// Whether a temporal effect (TAA / SSGI history) is accumulating — gates the convergence window.
    temporal_active: bool,
    /// Hard override: while set (the viewport is occluded / minimized) the loop renders nothing,
    /// regardless of activity — the host drives it from the editor's window-visibility signal.
    suppressed: bool,
    /// When activity (continuous or dirty) was last seen, for the keep-warm window.
    last_activity: Option<Instant>,
    /// Rendered frames since the last activity — the convergence progress counter.
    frames_since_activity: u32,
    /// The verdict the last [`Self::poll_should_render`] returned — the accurate idle readout.
    last_rendered: bool,
    /// The named reasons currently forcing continuous render, for observability (Phase 5).
    reasons: Vec<&'static str>,
}

impl Default for RedrawController {
    fn default() -> Self {
        Self {
            continuous: true,
            dirty: false,
            temporal_active: false,
            suppressed: false,
            last_activity: None,
            frames_since_activity: 0,
            last_rendered: false,
            reasons: Vec::new(),
        }
    }
}

impl RedrawController {
    /// Sets whether some state is evolving on its own this frame (animation/physics/smoothing) —
    /// the host calls this every `on_update`. `true` forces a render and resets convergence.
    pub fn set_continuous(&mut self, on: bool) {
        self.continuous = on;
    }

    /// Sets whether a temporal effect (TAA / SSGI history) is accumulating this frame, so the loop
    /// renders a convergence window after activity instead of stopping the instant motion ceases.
    pub fn set_temporal_active(&mut self, on: bool) {
        self.temporal_active = on;
    }

    /// Hard-suppresses all rendering (the viewport is occluded / minimized): the loop idles
    /// regardless of activity until cleared. The host drives it from the editor's visibility signal.
    pub fn set_suppressed(&mut self, on: bool) {
        self.suppressed = on;
    }

    /// Whether the loop is currently idling rather than rendering — the verdict the last
    /// [`Self::poll_should_render`] returned (so it accounts for the keep-warm window and
    /// suppression, not just the activity flags).
    #[must_use]
    pub fn is_idle(&self) -> bool {
        !self.last_rendered
    }

    /// Records the named reasons (for the Phase-5 observability readout) that the host is holding
    /// continuous render this frame. Purely informational; [`Self::set_continuous`] drives the
    /// decision.
    pub fn set_reasons(&mut self, reasons: Vec<&'static str>) {
        self.reasons = reasons;
    }

    /// The reasons continuous render is currently held (empty when idle).
    #[must_use]
    pub fn reasons(&self) -> &[&'static str] {
        &self.reasons
    }

    /// Whether the temporal effects have converged: not actively driven and past the convergence
    /// window since the last invalidation (the Phase-5 observability bit).
    #[must_use]
    pub fn converged(&self) -> bool {
        !self.continuous && (!self.temporal_active || self.frames_since_activity >= CONVERGE_FRAMES)
    }

    /// Requests one render next decision (a mutating control command landed). Resets convergence.
    pub fn request_redraw(&mut self) {
        self.dirty = true;
    }

    /// Resolves the render-or-skip verdict for this iteration at `now`, consuming the one-shot dirty
    /// flag. Renders while active (continuous or a pending request); after activity, keeps rendering
    /// until both the keep-warm window has elapsed and the temporal effects have converged
    /// ([`CONVERGE_FRAMES`] frames); idles otherwise, holding the converged frame.
    fn poll_should_render(&mut self, now: Instant) -> bool {
        let render = self.decide(now);
        self.last_rendered = render;
        render
    }

    /// The render-or-skip decision (factored out so [`Self::poll_should_render`] can record it).
    fn decide(&mut self, now: Instant) -> bool {
        // Occluded / minimized: render nothing, whatever the activity.
        if self.suppressed {
            self.dirty = false;
            return false;
        }
        let active = self.continuous || self.dirty;
        self.dirty = false;
        if active {
            self.last_activity = Some(now);
            self.frames_since_activity = 0;
            return true;
        }
        let warm = self
            .last_activity
            .is_some_and(|t| now.saturating_duration_since(t) < KEEP_WARM);
        let converging = self.temporal_active && self.frames_since_activity < CONVERGE_FRAMES;
        let render = warm || converging;
        if render {
            self.frames_since_activity = self.frames_since_activity.saturating_add(1);
        }
        render
    }
}

/// The running application state the loop and the [`Layer`] hooks share.
///
/// Field order encodes teardown: [`Self::frame_host`] (the renderer) drops before
/// [`Self::window`]. The layer vec rides on `App` but the loop `mem::take`s it for
/// the duration of each hook pass, so a hook borrows `&mut App` (window + frame
/// host) without aliasing the list being iterated.
pub struct App {
    /// The per-frame GPU host (the renderer), behind a trait so the loop is
    /// GPU-free-testable.
    pub frame_host: Box<dyn FrameHost>,
    /// The OS window, present only in the standalone windowed mode. Editor /
    /// headless mode runs windowless (`None`).
    pub window: Option<Window>,
    /// The loop's run latch; a layer or signal handler sets it `false` to exit.
    pub running: bool,
    /// The reactive-render verdict source; the host drives it each `on_update`, the loop reads it
    /// in [`step_frame`]. Defaults to continuous, so a layer-less app renders every frame.
    pub redraw: RedrawController,
    /// The attached layers. `attach_layer` pushes here; the loop `mem::take`s the
    /// vec out for each hook pass and restores it, so it is empty *during* a hook.
    layers: Vec<Box<dyn Layer>>,
}

impl App {
    /// Builds an app over a frame host and an optional window, with no layers
    /// attached yet (`on_create` attaches them).
    fn new(frame_host: Box<dyn FrameHost>, window: Option<Window>) -> Self {
        Self {
            frame_host,
            window,
            running: false,
            redraw: RedrawController::default(),
            layers: Vec::new(),
        }
    }
}

/// A set of lifecycle hooks — the runtime "interface" a client implements as a
/// trait with provided (default-empty) methods.
///
/// A client implements only the hooks it needs; the empties cost nothing. Stored
/// as `Box<dyn Layer>` because the layer set is open and client-extensible (the
/// host is itself one [`Layer`]). Each hook takes `&mut App` rather than capturing
/// it, so a layer never aliases the app it runs inside.
pub trait Layer {
    /// The layer's name, for logs. Defaults to `"Layer"`.
    fn name(&self) -> &str {
        "Layer"
    }

    /// Runs once after the window + renderer exist and the layer is attached.
    fn on_attach(&mut self, _app: &mut App) {}

    /// Runs once per frame before rendering, with the frame delta.
    fn on_update(&mut self, _app: &mut App, _dt: TimeSpan) {}

    /// Submits GPU work for the frame (recorded into the current frame's command
    /// buffer via the renderer's submit seam).
    fn on_render(&mut self, _app: &mut App) {}

    /// Builds UI / overlay geometry for the frame.
    fn on_ui(&mut self, _app: &mut App) {}

    /// Adds passes to the frame's render graph (e.g. post-process).
    fn on_render_graph(&mut self, _app: &mut App, _graph: &mut RenderGraph) {}

    /// Runs once during teardown, before the renderer is dropped.
    fn on_detach(&mut self, _app: &mut App) {}
}

/// Client-provided configuration handed to [`run`].
///
/// `on_create` runs once the window + renderer exist (attach layers, wire signals
/// there); `on_exit` runs during teardown after `wait_gpu_idle`. The closures are
/// boxed so the config is a plain owned value.
pub struct AppConfig {
    /// The window parameters for the standalone windowed mode (ignored in
    /// headless editor mode).
    pub window: WindowConfig,
    /// Runs once after bring-up; the host attaches its [`Layer`] and wires signals
    /// here.
    pub on_create: Box<dyn FnOnce(&mut App)>,
    /// Runs once during teardown, after `wait_gpu_idle`.
    pub on_exit: Box<dyn FnOnce(&mut App)>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            window: WindowConfig::default(),
            on_create: Box::new(|_| {}),
            on_exit: Box::new(|_| {}),
        }
    }
}

/// Whether the host runs windowed (standalone) or headless (the editor host).
///
/// Decided from `SAFFRON_EDITOR_NATIVE_VIEWPORT`: set → headless (no window, a
/// no-surface offscreen device — [`SurfaceSource::Offscreen`] — the frame path is
/// shm-publish). Unset → windowed standalone present-only host. This is a [`run`]-time
/// branch on the env var, not two `run` functions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HostMode {
    /// A winit window + a surface-bound renderer (the standalone present-only
    /// host).
    Windowed,
    /// No window + a no-surface offscreen device (the editor native-viewport host).
    Headless,
}

impl HostMode {
    /// Reads the mode from `SAFFRON_EDITOR_NATIVE_VIEWPORT` (the env var the
    /// editor sets when it spawns the host as its viewport producer).
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_present(std::env::var_os("SAFFRON_EDITOR_NATIVE_VIEWPORT").is_some())
    }

    /// Maps the presence of `SAFFRON_EDITOR_NATIVE_VIEWPORT` to a mode: present
    /// (any value, including empty) → headless, absent → windowed. Split out pure
    /// so it is testable without env mutation.
    #[must_use]
    fn from_present(present: bool) -> Self {
        if present {
            Self::Headless
        } else {
            Self::Windowed
        }
    }
}

/// Owns the main loop. Returns a process exit code (`0` on a clean exit, `1` on a
/// bring-up failure).
///
/// The mode (windowed vs headless) is read from the environment. A creation
/// failure logs via `saffron-core` and returns `1`; everything else runs the loop
/// to completion and returns `0`.
#[must_use]
pub fn run(config: AppConfig) -> i32 {
    match run_inner(config, HostMode::from_env()) {
        Ok(()) => 0,
        Err(err) => {
            tracing::error!("{err}");
            1
        }
    }
}

/// Brings up the host for `mode`, runs the loop, tears down. Split from [`run`] so
/// the bring-up failures stay typed (`?`) and [`run`] only collapses them to an
/// exit code.
///
/// Headless mode (the editor host) builds a no-surface offscreen device
/// ([`SurfaceSource::Offscreen`]) with no window and runs the plain [`drive`] loop.
/// Windowed mode (the standalone present-only host) needs a winit window, which winit
/// 0.30 only creates from inside an active event loop, so it hands off to
/// [`run_windowed`] — the renderer is then surface-bound and presents through a real
/// swapchain on the same feature-selected GPU. Both modes share the per-frame body and
/// the `wait_gpu_idle`-before-teardown ordering.
fn run_inner(config: AppConfig, mode: HostMode) -> Result<()> {
    match mode {
        HostMode::Headless => {
            let renderer = Renderer::new(
                &SurfaceSource::Offscreen,
                config.window.width,
                config.window.height,
            )?;
            let app = App::new(Box::new(renderer), None);
            drive(app, config, LoopLimits::from_env());
            Ok(())
        }
        HostMode::Windowed => run_windowed(config, LoopLimits::from_env()),
    }
}

/// The loop's frame-limit knob, read once from the environment.
///
/// Passed into [`drive`] so the loop body never reads the environment itself,
/// which keeps the loop tests free of process-global env mutation (the crate
/// denies `unsafe`, so `std::env::set_var` is unavailable here). The render *rate*
/// is paced by the reactive loop to the renderer's `target_fps`
/// ([`FrameHost::pace_target_fps`]).
#[derive(Clone, Copy, Default)]
struct LoopLimits {
    /// `SAFFRON_EXIT_AFTER_FRAMES`: exit after this many frames; `0` = no limit.
    frame_limit: u64,
}

impl LoopLimits {
    /// Reads the frame limit from the environment.
    fn from_env() -> Self {
        Self {
            frame_limit: frame_limit_from_env(),
        }
    }
}

/// The frame-clock state the per-frame step threads between iterations: the loop's
/// frame count and the previous-iteration instant (for `dt`). Shared by the plain
/// headless loop and the winit-driven windowed loop so both pace and count frames
/// identically.
struct FrameClock {
    frame_count: u64,
    last: Instant,
}

impl FrameClock {
    /// Starts the clock at `now`.
    fn new() -> Self {
        Self {
            frame_count: 0,
            last: Instant::now(),
        }
    }
}

/// Runs `on_create` then `on_attach` and latches the loop running. The shared
/// bring-up half both drivers call before their first frame.
fn start(app: &mut App, config: &mut AppConfig) {
    let on_create = std::mem::replace(&mut config.on_create, Box::new(|_| {}));
    on_create(app);
    run_hook(app, |layer, app| layer.on_attach(app));
    app.running = true;
}

/// `wait_gpu_idle`, then `on_detach`, then `on_exit` — the shared teardown half both
/// drivers call once the loop ends. `wait_gpu_idle` runs first so no in-flight
/// command buffer references a resource a handler is about to drop.
fn finish(app: &mut App, config: &mut AppConfig) {
    if let Err(err) = app.frame_host.wait_gpu_idle() {
        tracing::error!("wait_gpu_idle failed: {err}");
    }
    run_hook(app, |layer, app| layer.on_detach(app));
    let on_exit = std::mem::replace(&mut config.on_exit, Box::new(|_| {}));
    on_exit(app);
}

/// Runs one loop iteration: the `on_update` pass (where the host sets the reactive-render
/// verdict), then — only if a render is due — the timing record, `begin_frame` + render passes, and
/// the telemetry finalize, then the frame-limit check and the pacing sleep. Sets
/// `app.running = false` when the frame limit is hit or a frame fails. Shared by both drivers so the
/// hook order, minimized guard, reactive skip, telemetry split, and pacing are one implementation.
///
/// `on_update` (and so the control-socket drain) runs *every* iteration, including idle ones, so a
/// command lands within one [`IDLE_POLL_INTERVAL`] even while the GPU is quiet. Only the render is
/// gated by the [`RedrawController`] verdict.
fn step_frame(app: &mut App, limits: LoopLimits, clock: &mut FrameClock) {
    let now = Instant::now();
    let dt = TimeSpan::from_seconds((now - clock.last).as_secs_f32());
    clock.last = now;

    // The CPU busy window opens here (update + render) and closes after `run_frame`; the
    // GPU fence-wait inside `begin_frame` is the wait split. Both feed the renderer's
    // `cpuFrameMs`/`cpuWaitMs` EMA so `render-stats` reports a real CPU-side cost.
    let busy_start = Instant::now();
    run_hook(app, |layer, app| layer.on_update(app, dt));

    let (width, height) = app.frame_host.viewport_size();
    let minimized = width == 0 || height == 0;
    // The reactive verdict: render while the host reports activity or within the keep-warm window,
    // skip (holding the last published frame) when idle. A minimized host never renders.
    let render = !minimized && app.redraw.poll_should_render(now);

    let mut wait_seconds = 0.0;
    if render {
        // Feed the frame delta to the GPU host so `render-stats` reports live frame timing; only
        // rendered frames advance it, so idle does not pollute the fps EMA.
        app.frame_host.record_frame_timing(dt.seconds);
        let before_begin = Instant::now();
        match app.frame_host.begin_frame() {
            Ok(true) => {
                // `begin_frame` blocks on the slot's in-flight fence (the GPU-bound wait).
                wait_seconds = before_begin.elapsed().as_secs_f32();
                run_frame(app);
            }
            Ok(false) => {}
            Err(err) => {
                tracing::error!("begin_frame failed: {err}");
                app.running = false;
            }
        }
        // Busy = the whole update+render span minus the fence-wait; never negative.
        let busy_seconds = (busy_start.elapsed().as_secs_f32() - wait_seconds).max(0.0);
        // Finalize the frame's telemetry (CPU EMA + history ring + alarms + capture advance) only
        // for rendered frames, so the history + capture only see real frames.
        app.frame_host
            .finalize_frame_telemetry(busy_seconds, wait_seconds, dt.seconds);
    }

    clock.frame_count += 1;
    if limits.frame_limit != 0 && clock.frame_count >= limits.frame_limit {
        tracing::info!("frame limit reached ({}), exiting", limits.frame_limit);
        app.running = false;
    }

    pace_iteration(app.frame_host.as_ref(), render, now);
}

/// Runs the loop over an already-built [`App`]: `on_create`, `on_attach`, the
/// per-frame hook passes, then `wait_gpu_idle` before `on_detach` / `on_exit`.
///
/// The headless driver: a plain `while` loop, no window. Drives whatever
/// [`FrameHost`] the [`App`] holds, so the no-op host of the unit tests and the real
/// headless renderer share this body. The windowed standalone host uses
/// [`run_windowed`] instead (winit owns its loop), but both reuse [`start`],
/// [`step_frame`], and [`finish`].
fn drive(mut app: App, mut config: AppConfig, limits: LoopLimits) {
    start(&mut app, &mut config);

    let mut clock = FrameClock::new();
    while app.running {
        step_frame(&mut app, limits, &mut clock);
    }

    finish(&mut app, &mut config);
}

/// The standalone present-only host: a winit window + a surface-bound renderer that
/// presents through a real swapchain on the feature-selected GPU.
///
/// winit 0.30 only creates a window from inside an active event loop and owns the
/// loop itself, so this drives the frame body through an [`ApplicationHandler`]
/// ([`WindowedApp`]) rather than the plain `while` of [`drive`]. The window + the
/// [`SurfaceSource::Window`] renderer are built in `resumed` (the first time the
/// loop is ready), each frame runs in `about_to_wait`, and window events
/// (close / resize) feed the [`Window`]'s typed signals. The same [`start`] /
/// [`step_frame`] / [`finish`] the headless loop uses run here, so the hook order
/// and the `wait_gpu_idle`-before-teardown ordering are identical across modes.
fn run_windowed(config: AppConfig, limits: LoopLimits) -> Result<()> {
    let event_loop = EventLoop::new().map_err(|err| Error::EventLoop(err.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut windowed = WindowedApp {
        config: Some(config),
        limits,
        app: None,
        clock: FrameClock::new(),
        bring_up_error: None,
    };
    event_loop
        .run_app(&mut windowed)
        .map_err(|err| Error::EventLoop(err.to_string()))?;

    // A bring-up failure inside `resumed` (window or renderer creation) exits the
    // loop; surface it as the typed error so `run` collapses it to exit code 1.
    if let Some(err) = windowed.bring_up_error {
        return Err(err);
    }
    Ok(())
}

/// The winit [`ApplicationHandler`] that drives the windowed standalone host.
///
/// Holds the config until `resumed` consumes it to build the window + renderer, the
/// running [`App`] once built, the shared [`FrameClock`], and a deferred bring-up
/// error (a window/renderer failure in `resumed` cannot return through the winit
/// callback, so it is stashed and re-raised by [`run_windowed`]).
struct WindowedApp {
    config: Option<AppConfig>,
    limits: LoopLimits,
    app: Option<App>,
    clock: FrameClock,
    bring_up_error: Option<Error>,
}

impl ApplicationHandler for WindowedApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // `resumed` can fire more than once (a real desktop suspend/resume); build the
        // window + renderer only on the first, when the config is still present.
        let Some(mut config) = self.config.take() else {
            return;
        };

        let window = match Window::new(event_loop, &config.window) {
            Ok(window) => window,
            Err(err) => {
                self.bring_up_error = Some(err.into());
                event_loop.exit();
                return;
            }
        };
        let (width, height) = (window.width(), window.height());
        let renderer = match Renderer::new(&SurfaceSource::Window(&window), width, height) {
            Ok(renderer) => renderer,
            Err(err) => {
                self.bring_up_error = Some(err.into());
                event_loop.exit();
                return;
            }
        };

        let mut app = App::new(Box::new(renderer), Some(window));
        start(&mut app, &mut config);
        self.clock = FrameClock::new();
        self.config = Some(config);
        self.app = Some(app);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(app) = self.app.as_mut() else {
            return;
        };
        if let Some(window) = app.window.as_mut() {
            window.dispatch_window_event(&event);
        }
        match event {
            WindowEvent::CloseRequested => {
                app.running = false;
                event_loop.exit();
            }
            // A resize makes the swapchain out of date; rebuild it (and the offscreen
            // the present blits from) at the new surface size before the next frame.
            WindowEvent::Resized(size) => {
                app.frame_host.resized(size.width, size.height);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(app) = self.app.as_mut() else {
            return;
        };
        if app.window.as_ref().is_some_and(Window::should_close) {
            app.running = false;
        }
        if !app.running {
            event_loop.exit();
            return;
        }

        step_frame(app, self.limits, &mut self.clock);

        // A frame limit / failure inside `step_frame` clears `running`; exit promptly. `step_frame`
        // paces the iteration itself (rendering at `target_fps`, or sleeping an idle poll when the
        // viewport is quiet), so the `ControlFlow::Poll` loop never free-runs.
        if !app.running {
            event_loop.exit();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // The loop is ending: run the shared teardown (`wait_gpu_idle` → `on_detach` →
        // `on_exit`) exactly once, before the `App` (renderer) drops.
        if let (Some(mut app), Some(mut config)) = (self.app.take(), self.config.take()) {
            finish(&mut app, &mut config);
        }
    }
}

/// The render/ui/graph hook pass for one accepted frame: `on_render`, `on_ui`,
/// then `begin_frame_graph` + per-layer `on_render_graph`, then `end_frame`.
///
/// The render graph is owned here for the pass and moved into `end_frame`: the loop
/// hands it to each layer, then `end_frame` executes it.
fn run_frame(app: &mut App) {
    run_hook(app, |layer, app| layer.on_render(app));
    run_hook(app, |layer, app| layer.on_ui(app));

    let mut graph = RenderGraph::new();
    app.frame_host.begin_frame_graph(&mut graph);
    run_hook(app, |layer, app| layer.on_render_graph(app, &mut graph));
    if let Err(err) = app.frame_host.end_frame(graph) {
        tracing::error!("end_frame failed: {err}");
        app.running = false;
    }
}

/// Dispatches one hook across every attached layer.
///
/// Moves the layer vec out of `app` for the pass so `f` can borrow `&mut App`
/// (window + frame host + `running`) without aliasing the list, then restores it. A
/// hook that itself attaches a layer (via `attach_layer` on the
/// moved-out-then-restored vec) is picked up next pass, not mid-pass.
fn run_hook(app: &mut App, mut f: impl FnMut(&mut Box<dyn Layer>, &mut App)) {
    let mut layers = std::mem::take(&mut app.layers);
    for layer in &mut layers {
        f(layer, app);
    }
    // A hook may have attached new layers onto the (empty) `app.layers`; keep both
    // — the originals first, then any freshly attached.
    layers.append(&mut app.layers);
    app.layers = layers;
}

/// Sleeps to pace one loop iteration that started at `iter_start`.
///
/// A rendered frame paces to the host's `target_fps` ([`FrameHost::pace_target_fps`]); a `None` or
/// non-positive target runs uncapped (the GPU-free test host). A skipped (idle / minimized)
/// iteration sleeps one [`IDLE_POLL_INTERVAL`] so the loop keeps draining the control socket
/// without burning the CPU or waking the GPU. Pacing from the iteration start (not a running
/// accumulator) means a frame slower than the interval simply runs back-to-back — correct when the
/// GPU is the bottleneck.
fn pace_iteration(frame_host: &dyn FrameHost, rendered: bool, iter_start: Instant) {
    let deadline = if rendered {
        match frame_host.pace_target_fps() {
            Some(fps) if fps > 0.0 => iter_start + Duration::from_secs_f64(1.0 / fps),
            _ => return,
        }
    } else {
        iter_start + IDLE_POLL_INTERVAL
    };
    let now = Instant::now();
    if now < deadline {
        std::thread::sleep(deadline - now);
    }
}

/// Attaches a layer to the app.
///
/// Called from `AppConfig::on_create` (and legitimately from inside a hook). A
/// layer attached during a hook pass first runs its `on_attach`-less self on the
/// *next* pass (the running loop never replays `on_attach` mid-loop), so it only
/// joins the per-frame iteration after the attach phase.
pub fn attach_layer(app: &mut App, layer: Box<dyn Layer>) {
    app.layers.push(layer);
}

/// Parses `SAFFRON_EXIT_AFTER_FRAMES` strictly: a valid `u64` count, else `0`.
///
/// Trailing garbage (`"10x"`) is rejected (logged + ignored as `0`), and an unset
/// var is `0`. `0` means "no frame limit".
#[must_use]
pub fn frame_limit_from_env() -> u64 {
    let Some(raw) = std::env::var_os("SAFFRON_EXIT_AFTER_FRAMES") else {
        return 0;
    };
    let text = raw.to_string_lossy();
    match parse_strict_u64(&text) {
        Some(value) => value,
        None => {
            tracing::error!("invalid SAFFRON_EXIT_AFTER_FRAMES='{text}', ignoring");
            0
        }
    }
}

/// Parses `text` as a strict `u64`: the whole string must be a base-10 `u64` (no
/// trailing garbage, no sign, no whitespace). `0` is a valid value (no frame limit).
/// Returns `None` on any rejection. Split out pure so it is testable without mutating
/// the process environment (the crate denies `unsafe`, so `std::env::set_var` is
/// unavailable).
fn parse_strict_u64(text: &str) -> Option<u64> {
    text.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// A `FrameHost` that does no GPU work — drives the loop on no device.
    struct NoopFrameHost;

    impl FrameHost for NoopFrameHost {
        fn begin_frame(&mut self) -> Result<bool> {
            Ok(true)
        }
        fn begin_frame_graph(&mut self, _graph: &mut RenderGraph) {}
        fn end_frame(&mut self, _graph: RenderGraph) -> Result<()> {
            Ok(())
        }
        fn viewport_size(&self) -> (u32, u32) {
            (1, 1)
        }
        fn wait_gpu_idle(&self) -> Result<()> {
            Ok(())
        }
    }

    /// Builds an `App` over a no-op host with no window (the headless test spine).
    fn headless_app() -> App {
        App::new(Box::new(NoopFrameHost), None)
    }

    /// A config that attaches `layer` in `on_create` and runs `on_exit` after
    /// teardown.
    fn config_with(layer: Box<dyn Layer>, on_exit: Box<dyn FnOnce(&mut App)>) -> AppConfig {
        let layer = RefCell::new(Some(layer));
        AppConfig {
            window: WindowConfig::default(),
            on_create: Box::new(move |app| {
                if let Some(layer) = layer.borrow_mut().take() {
                    attach_layer(app, layer);
                }
            }),
            on_exit,
        }
    }

    /// The frame limit cannot be reached via env in this `unsafe`-free crate, so
    /// the loop tests pass `LoopLimits` explicitly.
    fn limited(frame_limit: u64) -> LoopLimits {
        LoopLimits { frame_limit }
    }

    #[test]
    fn empty_layer_defaults_are_noops() {
        struct Empty;
        impl Layer for Empty {}

        let mut layer = Empty;
        let mut app = headless_app();
        assert_eq!(layer.name(), "Layer");
        layer.on_attach(&mut app);
        layer.on_update(&mut app, TimeSpan::from_seconds(0.016));
        layer.on_render(&mut app);
        layer.on_ui(&mut app);
        let mut graph = RenderGraph::new();
        layer.on_render_graph(&mut app, &mut graph);
        layer.on_detach(&mut app);
    }

    #[test]
    fn frame_limit_parses_strictly() {
        // `SAFFRON_EXIT_AFTER_FRAMES`: `0` is a valid value (no limit).
        assert_eq!(parse_strict_u64("10"), Some(10), "valid count parses");
        assert_eq!(parse_strict_u64("0"), Some(0), "literal 0 is allowed");
        assert_eq!(parse_strict_u64("10x"), None, "trailing garbage rejected");
        assert_eq!(parse_strict_u64(""), None, "empty rejected");
        assert_eq!(parse_strict_u64("-1"), None, "sign rejected");
        assert_eq!(parse_strict_u64(" 5"), None, "whitespace rejected");
    }

    #[test]
    fn host_mode_maps_var_presence() {
        assert_eq!(
            HostMode::from_present(false),
            HostMode::Windowed,
            "absent → windowed"
        );
        // Presence (any value, including empty) selects headless.
        assert_eq!(
            HostMode::from_present(true),
            HostMode::Headless,
            "present → headless"
        );
    }

    #[test]
    fn redraw_controller_renders_while_active_then_idles_past_keep_warm() {
        let mut rc = RedrawController::default();
        let t0 = Instant::now();
        // Default is continuous → renders.
        assert!(rc.poll_should_render(t0), "default continuous renders");

        // Host marks idle; still within the keep-warm window after the last active frame → renders.
        rc.set_continuous(false);
        assert!(
            rc.poll_should_render(t0 + Duration::from_millis(100)),
            "renders inside keep-warm after activity"
        );

        // Past keep-warm with no activity → idles (skip render).
        rc.set_continuous(false);
        assert!(
            !rc.poll_should_render(t0 + KEEP_WARM + Duration::from_millis(200)),
            "idles once keep-warm elapses"
        );

        // A mutating command requests one render even while idle, and refreshes keep-warm.
        rc.set_continuous(false);
        rc.request_redraw();
        let t_late = t0 + KEEP_WARM + Duration::from_millis(400);
        assert!(
            rc.poll_should_render(t_late),
            "request_redraw forces a render"
        );
        rc.set_continuous(false);
        assert!(
            rc.poll_should_render(t_late + Duration::from_millis(50)),
            "the requested render refreshed keep-warm"
        );
    }

    #[test]
    fn temporal_convergence_renders_a_frame_window_past_keep_warm() {
        // With a temporal effect active, the loop keeps rendering a convergence window after the
        // last activity even once the wall-clock keep-warm has elapsed, so a static viewport settles
        // to its converged image before idling on it.
        let mut rc = RedrawController::default();
        rc.set_temporal_active(true);
        let t0 = Instant::now();

        rc.set_continuous(true);
        assert!(rc.poll_should_render(t0), "active frame renders");

        // Jump well past keep-warm: only the convergence window keeps it rendering now.
        let past = t0 + KEEP_WARM + Duration::from_secs(1);
        let mut rendered = 0u32;
        loop {
            rc.set_continuous(false);
            if rc.poll_should_render(past) {
                rendered += 1;
            } else {
                break;
            }
        }
        assert_eq!(
            rendered, CONVERGE_FRAMES,
            "renders exactly the convergence window past keep-warm"
        );
        assert!(rc.converged(), "reports converged once the window is spent");

        // With no temporal effect, convergence is immediate — past keep-warm idles at once.
        let mut plain = RedrawController::default();
        plain.set_continuous(true);
        assert!(plain.poll_should_render(t0));
        plain.set_continuous(false);
        assert!(
            !plain.poll_should_render(past),
            "no temporal effect → no convergence window, idles past keep-warm"
        );
        assert!(plain.converged());
    }

    #[test]
    fn loop_exits_after_frame_limit_with_correct_hook_order() {
        // A shared sink the layer records its hook order into. `Rc<RefCell>`
        // because the loop owns the layer by value, so the test reads its trace
        // out-of-band.
        let order: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let updates = Rc::new(Cell::new(0u32));

        struct ProbeLayer {
            order: Rc<RefCell<Vec<&'static str>>>,
            updates: Rc<Cell<u32>>,
        }
        impl Layer for ProbeLayer {
            fn on_attach(&mut self, _app: &mut App) {
                self.order.borrow_mut().push("attach");
            }
            fn on_update(&mut self, _app: &mut App, _dt: TimeSpan) {
                self.updates.set(self.updates.get() + 1);
                self.order.borrow_mut().push("update");
            }
            fn on_render(&mut self, _app: &mut App) {
                self.order.borrow_mut().push("render");
            }
            fn on_ui(&mut self, _app: &mut App) {
                self.order.borrow_mut().push("ui");
            }
            fn on_render_graph(&mut self, _app: &mut App, _graph: &mut RenderGraph) {
                self.order.borrow_mut().push("render_graph");
            }
            fn on_detach(&mut self, _app: &mut App) {
                self.order.borrow_mut().push("detach");
            }
        }

        let layer = Box::new(ProbeLayer {
            order: Rc::clone(&order),
            updates: Rc::clone(&updates),
        });
        drive(
            headless_app(),
            config_with(layer, Box::new(|_| {})),
            limited(3),
        );

        assert_eq!(
            updates.get(),
            3,
            "on_update fired exactly per the 3-frame limit"
        );
        let order = order.borrow();
        assert_eq!(order.first(), Some(&"attach"), "attach is first");
        assert_eq!(order.last(), Some(&"detach"), "detach is last");
        let frame: Vec<&'static str> = order[1..order.len() - 1].to_vec();
        assert_eq!(
            frame,
            vec![
                "update",
                "render",
                "ui",
                "render_graph", //
                "update",
                "render",
                "ui",
                "render_graph", //
                "update",
                "render",
                "ui",
                "render_graph",
            ],
            "per-frame hook order: update → render → ui → render_graph, three times"
        );
    }

    #[test]
    fn minimized_skips_frame_body_but_still_counts_to_the_limit() {
        struct ZeroSizeHost;
        impl FrameHost for ZeroSizeHost {
            fn begin_frame(&mut self) -> Result<bool> {
                panic!("begin_frame must not run while minimized");
            }
            fn begin_frame_graph(&mut self, _graph: &mut RenderGraph) {}
            fn end_frame(&mut self, _graph: RenderGraph) -> Result<()> {
                Ok(())
            }
            fn viewport_size(&self) -> (u32, u32) {
                (0, 720)
            }
            fn wait_gpu_idle(&self) -> Result<()> {
                Ok(())
            }
        }

        let renders = Rc::new(Cell::new(0u32));
        struct RenderCounter(Rc<Cell<u32>>);
        impl Layer for RenderCounter {
            fn on_render(&mut self, _app: &mut App) {
                self.0.set(self.0.get() + 1);
            }
        }

        let app = App::new(Box::new(ZeroSizeHost), None);
        let layer = Box::new(RenderCounter(Rc::clone(&renders)));
        drive(app, config_with(layer, Box::new(|_| {})), limited(2));

        assert_eq!(
            renders.get(),
            0,
            "a minimized host skips the frame body (no on_render) yet still exits via the frame limit"
        );
    }

    #[test]
    fn on_exit_runs_after_detach() {
        let trace: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

        struct DetachProbe(Rc<RefCell<Vec<&'static str>>>);
        impl Layer for DetachProbe {
            fn on_detach(&mut self, _app: &mut App) {
                self.0.borrow_mut().push("detach");
            }
        }

        let layer = Box::new(DetachProbe(Rc::clone(&trace)));
        let on_exit: Box<dyn FnOnce(&mut App)> = {
            let trace = Rc::clone(&trace);
            Box::new(move |_| trace.borrow_mut().push("exit"))
        };
        drive(headless_app(), config_with(layer, on_exit), limited(1));

        assert_eq!(
            &*trace.borrow(),
            &["detach", "exit"],
            "on_detach runs before on_exit (after wait_gpu_idle)"
        );
    }

    #[test]
    fn a_layer_attached_during_a_hook_joins_the_next_pass() {
        // The host attaches sub-layers from inside `on_attach`/`on_update`; verify
        // the `mem::take`-and-restore in `run_hook` preserves both the originals
        // and the freshly-attached without dropping either.
        let attached_count = Rc::new(Cell::new(0u32));

        struct Spawner {
            count: Rc<Cell<u32>>,
            done: bool,
        }
        impl Layer for Spawner {
            fn on_update(&mut self, app: &mut App, _dt: TimeSpan) {
                if !self.done {
                    self.done = true;
                    let count = Rc::clone(&self.count);
                    attach_layer(app, Box::new(Spawned(count)));
                }
            }
        }
        struct Spawned(Rc<Cell<u32>>);
        impl Layer for Spawned {
            fn on_update(&mut self, _app: &mut App, _dt: TimeSpan) {
                self.0.set(self.0.get() + 1);
            }
        }

        let layer = Box::new(Spawner {
            count: Rc::clone(&attached_count),
            done: false,
        });
        drive(
            headless_app(),
            config_with(layer, Box::new(|_| {})),
            limited(3),
        );

        // Frame 1: spawner attaches Spawned (which does not run that pass). Frames
        // 2 and 3: Spawned's on_update fires → 2.
        assert_eq!(
            attached_count.get(),
            2,
            "a mid-hook-attached layer joins the next pass, not the current one"
        );
    }

    // The full headless N-frame smoke (drive a real renderer for two frames over
    // `drive` on a present swapchain) needs hardware: the windowed swapchain present
    // path is covered by `tests/swapchain_present.rs` on a real Wayland surface, and
    // the offscreen path (the editor host) creates no swapchain at all. The renderer
    // crate's own offscreen tests validate the device / allocator / sync2 path via an
    // offscreen clear+readback. The loop's hook order, frame-limit exit, minimized
    // skip, and teardown ordering are fully proven above on the GPU-free `FrameHost`
    // stub.
}
