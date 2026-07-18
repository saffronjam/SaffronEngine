//! BGRA IOSurface pools for CALayer presentation. An IOSurface set as `CALayer.contents` is
//! sampled by the WindowServer directly on the GPU (the Chromium/Firefox macOS compositing path),
//! so presenting a frame costs one memcpy into a pool surface and a pointer swap. Core Animation
//! compares `contents` by pointer — re-setting the same object is a no-op — so frames rotate
//! through a small pool, never writing a surface the WindowServer still reads
//! (`IOSurfaceIsInUse`).

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};

/// `'BGRA'` — 32-bit little-endian B,G,R,A bytes, matching both CEF's `on_paint` buffer and the
/// engine's `Xrgb8888` slots byte-for-byte.
const FOURCC_BGRA: i32 = i32::from_be_bytes(*b"BGRA");

/// How many surfaces each stream rotates through: one on screen, one being written, one slack for
/// a WindowServer still holding the previous frame.
const POOL_DEPTH: usize = 3;

/// A retained IOSurface that may cross threads. IOSurface is a kernel-managed buffer designed for
/// cross-process (and cross-thread) sharing; retain/release and the pixel memory are thread-safe.
pub struct SendSurface(pub CFRetained<IOSurfaceRef>);
// SAFETY: IOSurface is documented as a cross-process shareable framebuffer object; its CF
// retain/release is thread-safe, and pixel access is guarded by the lock/unlock protocol.
unsafe impl Send for SendSurface {}

/// A fixed-depth pool of same-sized BGRA surfaces.
pub struct IoSurfacePool {
    surfaces: Vec<CFRetained<IOSurfaceRef>>,
    width: usize,
    height: usize,
}

impl IoSurfacePool {
    pub fn new() -> Self {
        Self {
            surfaces: Vec::new(),
            width: 0,
            height: 0,
        }
    }

    /// (Re)allocate the pool when the frame size changes.
    pub fn ensure(&mut self, width: usize, height: usize) {
        if self.width == width && self.height == height && !self.surfaces.is_empty() {
            return;
        }
        self.surfaces.clear();
        for _ in 0..POOL_DEPTH {
            if let Some(surface) = create_surface(width, height) {
                self.surfaces.push(surface);
            }
        }
        self.width = width;
        self.height = height;
    }

    /// A surface the WindowServer is not currently reading, or `None` when the whole pool is in
    /// flight (the caller drops the frame; a newer one follows).
    pub fn acquire(&mut self) -> Option<CFRetained<IOSurfaceRef>> {
        self.surfaces
            .iter()
            .find(|surface| !surface.is_in_use())
            .cloned()
    }
}

/// Create one BGRA surface. The kernel picks `bytes_per_row` (aligned); writers honor it.
fn create_surface(width: usize, height: usize) -> Option<CFRetained<IOSurfaceRef>> {
    // SAFETY: the property keys are the documented IOSurface creation keys; the dictionary
    // borrows the CFNumbers for the duration of the call.
    unsafe {
        let width_n = CFNumber::new_i32(width as i32);
        let height_n = CFNumber::new_i32(height as i32);
        let bpe = CFNumber::new_i32(4);
        let format = CFNumber::new_i32(FOURCC_BGRA);
        let keys: [&CFString; 4] = [
            objc2_io_surface::kIOSurfaceWidth,
            objc2_io_surface::kIOSurfaceHeight,
            objc2_io_surface::kIOSurfaceBytesPerElement,
            objc2_io_surface::kIOSurfacePixelFormat,
        ];
        let values: [&CFNumber; 4] = [&width_n, &height_n, &bpe, &format];
        let properties = CFDictionary::from_slices(&keys, &values);
        IOSurfaceRef::new(properties.as_opaque())
    }
}

/// Copy a tightly-packed BGRA frame into `surface`, honoring the surface's row stride. The
/// write-unlock bumps the surface seed, which is how Core Animation notices new content behind an
/// unchanged pointer (the pool rotation makes the pointer change anyway).
pub fn write_bgra(surface: &IOSurfaceRef, src: &[u8], width: usize, height: usize) {
    let row_bytes = width * 4;
    if src.len() < row_bytes * height {
        return;
    }
    // SAFETY: lock/unlock bracket CPU access per the IOSurface protocol; `base_address` is valid
    // for `alloc_size` bytes while locked, and the row loop stays within both buffers.
    unsafe {
        let mut seed = 0u32;
        if surface.lock(IOSurfaceLockOptions::empty(), &mut seed) != 0 {
            return;
        }
        let dst_stride = surface.bytes_per_row();
        let dst = surface.base_address().as_ptr().cast::<u8>();
        if dst_stride == row_bytes {
            std::ptr::copy_nonoverlapping(src.as_ptr(), dst, row_bytes * height);
        } else {
            for row in 0..height {
                std::ptr::copy_nonoverlapping(
                    src.as_ptr().add(row * row_bytes),
                    dst.add(row * dst_stride),
                    row_bytes,
                );
            }
        }
        surface.unlock(IOSurfaceLockOptions::empty(), &mut seed);
    }
}
