//! Presents the engine's shared-memory frames on CALayers placed between the backdrop and the UI
//! layer (`zPosition` −1). One reader thread per view walks the engine's seqlock ring
//! (`crate::viewport`), copies each new frame into a BGRA [`IoSurfacePool`] surface, and parks it
//! in a ready slot; an AppKit display link (`NSView.displayLink`, retargeting refresh across
//! monitor hops) swaps ready surfaces into the layers' contents on the main run loop, applying
//! the pane geometry and park state from [`ViewportShared`] in the same `CATransaction`.

use super::compositor::{Z_ENGINE, surface_as_contents};
use super::iosurface::{IoSurfacePool, SendSurface, write_bgra};
use super::window::Handles;
use crate::viewport::{
    SHM_HEADER_BYTES, SHM_MAGIC, View, ViewportShared, Viewports, open_shm, stat_shm, unpack_pair,
};
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::NSView;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSObject, NSRunLoop};
use objc2_quartz_core::{CADisplayLink, CALayer, CATransaction};
use std::ffi::CString;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// One view's cross-thread handoff: the reader parks the newest frame here; the display-link tick
/// takes it. A newer frame replaces an untaken older one (only the latest matters).
struct ReadySlot {
    frame: Mutex<Option<SendSurface>>,
}

/// Everything the display-link tick owns per view: the layer, the shared geometry cell, and the
/// handoff slot.
struct TickView {
    layer: Retained<CALayer>,
    shared: Arc<ViewportShared>,
    ready: Arc<ReadySlot>,
    /// Applied park state (avoids re-hiding every tick).
    parked: bool,
}

/// The display-link target's state.
pub struct TickState {
    views: Vec<TickView>,
    refresh_out: Arc<AtomicU32>,
    link_duration_logged: bool,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = std::cell::RefCell<TickState>]
    pub struct PresenterTick;

    impl PresenterTick {
        #[unsafe(method(onDisplayLink:))]
        fn on_display_link(&self, link: &CADisplayLink) {
            self.tick(link);
        }
    }

    unsafe impl NSObjectProtocol for PresenterTick {}
);

impl PresenterTick {
    fn new(mtm: MainThreadMarker, state: TickState) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(std::cell::RefCell::new(state));
        // SAFETY: plain NSObject init on the allocated, ivar-seeded instance.
        unsafe { msg_send![super(this), init] }
    }

    /// One vsync tick: publish the refresh rate, then for each view apply park/geometry and swap
    /// in a ready frame — all inside one `CATransaction` with implicit animations disabled.
    fn tick(&self, link: &CADisplayLink) {
        let mut state = self.ivars().borrow_mut();
        let state = &mut *state;

        // The link's `duration` is the seconds-per-frame of the display the view is on; publish it
        // as millihertz for `viewport_refresh_hz` (the AppKit-vended link retargets on monitor
        // hops, so this follows the window).
        let duration = link.duration();
        if duration > 0.0 {
            let mhz = (1000.0 / duration).round() as u32;
            state.refresh_out.store(mhz, Ordering::Relaxed);
            if !state.link_duration_logged {
                tracing::info!(
                    target: "shell",
                    "viewport display link up ({:.0} Hz)",
                    1.0 / duration
                );
                state.link_duration_logged = true;
            }
        }

        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for view in &mut state.views {
            let parked = view.shared.parked.load(Ordering::Relaxed);
            if parked != view.parked {
                view.layer.setHidden(parked);
                view.parked = parked;
            }
            if parked {
                continue;
            }

            // Pane geometry in logical points, straight from the shared cells the
            // `set_viewport_bounds` command writes (`offset` is the UI surface's origin within
            // the toplevel — always 0 in the CEF shell, composed for parity with the coordinate
            // contract). The layer stretches its contents to the frame (default
            // `contentsGravity`), so a stale-size frame stays glued to the pane during a dock
            // drag exactly like the Wayland `wp_viewport` destination.
            let (px, py) = unpack_pair(view.shared.pos.load(Ordering::Relaxed));
            let (ox, oy) = unpack_pair(view.shared.offset.load(Ordering::Relaxed));
            let (x, y) = (px + ox, py + oy);
            let (w, h) = unpack_pair(view.shared.size.load(Ordering::Relaxed));
            if w > 0 && h > 0 {
                view.layer.setFrame(CGRect::new(
                    CGPoint::new(f64::from(x), f64::from(y)),
                    CGSize::new(f64::from(w), f64::from(h)),
                ));
            }

            if let Some(SendSurface(surface)) = view
                .ready
                .frame
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                // SAFETY: main thread; the layer retains the surface as its contents.
                unsafe { view.layer.setContents(Some(surface_as_contents(&surface))) };
            }
        }
        CATransaction::commit();
    }
}

/// Build the per-view layers + display link on the main thread and spawn the shm reader threads.
pub fn install(handles: &Handles, scene_shm: String, asset_shm: String, viewports: &Viewports) {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::error!(target: "shell", "viewport presenter: not on the main thread");
        return;
    };
    let view = handles.ns_view() as *const NSView;
    // SAFETY: winit's live NSView, on the main thread; layers attach beneath the UI layer by
    // z-position.
    let root = unsafe {
        let view = &*view;
        view.setWantsLayer(true);
        view.layer()
    };
    let Some(root) = root else {
        tracing::error!(target: "shell", "viewport presenter: view has no backing layer");
        return;
    };

    let mut tick_views = Vec::with_capacity(2);
    for (view_id, shm_name) in [(View::Scene, scene_shm), (View::AssetPreview, asset_shm)] {
        let layer = CALayer::new();
        layer.setZPosition(Z_ENGINE);
        layer.setHidden(true);
        layer.setOpaque(true);
        root.addSublayer(&layer);

        let ready = Arc::new(ReadySlot {
            frame: Mutex::new(None),
        });
        let shared = Arc::clone(viewports.view(view_id));

        // The reader thread: waits for the engine's segment, then walks the seqlock ring and
        // parks each new frame in the ready slot as an IOSurface.
        let reader_ready = Arc::clone(&ready);
        let reader_shared = Arc::clone(&shared);
        thread::spawn(move || {
            read_view(view_id, &shm_name, &reader_shared, &reader_ready);
        });

        tick_views.push(TickView {
            layer,
            shared,
            ready,
            parked: true,
        });
    }

    let tick = PresenterTick::new(
        mtm,
        TickState {
            views: tick_views,
            refresh_out: Arc::clone(&viewports.refresh_mhz),
            link_duration_logged: false,
        },
    );
    // SAFETY: main thread; the run loop retains the link, the link retains the target. Leaking
    // the target keeps the tick alive for the shell's lifetime (the presenter never uninstalls).
    unsafe {
        let view = &*view;
        let link = view.displayLinkWithTarget_selector(&tick, sel!(onDisplayLink:));
        link.addToRunLoop_forMode(
            &NSRunLoop::mainRunLoop(),
            objc2_foundation::NSRunLoopCommonModes,
        );
        std::mem::forget(link);
        std::mem::forget(tick);
    }
}

/// The per-view reader loop: open the segment (retrying until the engine creates it), then walk
/// the seqlock header — remapping when the engine recreates the segment — and copy each new frame
/// into a pool surface parked in the ready slot. Geometry/park stay with the tick; this thread
/// only moves pixels.
fn read_view(view: View, shm_name: &str, shared: &Arc<ViewportShared>, ready: &Arc<ReadySlot>) {
    let Ok(cname) = CString::new(shm_name) else {
        tracing::error!(target: "shell", "viewport '{}': bad shm name", view.wire());
        return;
    };

    let mut attempts = 0u32;
    let (mut _fd, mut base, mut total, mut seg_ino) = loop {
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
    tracing::info!(
        target: "shell",
        "viewport '{}' reader up ({total} byte segment)",
        view.wire()
    );

    let mut pool = IoSurfacePool::new();
    let mut header = base as *const u32;
    let mut last_seq = 0u32;
    let mut last_segment_check = Instant::now();
    let mut first_frame = true;

    loop {
        // The engine recreates the segment when a frame outgrows the slot capacity (and a
        // restarted engine makes a fresh one): same name, new inode. Remap or this view keeps
        // reading the orphaned old mapping forever.
        if last_segment_check.elapsed() >= Duration::from_millis(250) {
            last_segment_check = Instant::now();
            if let Some((ino, size)) = stat_shm(&cname)
                && (ino != seg_ino || size != total)
                && let Some(mapping) = open_shm(&cname)
            {
                // SAFETY: unmapping the exact prior mapping before adopting the new one.
                unsafe { libc::munmap(base as *mut _, total) };
                (_fd, base, total, seg_ino) = mapping;
                header = base as *const u32;
                last_seq = 0;
                tracing::debug!(
                    target: "shell",
                    "viewport '{}' shm segment remapped ({total} bytes)",
                    view.wire()
                );
            }
        }

        // A parked view's frames go unconsumed; idle instead of copying them.
        if shared.parked.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(20));
            continue;
        }

        // SAFETY: `base` maps at least the 32-byte header; volatile reads of the producer's
        // seqlock fields. The multi-slot ring makes a torn read of the current slot unlikely; the
        // (cosmetic) risk is accepted over cross-process backpressure.
        let magic = unsafe { ptr::read_volatile(header) };
        let seq = unsafe { ptr::read_volatile(header.add(3)) };
        if magic != SHM_MAGIC || seq == last_seq {
            thread::sleep(Duration::from_millis(2));
            continue;
        }
        let width = unsafe { ptr::read_volatile(header.add(1)) };
        let height = unsafe { ptr::read_volatile(header.add(2)) };
        let slots = unsafe { ptr::read_volatile(header.add(4)) }.max(1);
        let capacity = unsafe { ptr::read_volatile(header.add(5)) } as usize;
        let pixel_bytes = (width as usize) * (height as usize) * 4;
        if pixel_bytes == 0
            || capacity == 0
            || SHM_HEADER_BYTES + (slots as usize) * capacity > total
            || pixel_bytes > capacity
        {
            thread::sleep(Duration::from_millis(2));
            continue;
        }

        pool.ensure(width as usize, height as usize);
        let Some(surface) = pool.acquire() else {
            // Every pool surface is in flight; retry next tick with the same seq.
            thread::sleep(Duration::from_millis(2));
            continue;
        };
        let slot_offset = SHM_HEADER_BYTES + ((seq % slots) as usize) * capacity;
        // SAFETY: the slot lies within `[base, base + total)` per the validation above.
        let src = unsafe { std::slice::from_raw_parts(base.add(slot_offset), pixel_bytes) };
        write_bgra(&surface, src, width as usize, height as usize);
        last_seq = seq;
        if first_frame {
            tracing::info!(
                target: "shell",
                "viewport '{}' first frame ({width}x{height})",
                view.wire()
            );
            first_frame = false;
        }

        *ready
            .frame
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(SendSurface(surface));
    }
}
