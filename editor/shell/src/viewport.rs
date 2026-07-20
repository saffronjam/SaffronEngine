//! The backend-neutral half of the engine-viewport presenter: the view identities, the lock-free
//! per-view geometry cells the lifecycle commands write, and the engine's shared-memory frame-ring
//! ABI (open/probe + the header layout the seqlock reader walks). The *presentation* — what turns a
//! ring slot into pixels on screen — lives in `backend::presenter`.
//!
//! The ring ABI is frozen against the engine's `shm_publish` producer: a 32-byte header of eight
//! `u32`s (`magic, width, height, seq, slots, capacity, generation_lo, generation_hi`), then
//! `slots` fixed-`capacity` frame slots addressed `seq % slots`, pixels `Xrgb8888` (bytes B,G,R,X
//! little-endian).

use std::ffi::CStr;
use std::os::fd::{FromRawFd, OwnedFd};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// The shm header magic ("SFV3").
pub const SHM_MAGIC: u32 = 0x5346_5633;
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

/// Open a published engine segment read-only and map it whole: `(fd, base, size, generation)`.
/// `None` while the engine has not created it (the caller retries) or it is too small for the
/// header.
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
        let header = base.cast::<u32>();
        let magic = ptr::read_volatile(header);
        let generation = u64::from(ptr::read_volatile(header.add(6)))
            | (u64::from(ptr::read_volatile(header.add(7))) << 32);
        if magic != SHM_MAGIC || generation == 0 {
            libc::munmap(base, size);
            libc::close(fd);
            return None;
        }
        Some((
            OwnedFd::from_raw_fd(fd),
            base as *const u8,
            size,
            generation,
        ))
    }
}

/// Generation + size of the segment currently behind `name` — a cheap probe to detect the engine
/// recreating it for bigger frames or after an engine restart.
pub fn stat_shm(name: &CStr) -> Option<(u64, usize)> {
    unsafe {
        let fd = libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0);
        if fd < 0 {
            return None;
        }
        let mut st: libc::stat = std::mem::zeroed();
        let ok = libc::fstat(fd, &mut st) == 0 && (st.st_size as usize) >= SHM_HEADER_BYTES;
        if !ok {
            libc::close(fd);
            return None;
        }
        let base = libc::mmap(
            ptr::null_mut(),
            SHM_HEADER_BYTES,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            0,
        );
        libc::close(fd);
        if base != libc::MAP_FAILED {
            let header = base.cast::<u32>();
            let magic = ptr::read_volatile(header);
            let generation = u64::from(ptr::read_volatile(header.add(6)))
                | (u64::from(ptr::read_volatile(header.add(7))) << 32);
            libc::munmap(base, SHM_HEADER_BYTES);
            (magic == SHM_MAGIC && generation != 0).then_some((generation, st.st_size as usize))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    struct ShmFixture {
        name: CString,
    }

    impl ShmFixture {
        fn new() -> Self {
            let name = CString::new(format!("/svt-{}", std::process::id())).unwrap();
            unsafe { libc::shm_unlink(name.as_ptr()) };
            Self { name }
        }

        fn create(&self, generation: u64) {
            unsafe {
                let fd = libc::shm_open(
                    self.name.as_ptr(),
                    libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                    0o600,
                );
                assert!(
                    fd >= 0,
                    "create test segment: {}",
                    std::io::Error::last_os_error()
                );
                assert_eq!(libc::ftruncate(fd, SHM_HEADER_BYTES as libc::off_t), 0);
                let base = libc::mmap(
                    ptr::null_mut(),
                    SHM_HEADER_BYTES,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                );
                assert_ne!(base, libc::MAP_FAILED);
                let header = base.cast::<u32>();
                ptr::write_volatile(header.add(6), generation as u32);
                ptr::write_volatile(header.add(7), (generation >> 32) as u32);
                ptr::write_volatile(header, SHM_MAGIC);
                assert_eq!(libc::munmap(base, SHM_HEADER_BYTES), 0);
                assert_eq!(libc::close(fd), 0);
            }
        }

        fn unlink(&self) {
            assert_eq!(unsafe { libc::shm_unlink(self.name.as_ptr()) }, 0);
        }
    }

    impl Drop for ShmFixture {
        fn drop(&mut self) {
            unsafe { libc::shm_unlink(self.name.as_ptr()) };
        }
    }

    #[test]
    fn generation_identifies_same_size_segment_replacement() {
        let fixture = ShmFixture::new();
        fixture.create(0x1111_2222_3333_4444);

        let first = stat_shm(&fixture.name).expect("probe first segment");
        let (fd, base, size, generation) = open_shm(&fixture.name).expect("open first segment");
        assert_eq!(generation, first.0);
        unsafe { libc::munmap(base.cast_mut().cast(), size) };
        drop(fd);

        fixture.unlink();
        fixture.create(0x5555_6666_7777_8888);
        let second = stat_shm(&fixture.name).expect("probe replacement segment");

        assert_eq!(second.1, first.1);
        assert_ne!(second.0, first.0);
    }
}
