//! Phase 3 (core): composite CEF's OSR `on_paint` buffer onto the host toplevel's `wl_surface` via
//! `wl_shm`, alpha preserved. CEF hands back BGRA8888 pre-multiplied; Wayland `Argb8888` is
//! `0xAARRGGBB` little-endian = bytes B,G,R,A, so the CEF buffer maps to `Argb8888` byte-for-byte.
//! The connection shares winit's `wl_display` through `Backend::from_foreign_display` — the same
//! foreign-display integration the engine viewport presenter (`wayland_viewport.rs`) uses — so the
//! surface reconstructed from winit's raw `wl_surface` pointer is the real toplevel surface.
//!
//! Not opaque: no `wl_surface::set_opaque_region` is set, so translucent/unpainted UI pixels resolve
//! against whatever is below (Phase 6's engine subsurfaces / the desktop) — the transparent-viewport
//! invariant. Double-buffered so a frame the compositor still holds is never overwritten mid-scanout.

use crate::ShellError;
use cef::Rect;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_region::WlRegion,
    wl_registry::{self, WlRegistry},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

#[derive(Default)]
struct CompState {
    globals: Vec<(u32, String, u32)>,
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
);

/// Composites CPU `on_paint` frames onto the toplevel surface. Lives on the main thread (CEF OSR
/// callbacks fire there), so `paint` is called straight from `on_paint`.
pub struct ToplevelCompositor {
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
    /// desktop). The engine viewport subsurfaces will later sit between this and the toplevel.
    backdrop_surface: WlSurface,
    #[allow(dead_code)]
    // held so the subsurface relationship persists for the compositor's lifetime
    backdrop_subsurface: WlSubsurface,
    backdrop_fd: Option<OwnedFd>,
    backdrop_base: *mut u8,
    backdrop_mapped: usize,
    backdrop_pool: Option<WlShmPool>,
    backdrop_buffer: Option<WlBuffer>,
    backdrop_dims: (i32, i32),
}

impl ToplevelCompositor {
    /// Wrap winit's `wl_display` + toplevel `wl_surface` (raw addresses from `raw-window-handle`).
    pub fn new(wl_display: usize, wl_surface: usize) -> Result<Self, ShellError> {
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
            backdrop_subsurface,
            backdrop_fd: None,
            backdrop_base: std::ptr::null_mut(),
            backdrop_mapped: 0,
            backdrop_pool: None,
            backdrop_buffer: None,
            backdrop_dims: (0, 0),
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
}
