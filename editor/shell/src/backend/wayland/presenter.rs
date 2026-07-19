//! Presents the engine's shared-memory frames on Wayland subsurfaces placed BELOW the toplevel's
//! `wl_surface`. The transparent CEF UI composites over them, so the viewport presents at the
//! monitor's refresh — independent of the UI
//! paint loop. A worker thread owns the subsurfaces on its own `from_foreign_display` connection
//! (the same foreign-display integration `compositor.rs` uses), paced by `wl_surface.frame`
//! callbacks with `wp_presentation` feedback learning the true refresh.
//!
//! The opaque backdrop lives in `compositor.rs` (below the toplevel, stretched to the window). These
//! viewport subsurfaces are created later and `place_below(parent)`, so they stack above that
//! backdrop and below the UI; a parked view detaches its buffer, revealing the backdrop through the
//! transparent hole.

use std::ffi::CString;
use std::os::fd::{AsFd, OwnedFd};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::window::Handles;
use crate::viewport::{
    SHM_HEADER_BYTES, SHM_MAGIC, View, ViewportShared, Viewports, open_shm, pack_pair, stat_shm,
    unpack_pair,
};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_callback::{self, WlCallback},
    wl_compositor::WlCompositor,
    wl_registry::{self, WlRegistry},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, Kind as FeedbackKind, WpPresentationFeedback},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

/// Ground truth for "is the viewport really updating": frame callbacks only pace commits, while
/// `wp_presentation` says what the compositor DID with each — displayed (presented, with the vblank
/// seq it hit) or superseded by a later commit (discarded).
#[derive(Default)]
struct PresentationStats {
    presented: u32,
    discarded: u32,
    refresh_ns: u32,
    flags: u32,
    last_seq: Option<u64>,
    seq_delta_sum: u64,
    seq_delta_count: u32,
}

impl PresentationStats {
    fn flags_label(&self) -> String {
        let mut names = Vec::new();
        for (bit, name) in [
            (FeedbackKind::Vsync, "vsync"),
            (FeedbackKind::HwClock, "hw-clock"),
            (FeedbackKind::HwCompletion, "hw-completion"),
            (FeedbackKind::ZeroCopy, "zero-copy"),
        ] {
            if self.flags & bit.bits() != 0 {
                names.push(name);
            }
        }
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join("+")
        }
    }

    fn reset_window(&mut self) {
        self.presented = 0;
        self.discarded = 0;
        self.flags = 0;
        self.seq_delta_sum = 0;
        self.seq_delta_count = 0;
        // last_seq survives so the first delta of the next window stays meaningful.
    }
}

#[derive(Default)]
struct State {
    globals: Vec<(u32, String, u32)>,
    stats: PresentationStats,
    /// Per-view frame-callback pending flags, indexed by `View`. A surface's frame callback clears
    /// its own slot, so the two panes pace independently on the compositor's refresh.
    frame_pending: [bool; 2],
    /// Shared with `Viewports`: the presenter publishes the output refresh (mHz) here from
    /// presentation feedback, so `viewport_refresh_hz` can report the true monitor refresh.
    refresh_out: Arc<AtomicU32>,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<WlCallback, usize> for State {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        view: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event
            && let Some(slot) = state.frame_pending.get_mut(*view)
        {
            *slot = false;
        }
    }
}

impl Dispatch<WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Release events: the engine's seqlock ring makes writer/reader collisions unlikely; we
        // accept the (cosmetic) risk rather than cross-process backpressure.
    }
}

impl Dispatch<WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpPresentation, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            tracing::debug!(target: "shell", "viewport presentation clock id {clk_id}");
        }
    }
}

impl Dispatch<WpPresentationFeedback, ()> for State {
    fn event(
        state: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                refresh,
                seq_hi,
                seq_lo,
                flags,
                ..
            } => {
                let stats = &mut state.stats;
                let seq = (u64::from(seq_hi) << 32) | u64::from(seq_lo);
                stats.presented += 1;
                stats.refresh_ns = refresh;
                if let Some(last) = stats.last_seq {
                    stats.seq_delta_sum += seq.saturating_sub(last);
                    stats.seq_delta_count += 1;
                }
                stats.last_seq = Some(seq);
                if let WEnum::Value(kind) = flags {
                    stats.flags |= kind.bits();
                }
                // Publish the output refresh (ns/vblank → mHz): mHz = 1e12 / refresh_ns. `0` = unknown.
                if refresh > 0 {
                    let mhz = (1_000_000_000_000u64 / u64::from(refresh)) as u32;
                    state.refresh_out.store(mhz, Ordering::Relaxed);
                }
            }
            wp_presentation_feedback::Event::Discarded => state.stats.discarded += 1,
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(State: ignore WlCompositor);
wayland_client::delegate_noop!(State: ignore WlSubcompositor);
wayland_client::delegate_noop!(State: ignore WlSubsurface);
wayland_client::delegate_noop!(State: ignore WlSurface);
wayland_client::delegate_noop!(State: ignore WlShmPool);
wayland_client::delegate_noop!(State: ignore WpViewporter);
wayland_client::delegate_noop!(State: ignore WpViewport);

/// Spawn the worker thread that owns the two viewport subsurfaces + commit loop. The handles carry
/// winit's raw `wl_display`/`wl_surface`; the worker shares winit's display via
/// `from_foreign_display`, so its subsurfaces are children of the real toplevel. Each view maps its
/// own engine shm segment (`scene_shm` / `asset_shm`), retrying until the engine creates it.
pub fn install(handles: &Handles, scene_shm: String, asset_shm: String, viewports: &Viewports) {
    let wl_display = handles.wl_display();
    let wl_surface = handles.wl_surface();
    let scene_shared = Arc::clone(viewports.view(View::Scene));
    let asset_shared = Arc::clone(viewports.view(View::AssetPreview));
    let refresh_out = Arc::clone(&viewports.refresh_mhz);
    thread::spawn(move || {
        let views = [
            (View::Scene, scene_shm, scene_shared),
            (View::AssetPreview, asset_shm, asset_shared),
        ];
        if let Err(err) = run(wl_display, wl_surface, views, refresh_out) {
            tracing::error!(target: "shell", "viewport presenter failed: {err}");
        }
    });
}

/// One view's compositor objects + the per-view state the loop carries between ticks. Each view owns
/// a subsurface permanently glued to its pane, its own shm mapping, and its own buffer ring — so a
/// tab switch parks/unparks rather than re-binding a shared surface.
struct ViewSurface {
    view: View,
    shared: Arc<ViewportShared>,
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
    cname: CString,
    pool_fd: OwnedFd,
    base: *const u8,
    total: usize,
    generation: u64,
    header: *const u32,
    pool: WlShmPool,
    buffers: Vec<WlBuffer>,
    buffer_dims: (u32, u32),
    last_seq: u32,
    applied_size: u64,
    applied_pos: u64,
    buffer_attached: bool,
    parked: bool,
    first_commit: bool,
    commits: u32,
    last_segment_check: Instant,
    frame_sent_at: Instant,
    last_commit_at: Instant,
}

fn run(
    display_addr: usize,
    parent_addr: usize,
    views: [(View, String, Arc<ViewportShared>); 2],
    refresh_out: Arc<AtomicU32>,
) -> Result<(), String> {
    let stats_enabled = std::env::var_os("SAFFRON_VIEWPORT_STATS").is_some();
    let backend = unsafe { Backend::from_foreign_display(display_addr as *mut _) };
    let conn = Connection::from_backend(backend);
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();

    let registry = conn.display().get_registry(&qh, ());
    let mut state = State {
        refresh_out,
        ..State::default()
    };
    queue
        .roundtrip(&mut state)
        .map_err(|err| format!("registry roundtrip: {err}"))?;

    let find = |wanted: &str| -> Option<(u32, u32)> {
        state
            .globals
            .iter()
            .find(|(_, name, _)| name == wanted)
            .map(|(id, _, ver)| (*id, *ver))
    };
    let (compositor_id, compositor_ver) = find("wl_compositor").ok_or("no wl_compositor global")?;
    let (subcompositor_id, _) = find("wl_subcompositor").ok_or("no wl_subcompositor global")?;
    let (shm_id, _) = find("wl_shm").ok_or("no wl_shm global")?;
    let (viewporter_id, _) = find("wp_viewporter").ok_or("no wp_viewporter global")?;

    let compositor: WlCompositor = registry.bind(compositor_id, compositor_ver.min(4), &qh, ());
    let subcompositor: WlSubcompositor = registry.bind(subcompositor_id, 1, &qh, ());
    let wl_shm: WlShm = registry.bind(shm_id, 1, &qh, ());
    let viewporter: WpViewporter = registry.bind(viewporter_id, 1, &qh, ());
    let presentation: Option<WpPresentation> =
        find("wp_presentation").map(|(id, ver)| registry.bind(id, ver.min(1), &qh, ()));
    if presentation.is_none() {
        tracing::warn!(target: "shell", "viewport: wp_presentation not advertised; refresh stats unavailable");
    }

    let parent: WlSurface = unsafe {
        let id = ObjectId::from_ptr(WlSurface::interface(), parent_addr as *mut _)
            .map_err(|err| format!("foreign wl_surface: {err}"))?;
        WlSurface::from_id(&conn, id).map_err(|err| format!("wl_surface from_id: {err}"))?
    };

    // One subsurface per pane, both below the toplevel (the transparent UI composites over them).
    // Created after `compositor.rs`'s backdrop, and each `place_below(parent)`, so they stack above
    // that backdrop and below the UI. The second is placed below the first for a deterministic order.
    let mut surfaces: Vec<ViewSurface> = Vec::with_capacity(views.len());
    let mut below: Option<WlSurface> = None;
    for (view, shm_name, shared) in views {
        let surface = compositor.create_surface(&qh, ());
        let subsurface = subcompositor.get_subsurface(&surface, &parent, &qh, ());
        subsurface.set_desync();
        match &below {
            Some(prev) => subsurface.place_below(prev),
            None => subsurface.place_below(&parent),
        }
        subsurface.set_position(0, 0);
        let viewport = viewporter.get_viewport(&surface, &qh, ());

        // Map this view's segment (retry until the engine creates it) + keep an fd for the pool.
        let cname = CString::new(shm_name.clone()).map_err(|_| "bad shm name".to_string())?;
        let mut attempts = 0u32;
        let (pool_fd, base, total, generation) = loop {
            if let Some(mapping) = open_shm(&cname) {
                break mapping;
            }
            attempts += 1;
            if attempts.is_multiple_of(50) {
                let errno = std::io::Error::last_os_error();
                tracing::warn!(target: "shell", "viewport: still waiting for shm '{shm_name}': {errno}");
            }
            thread::sleep(Duration::from_millis(100));
        };
        let pool: WlShmPool = wl_shm.create_pool(pool_fd.as_fd(), total as i32, &qh, ());
        tracing::info!(
            target: "shell",
            "viewport '{}' subsurface up ({total} byte pool)",
            view.wire()
        );

        below = Some(surface.clone());
        let now = Instant::now();
        surfaces.push(ViewSurface {
            view,
            shared,
            surface,
            subsurface,
            viewport,
            cname,
            base,
            total,
            generation,
            header: base as *const u32,
            pool_fd,
            pool,
            buffers: Vec::new(),
            buffer_dims: (0, 0),
            last_seq: 0,
            applied_size: 0,
            applied_pos: u64::MAX, // (0,0) is a legitimate position, so start impossible
            buffer_attached: false,
            parked: false,
            first_commit: true,
            commits: 0,
            last_segment_check: now,
            frame_sent_at: now,
            last_commit_at: now - Duration::from_secs(1),
        });
    }
    let _ = conn.flush();

    let mut last_report = Instant::now();
    loop {
        let _ = queue.dispatch_pending(&mut state);

        let mut committed = false;
        let mut all_parked = true;
        for vs in &mut surfaces {
            if step_view(
                vs,
                &mut state,
                &conn,
                &mut queue,
                &qh,
                &wl_shm,
                &presentation,
            ) {
                committed = true;
            }
            if !vs.parked {
                all_parked = false;
            }
        }

        if last_report.elapsed() >= Duration::from_secs(1) {
            if stats_enabled {
                let stats = &state.stats;
                let mean_delta = if stats.seq_delta_count > 0 {
                    stats.seq_delta_sum as f64 / f64::from(stats.seq_delta_count)
                } else {
                    0.0
                };
                let refresh_hz = if stats.refresh_ns > 0 {
                    1.0e9 / f64::from(stats.refresh_ns)
                } else {
                    0.0
                };
                let commits: u32 = surfaces.iter().map(|vs| vs.commits).sum();
                tracing::info!(
                    target: "shell",
                    "viewport: commit {commits}/s · presented {}/s discarded {}/s · vblank Δ mean {mean_delta:.2} · refresh {refresh_hz:.0} Hz · flags {}",
                    stats.presented,
                    stats.discarded,
                    stats.flags_label(),
                );
            }
            state.stats.reset_window();
            for vs in &mut surfaces {
                vs.commits = 0;
            }
            last_report = Instant::now();
        }

        // An active view paces on the compositor frame callback (no sleep when one committed). When
        // idle, a short rest if a view is still live (waiting on its next frame), longer when parked.
        if !committed {
            thread::sleep(if all_parked {
                Duration::from_millis(20)
            } else {
                Duration::from_micros(500)
            });
        }
    }
}

/// Advance one view by one tick: remap a recreated segment, apply geometry, park/unpark per its
/// shared flag, and attach+commit the next seqlock frame if one arrived and the view is unthrottled.
/// Returns true when this view committed a new frame this tick.
fn step_view(
    vs: &mut ViewSurface,
    state: &mut State,
    conn: &Connection,
    queue: &mut EventQueue<State>,
    qh: &QueueHandle<State>,
    wl_shm: &WlShm,
    presentation: &Option<WpPresentation>,
) -> bool {
    let pending_slot = vs.view.index();

    // The engine recreates the segment when a frame outgrows the slot capacity (and a restarted
    // engine makes a fresh one): same name, new generation. Remap and rebuild the pool + buffers, or this
    // view keeps reading the orphaned old mapping forever.
    if vs.last_segment_check.elapsed() >= Duration::from_millis(250) {
        vs.last_segment_check = Instant::now();
        if let Some((generation, size)) = stat_shm(&vs.cname)
            && (generation != vs.generation || size != vs.total)
            && let Some(mapping) = open_shm(&vs.cname)
        {
            for buffer in vs.buffers.drain(..) {
                buffer.destroy();
            }
            vs.pool.destroy();
            unsafe { libc::munmap(vs.base as *mut _, vs.total) };
            (vs.pool_fd, vs.base, vs.total, vs.generation) = mapping;
            vs.header = vs.base as *const u32;
            vs.pool = wl_shm.create_pool(vs.pool_fd.as_fd(), vs.total as i32, qh, ());
            vs.buffer_dims = (0, 0);
            vs.last_seq = 0;
            vs.buffer_attached = false;
            tracing::debug!(
                target: "shell",
                "viewport '{}' shm segment remapped ({} byte pool)",
                vs.view.wire(),
                vs.total
            );
        }
    }

    // Parked (tab inactive, or a modal owns the region): detach the buffer so the subsurface vanishes
    // and the UI's transparent hole (resolving against the backdrop) paints through. The ring retains
    // the last frame, so an unpark re-attaches it instantly.
    if vs.shared.parked.load(Ordering::Relaxed) {
        if !vs.parked {
            vs.surface.attach(None, 0, 0);
            vs.surface.commit();
            let _ = conn.flush();
            vs.parked = true;
            vs.buffer_attached = false;
            state.frame_pending[pending_slot] = false;
        }
        return false;
    }
    if vs.parked {
        vs.parked = false;
        vs.last_seq = vs.last_seq.wrapping_sub(1); // force a re-attach showing the retained frame
    }

    let magic = unsafe { ptr::read_volatile(vs.header) };
    let seq = unsafe { ptr::read_volatile(vs.header.add(3)) };

    // Geometry first, decoupled from frame arrival: position/destination changes apply to the
    // already-attached buffer immediately (the old frame stretches to the new rect), so the
    // subsurface stays glued to the pane during a dock drag even while the engine renders at the old
    // size.
    let mut geometry_changed = false;
    let packed = vs.shared.size.load(Ordering::Relaxed);
    if packed != vs.applied_size && packed != 0 {
        let (w, h) = unpack_pair(packed);
        if w > 0 && h > 0 {
            vs.viewport.set_destination(w, h);
            vs.applied_size = packed;
            geometry_changed = true;
        }
    }
    let packed_pos = vs.shared.pos.load(Ordering::Relaxed);
    let packed_offset = vs.shared.offset.load(Ordering::Relaxed);
    let combined = {
        let (x, y) = unpack_pair(packed_pos);
        let (ox, oy) = unpack_pair(packed_offset);
        pack_pair(x + ox, y + oy)
    };
    if combined != vs.applied_pos {
        let (x, y) = unpack_pair(combined);
        // Applied on the parent's next commit; the UI paints one every frame.
        vs.subsurface.set_position(x, y);
        vs.applied_pos = combined;
        geometry_changed = true;
    }
    if geometry_changed && vs.buffer_attached {
        vs.surface.commit();
        let _ = conn.flush();
    }

    // Pace on the compositor's frame callback when it flows (= the monitor's refresh). The callback
    // only flows once the subsurface is actually presented, which needs a PARENT commit to adopt it
    // — so fall back to bounded self-paced commits when a callback hasn't arrived in 50 ms (e.g.
    // before first visibility / when occluded).
    let throttled = if state.frame_pending[pending_slot] {
        if vs.frame_sent_at.elapsed() < Duration::from_millis(50) {
            // Force a read from the compositor: the socket may simply not be read while idle,
            // leaving our callback undelivered. A roundtrip flushes and reads (multi-reader safe).
            if vs.frame_sent_at.elapsed() > Duration::from_millis(2) {
                let _ = queue.roundtrip(state);
            }
            state.frame_pending[pending_slot]
        } else {
            state.frame_pending[pending_slot] = false; // callback lost/withheld; self-pace at >=8ms
            vs.last_commit_at.elapsed() < Duration::from_millis(8)
        }
    } else {
        false
    };

    if magic != SHM_MAGIC || seq == vs.last_seq || throttled {
        return false;
    }
    let width = unsafe { ptr::read_volatile(vs.header.add(1)) };
    let height = unsafe { ptr::read_volatile(vs.header.add(2)) };
    let slots = unsafe { ptr::read_volatile(vs.header.add(4)) }.max(1);
    let capacity = unsafe { ptr::read_volatile(vs.header.add(5)) } as usize;
    let pixel_bytes = (width as usize) * (height as usize) * 4;
    if pixel_bytes == 0
        || capacity == 0
        || SHM_HEADER_BYTES + (slots as usize) * capacity > vs.total
        || pixel_bytes > capacity
    {
        return false;
    }

    if vs.buffer_dims != (width, height) {
        for buffer in vs.buffers.drain(..) {
            buffer.destroy();
        }
        for slot in 0..slots {
            let offset = (SHM_HEADER_BYTES + (slot as usize) * capacity) as i32;
            vs.buffers.push(vs.pool.create_buffer(
                offset,
                width as i32,
                height as i32,
                (width * 4) as i32,
                wl_shm::Format::Xrgb8888,
                qh,
                (),
            ));
        }
        vs.buffer_dims = (width, height);
    }

    let buffer = &vs.buffers[(seq % slots) as usize];
    vs.surface.attach(Some(buffer), 0, 0);
    vs.surface.damage(0, 0, i32::MAX, i32::MAX);

    state.frame_pending[pending_slot] = true;
    vs.frame_sent_at = Instant::now();
    vs.surface.frame(qh, pending_slot);
    // One feedback per submission: the compositor answers presented (with the vblank it hit) or
    // discarded (a later commit superseded this one before any repaint).
    if let Some(presentation) = presentation {
        presentation.feedback(&vs.surface, qh, ());
    }
    vs.surface.commit();
    let _ = conn.flush();
    vs.last_commit_at = Instant::now();
    vs.last_seq = seq;
    vs.buffer_attached = true;
    if vs.first_commit {
        tracing::info!(
            target: "shell",
            "viewport '{}' first subsurface commit ({width}x{height} buffer)",
            vs.view.wire()
        );
        vs.first_commit = false;
    }
    vs.commits += 1;
    true
}
