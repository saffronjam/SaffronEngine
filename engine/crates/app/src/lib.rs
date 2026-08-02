//! The run loop and the `Layer` lifecycle.
//!
//! The loop is reactive: a [`RedrawController`] decides each iteration whether to render or skip,
//! so an idle viewport holds its last frame and the GPU goes quiet. One [`run`] serves both a
//! windowed standalone host (winit window + surface-bound renderer) and a headless editor host
//! (no window, offscreen device), selected by `SAFFRON_EDITOR_NATIVE_VIEWPORT`.

#![deny(unsafe_code)]

use std::time::{Duration, Instant};

use saffron_core::TimeSpan;
use saffron_rendering::{RenderGraph, Renderer, SurfaceSource};
use saffron_window::{
    ActiveEventLoop, ApplicationHandler, ControlFlow, EventLoop, Window, WindowConfig, WindowEvent,
    WindowId,
};

/// Errors from host bring-up and the run loop.
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

/// The per-frame GPU host the loop drives, behind a trait so the loop is GPU-free-testable.
pub trait FrameHost {
    /// Acquires the swapchain image (or, in publish mode, the staging slot) and prepares per-frame
    /// state. `false` means the frame must be skipped.
    ///
    /// # Errors
    ///
    /// Propagates any host failure that should abort the loop.
    fn begin_frame(&mut self) -> Result<bool>;

    /// Builds the frame graph (cull + scene passes) into `graph` before the layers' pass over it.
    fn begin_frame_graph(&mut self, graph: &mut RenderGraph);

    /// Executes the graph (deriving every barrier) and presents or publishes the result.
    ///
    /// # Errors
    ///
    /// Propagates any host failure that should abort the loop.
    fn end_frame(&mut self, graph: RenderGraph) -> Result<()>;

    /// The current viewport size in pixels. A zero in either axis means the host is minimized.
    fn viewport_size(&self) -> (u32, u32);

    /// Blocks until the GPU is idle. Called once before any teardown so no in-flight command
    /// buffer references a resource about to drop.
    ///
    /// # Errors
    ///
    /// Propagates a device-lost or wait failure.
    fn wait_gpu_idle(&self) -> Result<()>;

    /// Rebuilds the swapchain and the active offscreen view for a `(width, height)` pixel resize.
    /// The headless host has no swapchain and never receives one.
    fn resized(&mut self, _width: u32, _height: u32) {}

    /// The concrete [`Renderer`] when this host is the real GPU renderer, `None` for the
    /// GPU-free test host. The scene-render and overlay seams live above this crate in the DAG,
    /// so they cannot be folded into [`FrameHost`] itself.
    fn renderer_mut(&mut self) -> Option<&mut Renderer> {
        None
    }

    /// Records this frame's wall-clock delta in seconds, feeding the renderer's smoothed frame
    /// time and fps.
    fn record_frame_timing(&mut self, _dt_seconds: f32) {}

    /// Folds this frame's telemetry in once it is rendered: the CPU busy/wait split in seconds
    /// (the loop's update+render window minus the GPU fence-wait), the history ring, the
    /// perf-alarm detectors, and the profiler-capture state machine. `dt_seconds` is the
    /// wall-clock delta since the prior frame.
    fn finalize_frame_telemetry(
        &mut self,
        _busy_seconds: f32,
        _wait_seconds: f32,
        _dt_seconds: f32,
    ) {
    }

    /// The render rate the reactive loop paces to while active, in frames per second. `None` or a
    /// non-positive target runs uncapped.
    fn pace_target_fps(&self) -> Option<f64> {
        None
    }
}

impl FrameHost for Renderer {
    fn begin_frame(&mut self) -> Result<bool> {
        if self.swapchain().is_some() {
            Ok(self.begin_present_frame()?)
        } else {
            // Wait the frame slot's fence before the host's `on_render`/`on_ui` hooks reset any
            // per-frame GPU state (the skinning descriptor pool), so the slot is idle first.
            self.begin_offscreen_frame()?;
            Ok(true)
        }
    }

    fn begin_frame_graph(&mut self, _graph: &mut RenderGraph) {}

    fn end_frame(&mut self, _graph: RenderGraph) -> Result<()> {
        if self.swapchain().is_some() {
            self.present_active_view_to_swapchain()?;
        }
        // A layer that drew nothing never reached `render_scene_offscreen`, leaving the slot fence
        // reset-but-unsignalled and the next frame waiting on it forever.
        self.finish_unsubmitted_frame()?;
        Ok(())
    }

    fn viewport_size(&self) -> (u32, u32) {
        (self.viewport_width(), self.viewport_height())
    }

    fn wait_gpu_idle(&self) -> Result<()> {
        Ok(self.device().wait_idle()?)
    }

    fn resized(&mut self, width: u32, height: u32) {
        // A zero extent is a minimize, which the next frame's `viewport_size` guard skips anyway.
        if self.swapchain().is_none() || width == 0 || height == 0 {
            return;
        }
        if let Err(err) = self.recreate_swapchain(width, height) {
            tracing::error!("swapchain recreate failed: {err}");
        }
        // The present blit's source is the active offscreen view; track the window so the
        // presented image is rendered at native resolution, not scaled.
        let view = self.active_view_id();
        self.set_viewport_desired_size(view, width, height);
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
/// idle. Never hard-stop the loop the instant activity ceases, or the GPU downclocks mid-interaction.
const KEEP_WARM: Duration = Duration::from_millis(600);

/// Rendered frames a temporal effect (TAA / SSGI history) needs after an invalidation to converge.
/// A static viewport settles to its converged image before idling on it. At a low target fps this
/// outlasts the wall-clock [`KEEP_WARM`]; at a high one [`KEEP_WARM`] dominates.
const CONVERGE_FRAMES: u32 = 24;

/// The poll interval while fully idle: the loop still drains the control socket this often but
/// issues no GPU work.
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(8);

/// Decides, each loop iteration, whether to render a frame or skip it.
///
/// The host sets the per-frame activity in `on_update`. The loop renders while active, then keeps
/// rendering until **both** the wall-clock [`KEEP_WARM`] window has elapsed **and** the temporal
/// effects have had [`CONVERGE_FRAMES`] frames to settle, then idles holding the converged frame.
/// The default is `continuous`, so only a host that opts into reactivity ever idles.
pub struct RedrawController {
    /// Set each frame by the host: `true` while some state evolves without a new command.
    continuous: bool,
    /// One-shot: a mutating command landed this frame; render it (consumed by the decision).
    dirty: bool,
    /// Whether a temporal effect is accumulating — gates the convergence window.
    temporal_active: bool,
    /// While set (the viewport is occluded / minimized) the loop renders nothing, regardless of
    /// activity.
    suppressed: bool,
    /// When activity (continuous or dirty) was last seen, for the keep-warm window.
    last_activity: Option<Instant>,
    /// Rendered frames since the last activity — the convergence progress counter.
    frames_since_activity: u32,
    /// The verdict the last [`Self::poll_should_render`] returned.
    last_rendered: bool,
    /// The named reasons currently forcing continuous render, for observability.
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
    /// Sets whether some state is evolving on its own this frame. `true` forces a render and
    /// resets convergence.
    pub fn set_continuous(&mut self, on: bool) {
        self.continuous = on;
    }

    /// Sets whether a temporal effect is accumulating this frame, so the loop renders a
    /// convergence window after activity instead of stopping the instant motion ceases.
    pub fn set_temporal_active(&mut self, on: bool) {
        self.temporal_active = on;
    }

    /// Hard-suppresses all rendering (the viewport is occluded / minimized) until cleared.
    pub fn set_suppressed(&mut self, on: bool) {
        self.suppressed = on;
    }

    /// Whether the loop is currently idling rather than rendering.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        !self.last_rendered
    }

    /// Records the named reasons the host is holding continuous render this frame. Purely
    /// informational; [`Self::set_continuous`] drives the decision.
    pub fn set_reasons(&mut self, reasons: Vec<&'static str>) {
        self.reasons = reasons;
    }

    /// The reasons continuous render is currently held (empty when idle).
    #[must_use]
    pub fn reasons(&self) -> &[&'static str] {
        &self.reasons
    }

    /// Whether the temporal effects have converged.
    #[must_use]
    pub fn converged(&self) -> bool {
        !self.continuous && (!self.temporal_active || self.frames_since_activity >= CONVERGE_FRAMES)
    }

    /// Requests one render next decision. Resets convergence.
    pub fn request_redraw(&mut self) {
        self.dirty = true;
    }

    /// Resolves the render-or-skip verdict for this iteration at `now`, consuming the one-shot
    /// dirty flag.
    fn poll_should_render(&mut self, now: Instant) -> bool {
        let render = self.decide(now);
        self.last_rendered = render;
        render
    }

    fn decide(&mut self, now: Instant) -> bool {
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
/// Field order encodes teardown: [`Self::frame_host`] drops before [`Self::window`].
pub struct App {
    /// The per-frame GPU host.
    pub frame_host: Box<dyn FrameHost>,
    /// The OS window, present only in windowed mode.
    pub window: Option<Window>,
    /// The loop's run latch; a layer or signal handler sets it `false` to exit.
    pub running: bool,
    /// The reactive-render verdict source, driven by the host each `on_update`.
    pub redraw: RedrawController,
    /// The attached layers. The loop `mem::take`s the vec out for each hook pass and restores it,
    /// so it is empty *during* a hook.
    layers: Vec<Box<dyn Layer>>,
}

impl App {
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

/// A set of lifecycle hooks a client implements, with every hook defaulted to empty.
///
/// Each hook takes `&mut App` rather than capturing it, so a layer never aliases the app it runs
/// inside.
pub trait Layer {
    /// The layer's name, for logs.
    fn name(&self) -> &str {
        "Layer"
    }

    /// Runs once after the window + renderer exist and the layer is attached.
    fn on_attach(&mut self, _app: &mut App) {}

    /// Runs once per frame before rendering, with the frame delta.
    fn on_update(&mut self, _app: &mut App, _dt: TimeSpan) {}

    /// Submits GPU work for the frame through the renderer's submit seam.
    fn on_render(&mut self, _app: &mut App) {}

    /// Builds UI / overlay geometry for the frame.
    fn on_ui(&mut self, _app: &mut App) {}

    /// Adds passes to the frame's render graph.
    fn on_render_graph(&mut self, _app: &mut App, _graph: &mut RenderGraph) {}

    /// Runs once during teardown, before the renderer is dropped.
    fn on_detach(&mut self, _app: &mut App) {}
}

/// Client-provided configuration handed to [`run`].
pub struct AppConfig {
    /// The window parameters for windowed mode (ignored in headless mode).
    pub window: WindowConfig,
    /// Runs once after bring-up; the host attaches its [`Layer`] and wires signals here.
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HostMode {
    /// A winit window + a surface-bound renderer.
    Windowed,
    /// No window + a no-surface offscreen device.
    Headless,
}

impl HostMode {
    /// Reads the mode from `SAFFRON_EDITOR_NATIVE_VIEWPORT`, which the editor sets when it spawns
    /// the host as its viewport producer.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_present(std::env::var_os("SAFFRON_EDITOR_NATIVE_VIEWPORT").is_some())
    }

    /// Maps the presence of the var (any value, including empty) to a mode. Pure, so it is
    /// testable without env mutation.
    #[must_use]
    fn from_present(present: bool) -> Self {
        if present {
            Self::Headless
        } else {
            Self::Windowed
        }
    }
}

/// Owns the main loop. Returns a process exit code (`0` on a clean exit, `1` on a bring-up
/// failure). The mode is read from the environment.
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

/// Brings up the host for `mode`, runs the loop, tears down.
///
/// Windowed mode needs a winit window, which winit 0.30 only creates from inside an active event
/// loop, so it hands off to [`run_windowed`]. Both modes share the per-frame body and the
/// `wait_gpu_idle`-before-teardown ordering.
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

/// The loop's frame-limit knob, read once from the environment so the loop body never reads it
/// (the crate denies `unsafe`, so the tests cannot set env vars).
#[derive(Clone, Copy, Default)]
struct LoopLimits {
    /// `SAFFRON_EXIT_AFTER_FRAMES`: exit after this many frames; `0` = no limit.
    frame_limit: u64,
}

impl LoopLimits {
    fn from_env() -> Self {
        Self {
            frame_limit: frame_limit_from_env(),
        }
    }
}

/// The frame-clock state threaded between iterations, shared by both loop drivers so they pace and
/// count frames identically.
struct FrameClock {
    frame_count: u64,
    last: Instant,
}

impl FrameClock {
    fn new() -> Self {
        Self {
            frame_count: 0,
            last: Instant::now(),
        }
    }
}

/// Runs `on_create` then `on_attach` and latches the loop running.
fn start(app: &mut App, config: &mut AppConfig) {
    let on_create = std::mem::replace(&mut config.on_create, Box::new(|_| {}));
    on_create(app);
    run_hook(app, |layer, app| layer.on_attach(app));
    app.running = true;
}

/// `wait_gpu_idle`, then `on_detach`, then `on_exit`. `wait_gpu_idle` runs first so no in-flight
/// command buffer references a resource a handler is about to drop.
fn finish(app: &mut App, config: &mut AppConfig) {
    if let Err(err) = app.frame_host.wait_gpu_idle() {
        tracing::error!("wait_gpu_idle failed: {err}");
    }
    run_hook(app, |layer, app| layer.on_detach(app));
    let on_exit = std::mem::replace(&mut config.on_exit, Box::new(|_| {}));
    on_exit(app);
}

/// Runs one loop iteration: the `on_update` pass, then — only if a render is due — the frame body
/// and its telemetry, then the frame-limit check and the pacing sleep.
///
/// `on_update` (and so the control-socket drain) runs *every* iteration, including idle ones, so a
/// command lands within one [`IDLE_POLL_INTERVAL`] even while the GPU is quiet.
fn step_frame(app: &mut App, limits: LoopLimits, clock: &mut FrameClock) {
    let now = Instant::now();
    let dt = TimeSpan::from_seconds((now - clock.last).as_secs_f32());
    clock.last = now;

    // The CPU busy window opens here and closes after `run_frame`; the GPU fence-wait inside
    // `begin_frame` is the wait split.
    let busy_start = Instant::now();
    run_hook(app, |layer, app| layer.on_update(app, dt));

    let (width, height) = app.frame_host.viewport_size();
    let minimized = width == 0 || height == 0;
    let render = !minimized && app.redraw.poll_should_render(now);

    let mut wait_seconds = 0.0;
    if render {
        // Only rendered frames advance the frame delta, so idle does not pollute the fps EMA.
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
        let busy_seconds = (busy_start.elapsed().as_secs_f32() - wait_seconds).max(0.0);
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

/// The headless driver: a plain `while` loop over an already-built [`App`], no window.
fn drive(mut app: App, mut config: AppConfig, limits: LoopLimits) {
    start(&mut app, &mut config);

    let mut clock = FrameClock::new();
    while app.running {
        step_frame(&mut app, limits, &mut clock);
    }

    finish(&mut app, &mut config);
}

/// The standalone present-only host: a winit window + a surface-bound renderer.
///
/// winit 0.30 owns its own loop and only creates a window from inside it, so the frame body runs
/// through an [`ApplicationHandler`] rather than the plain `while` of [`drive`], reusing the same
/// [`start`] / [`step_frame`] / [`finish`].
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

    if let Some(err) = windowed.bring_up_error {
        return Err(err);
    }
    Ok(())
}

/// The winit [`ApplicationHandler`] driving the windowed host.
///
/// A window/renderer failure in `resumed` cannot return through the winit callback, so it is
/// stashed in `bring_up_error` and re-raised by [`run_windowed`].
struct WindowedApp {
    config: Option<AppConfig>,
    limits: LoopLimits,
    app: Option<App>,
    clock: FrameClock,
    bring_up_error: Option<Error>,
}

impl ApplicationHandler for WindowedApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // `resumed` can fire more than once (a real desktop suspend/resume); build the window +
        // renderer only on the first, when the config is still present.
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

        // `step_frame` paces the iteration itself, so the `ControlFlow::Poll` loop never free-runs.
        step_frame(app, self.limits, &mut self.clock);

        if !app.running {
            event_loop.exit();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let (Some(mut app), Some(mut config)) = (self.app.take(), self.config.take()) {
            finish(&mut app, &mut config);
        }
    }
}

/// The render/ui/graph hook pass for one accepted frame. The render graph is owned here for the
/// pass and moved into `end_frame`, which executes it.
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
/// The layer vec moves out of `app` for the pass so `f` can borrow `&mut App` without aliasing the
/// list. A hook that itself attaches a layer is picked up next pass, not mid-pass.
fn run_hook(app: &mut App, mut f: impl FnMut(&mut Box<dyn Layer>, &mut App)) {
    let mut layers = std::mem::take(&mut app.layers);
    for layer in &mut layers {
        f(layer, app);
    }
    layers.append(&mut app.layers);
    app.layers = layers;
}

/// Sleeps to pace one loop iteration that started at `iter_start`.
///
/// Pacing from the iteration start rather than a running accumulator means a frame slower than the
/// interval simply runs back-to-back, which is correct when the GPU is the bottleneck.
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

/// Attaches a layer to the app. A layer attached during a hook pass joins the per-frame iteration
/// on the *next* pass; the running loop never replays `on_attach` mid-loop.
pub fn attach_layer(app: &mut App, layer: Box<dyn Layer>) {
    app.layers.push(layer);
}

/// Parses `SAFFRON_EXIT_AFTER_FRAMES` strictly: a valid `u64` count, else `0` (no frame limit).
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

/// Parses `text` as a whole base-10 `u64` — no trailing garbage, sign, or whitespace. Pure, so it
/// is testable without mutating the process environment.
fn parse_strict_u64(text: &str) -> Option<u64> {
    text.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

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

    fn headless_app() -> App {
        App::new(Box::new(NoopFrameHost), None)
    }

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

    /// The frame limit cannot be reached via env in this `unsafe`-free crate, so the loop tests
    /// pass `LoopLimits` explicitly.
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
        assert!(rc.poll_should_render(t0), "default continuous renders");

        rc.set_continuous(false);
        assert!(
            rc.poll_should_render(t0 + Duration::from_millis(100)),
            "renders inside keep-warm after activity"
        );

        rc.set_continuous(false);
        assert!(
            !rc.poll_should_render(t0 + KEEP_WARM + Duration::from_millis(200)),
            "idles once keep-warm elapses"
        );

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
        let mut rc = RedrawController::default();
        rc.set_temporal_active(true);
        let t0 = Instant::now();

        rc.set_continuous(true);
        assert!(rc.poll_should_render(t0), "active frame renders");

        // Past keep-warm, only the convergence window keeps it rendering.
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

        assert_eq!(
            attached_count.get(),
            2,
            "a mid-hook-attached layer joins the next pass, not the current one"
        );
    }
}
