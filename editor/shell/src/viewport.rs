//! The backend-neutral half of the engine-viewport presenter: the view identities, the lock-free
//! per-view geometry cells the lifecycle commands write, and the engine's shared-memory frame-ring
//! ABI (open/probe + the header layout the seqlock reader walks). The *presentation* — what turns a
//! ring slot into pixels on screen — lives in `backend::presenter`.
//!
//! The ring ABI is frozen against the engine's `shm_publish` producer: a 32-byte header of eight
//! `u32`s (`magic, width, height, seq, slots, capacity, _, _`), then `slots` fixed-`capacity` frame
//! slots addressed `seq % slots`, pixels `Xrgb8888` (bytes B,G,R,X little-endian).

use std::ffi::CStr;
use std::os::fd::{FromRawFd, OwnedFd};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// The shm header magic ("SFV2").
pub const SHM_MAGIC: u32 = 0x5346_5632;
/// The shm header size in bytes.
pub const SHM_HEADER_BYTES: usize = 32;

/// A viewport view, identified by its engine wire token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Scene,
    AssetPreview,
}

impl View {
    pub fn wire(self) -> &'static str {
        match self {
            View::Scene => "scene",
            View::AssetPreview => "assetPreview",
        }
    }

    pub fn from_wire(wire: &str) -> Option<View> {
        match wire {
            "scene" => Some(View::Scene),
            "assetPreview" => Some(View::AssetPreview),
            _ => None,
        }
    }

    pub fn index(self) -> usize {
        match self {
            View::Scene => 0,
            View::AssetPreview => 1,
        }
    }
}

/// Pack two non-negative `i32`s into a `u64` (`a << 32 | b`) for lock-free atomic storage.
pub fn pack_pair(a: i32, b: i32) -> u64 {
    ((a.max(0) as u64) << 32) | (b.max(0) as u64)
}

/// Unpack what [`pack_pair`] stored.
pub fn unpack_pair(packed: u64) -> (i32, i32) {
    (
        ((packed >> 32) & 0xffff_ffff) as i32,
        (packed & 0xffff_ffff) as i32,
    )
}

/// One view's live geometry, shared lock-free between the command thread (writer) and the present
/// loop (reader). Positions/sizes are logical CSS pixels relative to the UI surface; the present
/// loop scales the engine's device-pixel frame to the destination rect.
#[derive(Default)]
pub struct ViewportShared {
    /// Packed logical `(x << 32 | y)`, the pane's origin within the UI surface.
    pub(crate) pos: AtomicU64,
    /// Packed logical `(w << 32 | h)`, the pane's size.
    pub(crate) size: AtomicU64,
    /// Packed logical origin of the UI surface within the toplevel — always `0` in the CEF shell,
    /// where the UI *is* the toplevel surface; kept for parity with the presenter's coordinate math.
    pub(crate) offset: AtomicU64,
    /// Whether this view is parked (tab inactive / modal over it): the present loop hides the
    /// view's surface so the UI's transparent hole shows the backdrop.
    pub(crate) parked: AtomicBool,
}

impl ViewportShared {
    pub fn set_bounds(&self, x: i32, y: i32, width: i32, height: i32) {
        self.pos.store(pack_pair(x, y), Ordering::Relaxed);
        self.size.store(pack_pair(width, height), Ordering::Relaxed);
    }

    pub fn set_parked(&self, parked: bool) {
        self.parked.store(parked, Ordering::Relaxed);
    }
}

/// The two per-view shared cells plus the present loop's learned refresh. Held by `ShellState`.
#[derive(Default)]
pub struct Viewports {
    pub(crate) views: [Arc<ViewportShared>; 2],
    /// The presented output's refresh in millihertz (`144000` = 144 Hz), written by the present
    /// loop. `0` until the first presented frame reports it.
    pub(crate) refresh_mhz: Arc<AtomicU32>,
}

impl Viewports {
    pub fn view(&self, view: View) -> &Arc<ViewportShared> {
        &self.views[view.index()]
    }

    /// The presented output's refresh in millihertz (`0` if not yet known).
    pub fn refresh_mhz(&self) -> u32 {
        self.refresh_mhz.load(Ordering::Relaxed)
    }
}

/// Open a published engine segment read-only and map it whole: `(fd, base, size, inode)`. `None`
/// while the engine has not created it (the caller retries) or it is too small for the header.
pub fn open_shm(name: &CStr) -> Option<(OwnedFd, *const u8, usize, u64)> {
    unsafe {
        let fd = libc::shm_open(name.as_ptr(), libc::O_RDWR, 0);
        if fd < 0 {
            return None;
        }
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut st) != 0 || (st.st_size as usize) < SHM_HEADER_BYTES {
            libc::close(fd);
            return None;
        }
        let size = st.st_size as usize;
        let base = libc::mmap(
            ptr::null_mut(),
            size,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            0,
        );
        if base == libc::MAP_FAILED {
            libc::close(fd);
            return None;
        }
        Some((
            OwnedFd::from_raw_fd(fd),
            base as *const u8,
            size,
            st.st_ino as u64,
        ))
    }
}

/// Inode + size of the segment currently behind `name` — a cheap probe to detect the engine
/// recreating it (bigger frames, or an engine restart).
pub fn stat_shm(name: &CStr) -> Option<(u64, usize)> {
    unsafe {
        let fd = libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0);
        if fd < 0 {
            return None;
        }
        let mut st: libc::stat = std::mem::zeroed();
        let ok = libc::fstat(fd, &mut st) == 0;
        libc::close(fd);
        if ok {
            Some((st.st_ino as u64, st.st_size as usize))
        } else {
            None
        }
    }
}
