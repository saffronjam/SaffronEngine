//! Emits physical-GPU spatial-numeric and resident graph-program evidence as JSON.

use std::sync::Arc;

use saffron_rendering::{Device, SurfaceSource, capture_compute_conformance};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let device = Arc::new(Device::new(&SurfaceSource::Offscreen)?);
    let evidence = capture_compute_conformance(device)?;
    evidence.write_json(std::io::stdout().lock())?;
    Ok(())
}
