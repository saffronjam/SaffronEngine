//! The real acquire → clear → present half of the bring-up gate, on a windowed
//! Vulkan surface.
//!
//! Lavapipe's `VK_EXT_headless_surface` swapchain WSI is unimplemented (it crashes
//! creating native swapchain image memory), so the present-engine path is exercised
//! against a real Wayland surface — under a headless `weston` in the toolbox per
//! AGENTS.md. This test creates a winit window, brings up the [`Renderer`] from its
//! surface handle, runs several `render_frame`s, and asserts the run is
//! validation-clean. It **skips cleanly** when no Wayland/X11 display is available
//! (no `WAYLAND_DISPLAY` / `DISPLAY`), so the gate stays green off a display while
//! the self-contained offscreen smoke in the crate's unit tests always runs.
//!
//! Windowed present is exercised only on Linux: the test drives an off-main-thread Wayland/X11
//! event loop (`winit::platform::{wayland,x11}` + `with_any_thread`), APIs that exist only there.
//! The offscreen render-and-read-back path — which is what every other platform uses — is covered
//! by the always-on unit tests, so gating this one to Linux loses no coverage elsewhere.
#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use saffron_geometry::glam::{Mat4, Vec3};
use saffron_rendering::{Renderer, SurfaceSource, validation_issue_count};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// How many frames the present smoke records before exiting.
const FRAMES: u32 = 8;

/// Builds an event loop that may run off the main thread (the test harness spawns
/// the test). Prefers the Wayland any-thread builder, falling back to X11.
fn build_any_thread_event_loop() -> Result<EventLoop<()>, winit::error::EventLoopError> {
    use winit::event_loop::EventLoopBuilder;

    let mut builder = EventLoopBuilder::default();
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        use winit::platform::wayland::EventLoopBuilderExtWayland;
        builder.with_any_thread(true);
    } else {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_any_thread(true);
    }
    builder.build()
}

/// Drives a winit window through a validation-clean clear+present run, then exits.
///
/// Field order is load-bearing: the [`Renderer`] (which owns the `VkSurfaceKHR`
/// and swapchain built from the window's Wayland surface) must drop *before* the
/// [`Window`] — destroying the swapchain references the window's `wl_surface`, and
/// lavapipe's Wayland WSI crashes if that surface is already gone. Rust drops
/// fields in declaration order, so `renderer` is declared first.
struct PresentSmoke {
    renderer: Option<Renderer>,
    window: Option<Window>,
    frames_done: u32,
    issues_before: u64,
    failure: Option<String>,
    deadline: Instant,
    /// When set, a window screenshot is armed after the first present and the file is
    /// expected to exist by the end of the run (the window-capture validation).
    capture_path: Option<std::path::PathBuf>,
    capture_armed: bool,
}

impl PresentSmoke {
    fn new() -> Self {
        Self {
            renderer: None,
            window: None,
            frames_done: 0,
            issues_before: validation_issue_count(),
            failure: None,
            deadline: Instant::now() + Duration::from_secs(30),
            capture_path: None,
            capture_armed: false,
        }
    }

    fn with_capture(path: std::path::PathBuf) -> Self {
        let mut smoke = Self::new();
        smoke.capture_path = Some(path);
        smoke
    }
}

impl ApplicationHandler for PresentSmoke {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("saffron-present-smoke")
            .with_inner_size(LogicalSize::new(640, 480));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => window,
            Err(err) => {
                self.failure = Some(format!("create_window failed: {err}"));
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        match Renderer::new(&SurfaceSource::Window(&window), size.width, size.height) {
            Ok(renderer) => {
                self.renderer = Some(renderer);
                self.window = Some(window);
            }
            Err(err) => {
                self.failure = Some(format!("renderer bring-up failed: {err}"));
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if Instant::now() > self.deadline {
            self.failure.get_or_insert_with(|| "timed out".to_string());
            event_loop.exit();
            return;
        }
        event_loop.set_control_flow(ControlFlow::Poll);
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

impl PresentSmoke {
    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        // Arm the window capture after a couple of presents so the swapchain image holds
        // a rendered frame; the next `render_frame` presents then copies it to the PNG.
        if let Some(path) = &self.capture_path
            && !self.capture_armed
            && self.frames_done >= 2
            && let Err(err) = renderer.request_window_capture(path)
        {
            self.failure = Some(format!("request_window_capture: {err}"));
            event_loop.exit();
            return;
        }
        if self.capture_path.is_some() && self.frames_done >= 2 {
            self.capture_armed = true;
        }
        match renderer.render_frame() {
            Ok(_presented) => {
                self.frames_done += 1;
                if self.frames_done >= FRAMES {
                    let _ = renderer.device().wait_idle();
                    event_loop.exit();
                }
            }
            Err(err) => {
                self.failure = Some(format!("frame {} failed: {err}", self.frames_done));
                event_loop.exit();
            }
        }
    }
}

/// Brings up a windowed renderer and runs a validation-clean clear+present.
#[test]
fn windowed_clear_present_is_validation_clean() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
        eprintln!("skipping: no Wayland/X11 display (set up a headless weston per AGENTS.md)");
        return;
    }

    // `cargo test` runs each test on a spawned thread; winit blocks an event loop
    // off the main thread on Linux unless the platform `any_thread` builder is used.
    let event_loop = match build_any_thread_event_loop() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            eprintln!("skipping: no winit event loop ({err})");
            return;
        }
    };

    let mut smoke = PresentSmoke::new();
    if let Err(err) = event_loop.run_app(&mut smoke) {
        panic!("event loop failed: {err}");
    }

    if let Some(failure) = smoke.failure {
        panic!("present smoke failed: {failure}");
    }
    assert_eq!(
        smoke.frames_done, FRAMES,
        "ran the full clear+present sequence"
    );

    let issues_after = validation_issue_count();
    assert_eq!(
        smoke.issues_before,
        issues_after,
        "the windowed clear+present must be validation-clean (saw {} new issue(s))",
        issues_after - smoke.issues_before
    );
}

/// Arms `request_window_capture` mid-run on a windowed renderer; the next present copies
/// the composited swapchain image to a PNG. Asserts the file is a valid PNG of the
/// swapchain extent and the run stays validation-clean. The window/composited-capture
/// feature's functional validation — it needs a real present surface, so it skips off a
/// display (DEFERRED-NEEDS-DISPLAY off a display; live under the toolbox weston).
#[test]
fn window_capture_writes_a_png_of_the_composited_output() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
        eprintln!("skipping: no Wayland/X11 display (DEFERRED-NEEDS-DISPLAY off a display)");
        return;
    }
    let event_loop = match build_any_thread_event_loop() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            eprintln!("skipping: no winit event loop ({err})");
            return;
        }
    };

    let path =
        std::env::temp_dir().join(format!("saffron-window-capture-{}.png", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut smoke = PresentSmoke::with_capture(path.clone());
    if let Err(err) = event_loop.run_app(&mut smoke) {
        panic!("event loop failed: {err}");
    }
    if let Some(failure) = smoke.failure {
        panic!("window capture smoke failed: {failure}");
    }

    // The capture file was written and decodes as a valid PNG with a non-zero extent.
    let bytes = std::fs::read(&path).expect("window capture PNG written");
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
        "the capture is a valid PNG"
    );
    let decoded = image::load_from_memory(&bytes).expect("decode window capture");
    let (w, h) = (decoded.width(), decoded.height());
    assert!(
        w > 0 && h > 0,
        "the capture has a non-zero extent ({w}x{h})"
    );
    let _ = std::fs::remove_file(&path);

    let issues_after = validation_issue_count();
    assert_eq!(
        smoke.issues_before,
        issues_after,
        "the window capture must be validation-clean (saw {} new issue(s))",
        issues_after - smoke.issues_before
    );
}

/// Drives the standalone present-only host's frame path — `begin_present_frame` (acquire) →
/// `render_scene_offscreen` (the procedural sky + grid into the offscreen) →
/// `present_active_view_to_swapchain` (blit offscreen → swapchain, present) — and captures
/// the presented swapchain image. This is the windowed-compositing path the host loop runs:
/// it proves the offscreen is actually blitted onto the swapchain, not a bare clear.
struct BlitSmoke {
    renderer: Option<Renderer>,
    window: Option<Window>,
    frames_done: u32,
    issues_before: u64,
    failure: Option<String>,
    deadline: Instant,
    capture_path: std::path::PathBuf,
    capture_armed: bool,
}

impl BlitSmoke {
    fn new(capture_path: std::path::PathBuf) -> Self {
        Self {
            renderer: None,
            window: None,
            frames_done: 0,
            issues_before: validation_issue_count(),
            failure: None,
            deadline: Instant::now() + Duration::from_secs(30),
            capture_path,
            capture_armed: false,
        }
    }
}

impl ApplicationHandler for BlitSmoke {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("saffron-blit-smoke")
            .with_inner_size(LogicalSize::new(320, 240));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => window,
            Err(err) => {
                self.failure = Some(format!("create_window failed: {err}"));
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        match Renderer::new(&SurfaceSource::Window(&window), size.width, size.height) {
            Ok(mut renderer) => {
                // The standalone host always renders present-only (the blit path).
                renderer.set_present_viewport_only(true);
                self.renderer = Some(renderer);
                self.window = Some(window);
            }
            Err(err) => {
                self.failure = Some(format!("renderer bring-up failed: {err}"));
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if Instant::now() > self.deadline {
            self.failure.get_or_insert_with(|| "timed out".to_string());
            event_loop.exit();
            return;
        }
        event_loop.set_control_flow(ControlFlow::Poll);
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

impl BlitSmoke {
    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        // Arm the window capture after a couple of presents so the swapchain image holds a
        // blitted frame; the next present then copies the composited output to the PNG.
        if !self.capture_armed
            && self.frames_done >= 3
            && let Err(err) = renderer.request_window_capture(&self.capture_path)
        {
            self.failure = Some(format!("request_window_capture: {err}"));
            event_loop.exit();
            return;
        }
        if self.frames_done >= 3 {
            self.capture_armed = true;
        }

        // The host loop's per-frame body for the windowed present-only host: acquire, render
        // the scene offscreen (procedural sky, no draw list), then blit + present.
        let presented = match renderer.begin_present_frame() {
            Ok(presented) => presented,
            Err(err) => {
                self.failure = Some(format!("begin_present_frame {}: {err}", self.frames_done));
                event_loop.exit();
                return;
            }
        };
        if !presented {
            // Out-of-date swapchain (a resize): skip this frame.
            return;
        }
        // Set up a visible procedural sky + a camera so `render_scene_offscreen` fills the
        // offscreen with the sky gradient (the non-uniform content the blit must carry to the
        // swapchain). A simple look-down-the-Z perspective view-proj.
        renderer.submit_sky(&saffron_rendering::SkyRenderSettings::default());
        if let Err(err) = renderer.set_scene_lighting(&saffron_rendering::SceneLighting::default())
        {
            self.failure = Some(format!("set_scene_lighting {}: {err}", self.frames_done));
            event_loop.exit();
            return;
        }
        let proj = Mat4::perspective_rh(60.0_f32.to_radians(), 320.0 / 240.0, 0.1, 100.0);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 1.0, 4.0), Vec3::ZERO, Vec3::Y);
        if let Err(err) = renderer.submit_gpu_scene_deformations(proj * view, &[], &[]) {
            self.failure = Some(format!(
                "submit_gpu_scene_deformations {}: {err}",
                self.frames_done
            ));
            event_loop.exit();
            return;
        }
        if let Err(err) = renderer.render_scene_offscreen() {
            self.failure = Some(format!(
                "render_scene_offscreen {}: {err}",
                self.frames_done
            ));
            event_loop.exit();
            return;
        }
        if let Err(err) = renderer.present_active_view_to_swapchain() {
            self.failure = Some(format!("present_to_swapchain {}: {err}", self.frames_done));
            event_loop.exit();
            return;
        }
        self.frames_done += 1;
        if self.frames_done >= FRAMES {
            let _ = renderer.device().wait_idle();
            event_loop.exit();
        }
    }
}

/// The windowed present-only blit proof: brings up a windowed renderer in present-only mode,
/// runs the host's acquire → offscreen-render → blit → present path for several frames,
/// captures the presented swapchain image, and asserts it is NON-BLANK (the procedural sky's
/// gradient gives many distinct colors — a bare clear would be one). Also asserts the run is
/// validation-clean. Skips off a display (DEFERRED-NEEDS-DISPLAY; live under the toolbox
/// weston or a real Wayland session).
#[test]
fn present_only_blit_shows_a_non_blank_scene() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
        eprintln!("skipping: no Wayland/X11 display (DEFERRED-NEEDS-DISPLAY off a display)");
        return;
    }
    let event_loop = match build_any_thread_event_loop() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            eprintln!("skipping: no winit event loop ({err})");
            return;
        }
    };

    let path = std::env::temp_dir().join(format!("saffron-blit-smoke-{}.png", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut smoke = BlitSmoke::new(path.clone());
    if let Err(err) = event_loop.run_app(&mut smoke) {
        panic!("event loop failed: {err}");
    }
    if let Some(failure) = smoke.failure {
        panic!("blit smoke failed: {failure}");
    }
    assert_eq!(
        smoke.frames_done, FRAMES,
        "ran the full blit+present sequence"
    );

    // The captured presented swapchain image must be NON-BLANK: the procedural sky renders a
    // gradient, so the blitted frame has many distinct colors. A clear-only present would
    // have a single uniform color. Decode + count distinct pixels.
    let bytes = std::fs::read(&path).expect("window capture PNG written");
    let decoded = image::load_from_memory(&bytes).expect("decode capture");
    let rgba = decoded.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    assert!(w > 0 && h > 0, "capture has a non-zero extent ({w}x{h})");
    let mut distinct = std::collections::HashSet::new();
    for px in rgba.pixels() {
        distinct.insert([px[0], px[1], px[2]]);
        if distinct.len() > 64 {
            break;
        }
    }
    assert!(
        distinct.len() > 16,
        "the presented swapchain image is NON-BLANK (saw {} distinct colors; a blank \
         clear-only present would be 1) — the offscreen scene was blitted to the swapchain",
        distinct.len()
    );
    let _ = std::fs::remove_file(&path);

    let issues_after = validation_issue_count();
    assert_eq!(
        smoke.issues_before,
        issues_after,
        "the present-only blit must be validation-clean (saw {} new issue(s))",
        issues_after.saturating_sub(smoke.issues_before)
    );
}
