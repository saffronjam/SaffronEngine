//! GPU conformance gate for the shared authoritative spatial numerics.

use std::mem::size_of;
use std::sync::Arc;

use crate::compute_dispatch::{ComputeBuffer, run_compute};
use crate::conformance::spatial_numeric_reference_words;
use crate::{Device, SurfaceSource, validation_issue_count};

#[test]
fn rust_and_slang_spatial_goldens_are_byte_identical_on_gpu() {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => Arc::new(device),
        Err(error) => {
            eprintln!("skipping: no Vulkan device obtainable ({error})");
            return;
        }
    };
    let expected = spatial_numeric_reference_words();
    let before = validation_issue_count();
    let buffers = run_compute(
        Arc::clone(&device),
        "spatial_numeric_test",
        vec![ComputeBuffer::zeroed(expected.len() * size_of::<u32>())],
        [1, 1, 1],
    )
    .expect("spatial numeric GPU dispatch");
    let actual = buffers[0]
        .chunks_exact(size_of::<u32>())
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    device.wait_idle().expect("idle before teardown");
    drop(device);
    assert_eq!(validation_issue_count(), before);
}
