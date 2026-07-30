//! Composite CEF's OSR `on_paint` buffer onto the host toplevel's `wl_surface` via
//! `wl_shm`, alpha preserved. CEF hands back BGRA8888 pre-multiplied; Wayland `Argb8888` is
//! `0xAARRGGBB` little-endian = bytes B,G,R,A, so the CEF buffer maps to `Argb8888` byte-for-byte.
//! The connection shares winit's `wl_display` through `Backend::from_foreign_display` — the same
//! foreign-display integration the engine viewport presenter (`presenter.rs`) uses — so the
//! surface reconstructed from winit's raw `wl_surface` pointer is the real toplevel surface.
//!
//! Not opaque: no `wl_surface::set_opaque_region` is set, so translucent/unpainted UI pixels resolve
//! against whatever is below (the engine subsurfaces / the backdrop) — the transparent-viewport
//! invariant. Double-buffered so a frame the compositor still holds is never overwritten mid-scanout.

use super::window::Handles;
use crate::ShellError;
use crate::dnd::DndEvent;
use cef::Rect;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{DndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_region::WlRegion,
    wl_registry::{self, WlRegistry},
    wl_seat::WlSeat,
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child};

/// The `text/uri-list` MIME the file-drop receiver negotiates. File managers advertise it for a file
/// drag; it carries newline-separated `file://` URIs (RFC 2483).
const URI_LIST_MIME: &str = "text/uri-list";

#[derive(Default)]
struct CompState {
    globals: Vec<(u32, String, u32)>,
    /// The in-flight drag offer (set on `enter`, cleared on `leave`/`drop`).
    dnd_offer: Option<WlDataOffer>,
    /// MIME types the current offer advertised (accumulated from `wl_data_offer::Offer` before `enter`).
    dnd_mimes: Vec<String>,
    /// Whether the current offer advertises `text/uri-list` (a file drag we can accept).
    dnd_has_uri: bool,
    /// Last pointer position of the drag in surface-local device pixels (`drop` carries none).
    dnd_pos: (i32, i32),
    /// Drag steps produced this dispatch, drained by `pump_dnd`.
    dnd_queue: Vec<DndEvent>,
}

impl Dispatch<WlRegistry, ()> for CompState {
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

macro_rules! ignore_events {
    ($($t:ty),* $(,)?) => {
        $(impl Dispatch<$t, ()> for CompState {
            fn event(
                _: &mut Self,
                _: &$t,
                _: <$t as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        })*
    };
}
ignore_events!(
    WlShm,
    WlShmPool,
    WlBuffer,
    WlSurface,
    WlCompositor,
    WlSubcompositor,
    WlSubsurface,
    WlRegion,
    WlSeat,
    WlDataDeviceManager,
);

impl Dispatch<WlDataDevice, ()> for CompState {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        conn: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // A new offer object precedes each drag/selection; its `Offer` events (the advertised MIME
            // types) arrive before `enter`. Reset the MIME accumulator for it.
            wl_data_device::Event::DataOffer { .. } => {
                state.dnd_mimes.clear();
                state.dnd_has_uri = false;
            }
            // The drag entered our surface: adopt the offer, and if it's a file drag accept the URI
            // list under a copy action (both `set_actions` and `accept` are needed for Mutter to
            // negotiate the drop and later allow `finish`).
            wl_data_device::Event::Enter {
                serial, x, y, id, ..
            } => {
                state.dnd_pos = (x as i32, y as i32);
                state.dnd_has_uri = state.dnd_mimes.iter().any(|m| m == URI_LIST_MIME);
                state.dnd_offer = id;
                if let Some(offer) = &state.dnd_offer
                    && state.dnd_has_uri
                {
                    if offer.version() >= 3 {
                        offer.set_actions(DndAction::Copy, DndAction::Copy);
                    }
                    offer.accept(serial, Some(URI_LIST_MIME.to_string()));
                }
                state.dnd_queue.push(DndEvent::Over {
                    x: x as i32,
                    y: y as i32,
                });
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                state.dnd_pos = (x as i32, y as i32);
                state.dnd_queue.push(DndEvent::Over {
                    x: x as i32,
                    y: y as i32,
                });
            }
            wl_data_device::Event::Leave => {
                if let Some(offer) = state.dnd_offer.take() {
                    offer.destroy();
                }
                state.dnd_mimes.clear();
                state.dnd_has_uri = false;
                state.dnd_queue.push(DndEvent::Leave);
            }
            // The drop landed: read the offered URI list off a pipe, parse the file paths, finish the
            // drag, and queue a `Drop` with the (last-known) position. A non-file drag reads nothing.
            wl_data_device::Event::Drop => {
                let (x, y) = state.dnd_pos;
                let paths = match state.dnd_offer.take() {
                    Some(offer) if state.dnd_has_uri => {
                        let paths = read_uri_list(&offer, conn);
                        if offer.version() >= 3 {
                            offer.finish();
                        }
                        offer.destroy();
                        paths
                    }
                    Some(offer) => {
                        offer.destroy();
                        Vec::new()
                    }
                    None => Vec::new(),
                };
                state.dnd_has_uri = false;
                state.dnd_queue.push(DndEvent::Drop { paths, x, y });
            }
            // Clipboard selection offer — not a drag; ignore.
            wl_data_device::Event::Selection { .. } => {}
            _ => {}
        }
    }

    // The `data_offer` event (opcode 0) creates a server-side child object; without this the runtime
    // panics ("Missing event_created_child specialization") at the first drag-enter.
    event_created_child!(CompState, WlDataDevice, [
        0 => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for CompState {
    fn event(
        state: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Collect the offered MIME types (they arrive before `enter`); the action feedback is unused.
        if let wl_data_offer::Event::Offer { mime_type } = event {
            state.dnd_mimes.push(mime_type);
        }
    }
}

/// Composites CPU `on_paint` frames onto the toplevel surface. Lives on the main thread (CEF OSR
/// callbacks fire there), so `paint` is called straight from `on_paint`.
pub struct UiCompositor {
    conn: Connection,
    queue: EventQueue<CompState>,
    state: CompState,
    shm: WlShm,
    compositor: WlCompositor,
    surface: WlSurface,
    fd: Option<OwnedFd>,
    base: *mut u8,
    mapped: usize,
    pool: Option<WlShmPool>,
    buffers: Vec<WlBuffer>,
    dims: (i32, i32),
    /// Forces the next `paint` to damage the whole surface (ignoring CEF's dirty rects) after a pool
    /// (re)allocation — a freshly-sized buffer has no valid prior content for the compositor to keep.
    needs_full_damage: bool,
    frame: u64,
    first_commit: bool,
    /// The shared opaque backdrop: a `wl_subsurface` just below the toplevel that fills the window
    /// with the theme background, so the editor's transparent regions resolve against it (not the
    /// desktop). The engine viewport subsurfaces sit between this and the toplevel.
    backdrop_surface: WlSurface,
    /// Held so the subsurface relationship outlives every frame; never read after setup.
    _backdrop_subsurface: WlSubsurface,
    backdrop_fd: Option<OwnedFd>,
    backdrop_base: *mut u8,
    backdrop_mapped: usize,
    backdrop_pool: Option<WlShmPool>,
    backdrop_buffer: Option<WlBuffer>,
    backdrop_dims: (i32, i32),
    /// The file-drop data device (OS→editor drag) and its seat + manager. Held only so the proxies —
    /// and thus the drop-event subscription on `wl_data_device` — live for the compositor's lifetime;
    /// events arrive through `CompState`'s `Dispatch`, not these fields. `None` if the seat /
    /// data-device-manager globals were absent (OS file-drop then disabled).
    _seat: Option<WlSeat>,
    _data_device_manager: Option<WlDataDeviceManager>,
    _data_device: Option<WlDataDevice>,
}

impl UiCompositor {
    /// Wrap winit's `wl_display` + toplevel `wl_surface` (raw addresses from the backend handles).
    pub fn new(handles: &Handles) -> Result<Self, ShellError> {
        let (wl_display, wl_surface) = (handles.wl_display(), handles.wl_surface());
        let backend = unsafe { Backend::from_foreign_display(wl_display as *mut _) };
        let conn = Connection::from_backend(backend);
        let mut queue = conn.new_event_queue::<CompState>();
        let qh = queue.handle();
        let registry = conn.display().get_registry(&qh, ());
        let mut state = CompState::default();
        queue
            .roundtrip(&mut state)
            .map_err(|e| ShellError::Handle(format!("registry roundtrip: {e}")))?;

        let bind_global = |name: &str, max_ver: u32| -> Option<(u32, u32)> {
            state
                .globals
                .iter()
                .find(|(_, n, _)| n == name)
                .map(|(id, _, ver)| (*id, (*ver).min(max_ver)))
        };

        let (shm_id, shm_ver) = bind_global("wl_shm", 1)
            .ok_or_else(|| ShellError::Handle("no wl_shm global".into()))?;
        let shm: WlShm = registry.bind(shm_id, shm_ver, &qh, ());

        let (comp_id, comp_ver) = bind_global("wl_compositor", 4)
            .ok_or_else(|| ShellError::Handle("no wl_compositor global".into()))?;
        let compositor: WlCompositor = registry.bind(comp_id, comp_ver, &qh, ());

        let (sub_id, sub_ver) = bind_global("wl_subcompositor", 1)
            .ok_or_else(|| ShellError::Handle("no wl_subcompositor global".into()))?;
        let subcompositor: WlSubcompositor = registry.bind(sub_id, sub_ver, &qh, ());

        // OS→editor file drag-and-drop: bind a data device on the seat so `wl_data_device` drop events
        // reach us (winit's Wayland backend never delivers file drops). Non-fatal — a missing seat or
        // manager just means no OS file-drop, not a failed boot. v3 for the `set_actions`/`finish` flow
        // modern compositors (Mutter) drive DnD through.
        let (seat, data_device_manager, data_device) = match (
            bind_global("wl_seat", 5),
            bind_global("wl_data_device_manager", 3),
        ) {
            (Some((seat_id, seat_ver)), Some((ddm_id, ddm_ver))) => {
                let seat: WlSeat = registry.bind(seat_id, seat_ver, &qh, ());
                let ddm: WlDataDeviceManager = registry.bind(ddm_id, ddm_ver, &qh, ());
                let device = ddm.get_data_device(&seat, &qh, ());
                (Some(seat), Some(ddm), Some(device))
            }
            _ => {
                tracing::warn!(
                    target: "shell",
                    "no wl_seat / wl_data_device_manager — OS file drag-and-drop disabled"
                );
                (None, None, None)
            }
        };

        let surface = unsafe {
            let id = ObjectId::from_ptr(WlSurface::interface(), wl_surface as *mut _)
                .map_err(|e| ShellError::Handle(format!("foreign wl_surface: {e}")))?;
            WlSurface::from_id(&conn, id)
                .map_err(|e| ShellError::Handle(format!("wl_surface from_id: {e}")))?
        };

        // The opaque backdrop subsurface, placed just below the toplevel. Desync so it commits
        // independently of the toplevel's UI frames.
        let backdrop_surface = compositor.create_surface(&qh, ());
        let backdrop_subsurface =
            subcompositor.get_subsurface(&backdrop_surface, &surface, &qh, ());
        backdrop_subsurface.place_below(&surface);
        backdrop_subsurface.set_position(0, 0);
        backdrop_subsurface.set_desync();

        Ok(Self {
            conn,
            queue,
            state,
            shm,
            compositor,
            surface,
            fd: None,
            base: std::ptr::null_mut(),
            mapped: 0,
            pool: None,
            buffers: Vec::new(),
            dims: (0, 0),
            needs_full_damage: true,
            frame: 0,
            first_commit: true,
            backdrop_surface,
            _backdrop_subsurface: backdrop_subsurface,
            backdrop_fd: None,
            backdrop_base: std::ptr::null_mut(),
            backdrop_mapped: 0,
            backdrop_pool: None,
            backdrop_buffer: None,
            backdrop_dims: (0, 0),
            _seat: seat,
            _data_device_manager: data_device_manager,
            _data_device: data_device,
        })
    }

    /// (Re)fill the opaque backdrop to the current window size with the theme background
    /// (`oklch(0.145 0 0)` ≈ `#0a0a0a`), and mark it fully opaque. Recreated on size change.
    fn ensure_backdrop(&mut self, w: i32, h: i32) -> Result<(), ShellError> {
        if self.backdrop_dims == (w, h) && self.backdrop_buffer.is_some() {
            return Ok(());
        }
        if let Some(buffer) = self.backdrop_buffer.take() {
            buffer.destroy();
        }
        if let Some(pool) = self.backdrop_pool.take() {
            pool.destroy();
        }
        if !self.backdrop_base.is_null() && self.backdrop_mapped > 0 {
            unsafe { libc::munmap(self.backdrop_base as *mut _, self.backdrop_mapped) };
        }
        let stride = (w * 4) as usize;
        let total = stride * h as usize;
        let fd = unsafe {
            let fd = libc::memfd_create(c"saffron-shell-backdrop".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return Err(ShellError::Handle("backdrop memfd_create".into()));
            }
            if libc::ftruncate(fd, total as libc::off_t) != 0 {
                libc::close(fd);
                return Err(ShellError::Handle("backdrop ftruncate".into()));
            }
            OwnedFd::from_raw_fd(fd)
        };
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                total,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(ShellError::Handle("backdrop mmap".into()));
        }
        // Argb8888 little-endian = bytes B,G,R,A. #0a0a0a opaque = (10,10,10,255).
        unsafe {
            let px: [u8; 4] = [10, 10, 10, 255];
            let words = base as *mut [u8; 4];
            for i in 0..(w as usize * h as usize) {
                std::ptr::write(words.add(i), px);
            }
        }
        let qh = self.queue.handle();
        let pool = self.shm.create_pool(fd.as_fd(), total as i32, &qh, ());
        let buffer = pool.create_buffer(0, w, h, stride as i32, wl_shm::Format::Argb8888, &qh, ());
        // Mark the whole backdrop opaque so the compositor skips blending it.
        let region = self.compositor.create_region(&qh, ());
        region.add(0, 0, w, h);
        self.backdrop_surface.set_opaque_region(Some(&region));
        region.destroy();
        self.backdrop_surface.attach(Some(&buffer), 0, 0);
        self.backdrop_surface.damage(0, 0, i32::MAX, i32::MAX);
        self.backdrop_surface.commit();

        self.backdrop_fd = Some(fd);
        self.backdrop_base = base as *mut u8;
        self.backdrop_mapped = total;
        self.backdrop_pool = Some(pool);
        self.backdrop_buffer = Some(buffer);
        self.backdrop_dims = (w, h);
        Ok(())
    }

    /// (Re)create the double-buffered shm pool when the dimensions change.
    fn ensure(&mut self, w: i32, h: i32) -> Result<(), ShellError> {
        if self.dims == (w, h) && self.pool.is_some() {
            return Ok(());
        }
        // Fresh buffers: the next commit must repaint everything, not a stale dirty subset.
        self.needs_full_damage = true;
        for buffer in self.buffers.drain(..) {
            buffer.destroy();
        }
        if let Some(pool) = self.pool.take() {
            pool.destroy();
        }
        if !self.base.is_null() && self.mapped > 0 {
            unsafe {
                libc::munmap(self.base as *mut _, self.mapped);
            }
        }
        self.base = std::ptr::null_mut();
        self.mapped = 0;
        self.fd = None;

        let stride = (w * 4) as usize;
        let slot = stride * h as usize;
        let total = slot * 2; // two slots, alternated per frame
        let fd = unsafe {
            let fd = libc::memfd_create(c"saffron-shell-ui".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return Err(ShellError::Handle("memfd_create".into()));
            }
            if libc::ftruncate(fd, total as libc::off_t) != 0 {
                libc::close(fd);
                return Err(ShellError::Handle("ftruncate".into()));
            }
            OwnedFd::from_raw_fd(fd)
        };
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                total,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(ShellError::Handle("mmap".into()));
        }

        let qh = self.queue.handle();
        let pool: WlShmPool = self.shm.create_pool(fd.as_fd(), total as i32, &qh, ());
        let mut buffers = Vec::with_capacity(2);
        for i in 0..2 {
            buffers.push(pool.create_buffer(
                (i * slot) as i32,
                w,
                h,
                stride as i32,
                wl_shm::Format::Argb8888,
                &qh,
                (),
            ));
        }

        self.base = base as *mut u8;
        self.mapped = total;
        self.fd = Some(fd);
        self.pool = Some(pool);
        self.buffers = buffers;
        self.dims = (w, h);
        Ok(())
    }

    /// Damage the toplevel for this frame: exactly the regions CEF reported dirty (buffer pixels),
    /// or the whole surface after a (re)allocation / when no rects are given. Full-surface damage
    /// forces the compositor to re-upload the entire `wl_shm` buffer to a texture every commit —
    /// ruinous at high refresh on a large surface (it stalls the compositor, stuttering even the
    /// cursor) — so honoring CEF's dirty rects is what keeps presentation cheap.
    fn apply_damage(&mut self, dirty: Option<&[Rect]>, w: i32, h: i32) {
        if self.needs_full_damage {
            self.needs_full_damage = false;
            self.surface.damage_buffer(0, 0, w, h);
            return;
        }
        match dirty {
            Some(rects) if !rects.is_empty() => {
                for r in rects {
                    self.surface.damage_buffer(r.x, r.y, r.width, r.height);
                }
            }
            _ => self.surface.damage_buffer(0, 0, w, h),
        }
    }

    /// Copy one CEF BGRA frame into the next slot and attach+commit it to the toplevel surface,
    /// damaging only the regions CEF marked dirty.
    pub fn paint(
        &mut self,
        bgra: &[u8],
        w: i32,
        h: i32,
        dirty: Option<&[Rect]>,
    ) -> Result<(), ShellError> {
        if w <= 0 || h <= 0 {
            return Ok(());
        }
        let need = (w as usize) * (h as usize) * 4;
        if bgra.len() < need {
            return Ok(());
        }
        // The opaque backdrop below the toplevel: the editor's transparent panel/gap areas resolve
        // against it instead of the desktop.
        self.ensure_backdrop(w, h)?;
        self.ensure(w, h)?;

        // Full frame into the alternated slot (so each slot is a complete frame — partial damage on
        // top stays correct because the undamaged regions are unchanged from the prior frame).
        let slot = (self.frame % 2) as usize;
        let offset = slot * (w as usize) * 4 * (h as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(bgra.as_ptr(), self.base.add(offset), need);
        }

        self.surface.attach(Some(&self.buffers[slot]), 0, 0);
        self.apply_damage(dirty, w, h);
        self.surface.commit();
        let _ = self.conn.flush();
        let _ = self.queue.dispatch_pending(&mut self.state);
        self.frame += 1;
        if self.first_commit {
            tracing::info!(target: "shell", "first UI commit to toplevel ({w}x{h})");
            self.first_commit = false;
        }
        Ok(())
    }

    /// Flush pending requests, dispatch queued Wayland events, and drain the file-drag steps produced
    /// this tick. Called once per main-loop iteration so OS-drag feedback stays responsive independent
    /// of CEF's paint cadence (the queue is otherwise only advanced inside `paint`).
    pub fn pump_dnd(&mut self) -> Vec<DndEvent> {
        let _ = self.conn.flush();
        let _ = self.queue.dispatch_pending(&mut self.state);
        std::mem::take(&mut self.state.dnd_queue)
    }

    /// Winit events feed no part of this backend's drag source (drops arrive on the compositor's own
    /// `wl_data_device`); present for the backend contract.
    pub fn observe_window_event(&mut self, _event: &winit::event::WindowEvent) {}
}

/// Read the dropped `text/uri-list` off a pipe the compositor writes into, and parse its file paths.
/// The offer's data is delivered by `receive(mime, write_fd)`: the compositor writes the URI list to
/// its dup of the write end and closes it, so we read the read end to EOF. Bounded by a poll timeout so
/// a misbehaving source can never permanently stall the main thread (returns the paths read so far).
fn read_uri_list(offer: &WlDataOffer, conn: &Connection) -> Vec<PathBuf> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        tracing::warn!(target: "shell", "drop: pipe2 failed: {}", std::io::Error::last_os_error());
        return Vec::new();
    }
    // SAFETY: pipe2 succeeded, so both fds are valid and owned by us.
    let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };

    offer.receive(URI_LIST_MIME.to_string(), write_end.as_fd());
    // The request must reach the server before we block reading, and our write end must be closed so
    // the read sees EOF once the compositor finishes writing.
    let _ = conn.flush();
    drop(write_end);

    let read_fd = read_end.as_raw_fd();
    // Non-blocking + poll so the read can't hang the loop; the payload is a few hundred bytes the
    // compositor writes at once, so this returns in well under the timeout in practice.
    unsafe {
        let flags = libc::fcntl(read_fd, libc::F_GETFL);
        libc::fcntl(read_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = std::time::Duration::from_secs(1);
    let start = std::time::Instant::now();
    loop {
        let mut pfd = libc::pollfd {
            fd: read_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let remaining = deadline.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            tracing::warn!(target: "shell", "drop: timed out reading uri-list");
            break;
        }
        let ready = unsafe { libc::poll(&mut pfd, 1, remaining.as_millis().min(1000) as i32) };
        if ready <= 0 {
            if ready < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            break;
        }
        let n = unsafe {
            libc::read(
                read_fd,
                chunk.as_mut_ptr() as *mut libc::c_void,
                chunk.len(),
            )
        };
        match n {
            0 => break,
            n if n > 0 => buf.extend_from_slice(&chunk[..n as usize]),
            _ => {
                let err = std::io::Error::last_os_error();
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) {
                    continue;
                }
                break;
            }
        }
    }
    parse_uri_list(&buf)
}

/// Parse an RFC 2483 `text/uri-list` into filesystem paths: newline-separated (CRLF or LF), `#`-comment
/// and blank lines skipped, only `file://` URIs kept, the scheme + optional host stripped, and the path
/// percent-decoded.
fn parse_uri_list(bytes: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(bytes);
    let mut paths = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("file://") else {
            continue;
        };
        // `file://host/path` — drop the host component (up to the path's leading '/'); a bare
        // `file:///path` leaves `rest` already starting at '/'.
        let path = match rest.find('/') {
            Some(0) => rest,
            Some(slash) => &rest[slash..],
            None => continue,
        };
        paths.push(PathBuf::from(crate::scheme::percent_decode(path)));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::parse_uri_list;
    use std::path::PathBuf;

    #[test]
    fn parses_file_uris_skipping_comments_and_decoding() {
        let list = "#comment\r\n\
                    file:///home/user/a.glb\r\n\
                    file://localhost/home/user/b.png\r\n\
                    file:///home/user/My%20Model.glb\r\n\
                    \r\n\
                    https://example.com/skip.png\r\n";
        assert_eq!(
            parse_uri_list(list.as_bytes()),
            vec![
                PathBuf::from("/home/user/a.glb"),
                PathBuf::from("/home/user/b.png"),
                PathBuf::from("/home/user/My Model.glb"),
            ]
        );
    }

    #[test]
    fn tolerates_lf_only_and_ignores_non_file_lines() {
        let list = "file:///a.hdr\nnot-a-uri\nfile:///b.smat\n";
        assert_eq!(
            parse_uri_list(list.as_bytes()),
            vec![PathBuf::from("/a.hdr"), PathBuf::from("/b.smat")]
        );
    }
}
