//! Stable handles, global GPU arenas, immutable tables, and frame-safe uploads.

mod arena;
mod pso_bin;
mod records;
mod tables;
mod upload;

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use ash::vk;
use bytemuck::{Pod, Zeroable};
use saffron_material::{
    AlphaClassification, CoverageMipMetadata, CoverageSource, OpacityMicromapDerivation,
    SurfaceModel,
};

use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::gpu_types::MaterialParamsData;
use crate::render_graph::{
    RenderGraph, RgBufferLifetime, RgBufferRange, RgBufferResource, RgPass, RgUsage,
};
use crate::resources::{Buffer, DeviceResources};
use crate::{Device, Error, Result};

pub use arena::*;
pub use pso_bin::*;
pub use records::*;
pub use tables::*;
pub use upload::*;

const INITIAL_ARENA_BYTES: u64 = 64 * 1024;

const INITIAL_UPLOAD_BYTES: u64 = 256 * 1024;

const GPU_COPY_ALIGNMENT: u64 = 4;

/// Version of the byte-locked Rust/Slang global GPU data ABI.
pub const GLOBAL_GPU_DATA_ABI_VERSION: u32 = 2;

/// PSO-bin shift for geometry representation.
pub const GPU_PSO_REPRESENTATION_SHIFT: u32 = 0;

/// PSO-bin shift for the pass-independent material class.
pub const GPU_PSO_MATERIAL_SHIFT: u32 = 2;

/// PSO-bin shift for canonical coverage classification.
pub const GPU_PSO_COVERAGE_SHIFT: u32 = 2;

/// PSO-bin shift for raster sidedness.
pub const GPU_PSO_SIDEDNESS_SHIFT: u32 = 4;

/// PSO-bin shift for the canonical material surface model.
pub const GPU_PSO_SURFACE_MODEL_SHIFT: u32 = 5;

/// PSO-bin shift for transparency compositing.
pub const GPU_PSO_TRANSPARENCY_SHIFT: u32 = 6;

/// PSO-bin shift for deformation provider.
pub const GPU_PSO_DEFORMATION_SHIFT: u32 = 8;

/// PSO-bin shift for pass class.
pub const GPU_PSO_PASS_SHIFT: u32 = 9;

/// Material-class shift for canonical coverage.
pub const GPU_MATERIAL_COVERAGE_SHIFT: u32 = 0;

/// Material-class shift for sidedness.
pub const GPU_MATERIAL_SIDEDNESS_SHIFT: u32 = 2;

/// Material-class shift for the surface model.
pub const GPU_MATERIAL_SURFACE_MODEL_SHIFT: u32 = 3;

/// Material-class shift for transparency.
pub const GPU_MATERIAL_TRANSPARENCY_SHIFT: u32 = 4;

/// Unlit shading bit within [`GpuMaterialClass`].
pub const GPU_MATERIAL_UNLIT_SHIFT: u32 = 5;

fn live_frame_mask() -> u64 {
    (1_u64 << MAX_FRAMES_IN_FLIGHT) - 1
}

fn frame_slot_bit(frame_slot: usize) -> Result<u64> {
    if frame_slot >= MAX_FRAMES_IN_FLIGHT || frame_slot >= u64::BITS as usize {
        return Err(Error::InvalidUploadData(format!(
            "frame slot {frame_slot} exceeds the {MAX_FRAMES_IN_FLIGHT}-slot GPU lifetime ring"
        )));
    }
    Ok(1_u64 << frame_slot)
}

fn align_up(value: u64, alignment: u64, subject: &'static str) -> Result<u64> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(Error::InvalidUploadData(format!(
            "{subject} alignment must be a nonzero power of two"
        )));
    }
    value
        .checked_add(alignment - 1)
        .map(|aligned| aligned & !(alignment - 1))
        .ok_or_else(|| Error::InvalidUploadData(format!("{subject} alignment overflow")))
}

fn greatest_common_divisor(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn physical_range_layout(
    logical_count: u32,
    element_stride: u64,
    requested_alignment: u32,
) -> Result<(u32, u32)> {
    if element_stride == 0 {
        return Err(Error::InvalidUploadData(
            "global GPU arena stride must be nonzero".to_owned(),
        ));
    }
    if requested_alignment == 0 || !requested_alignment.is_power_of_two() {
        return Err(Error::InvalidUploadData(
            "global GPU range alignment must be a nonzero power of two".to_owned(),
        ));
    }
    let copy_elements =
        GPU_COPY_ALIGNMENT / greatest_common_divisor(element_stride, GPU_COPY_ALIGNMENT);
    let reserved = align_up(
        u64::from(logical_count),
        copy_elements,
        "global GPU physical range",
    )?;
    let reserved = u32::try_from(reserved).map_err(|_| {
        Error::InvalidUploadData("global GPU physical range exceeds u32 elements".to_owned())
    })?;
    let copy_alignment = u32::try_from(copy_elements).map_err(|_| {
        Error::InvalidUploadData("global GPU copy alignment exceeds u32 elements".to_owned())
    })?;
    Ok((reserved, requested_alignment.max(copy_alignment)))
}

fn next_capacity(current: u64, needed: u64, floor: u64) -> Result<u64> {
    let mut capacity = current.max(floor);
    while capacity < needed {
        capacity = capacity.checked_mul(2).ok_or_else(|| {
            Error::InvalidUploadData("global GPU arena capacity overflow".to_owned())
        })?;
    }
    Ok(capacity)
}

fn arena_byte_offset(first: u32, element_stride: u64) -> Result<u64> {
    u64::from(first)
        .checked_mul(element_stride)
        .ok_or_else(|| Error::InvalidUploadData("global GPU byte offset overflow".to_owned()))
}

#[cfg(test)]
mod tests;
