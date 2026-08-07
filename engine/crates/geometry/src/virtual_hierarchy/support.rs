//! Small shared quantization and bounds helpers for the hierarchy cooker.

use std::collections::BTreeSet;

use crate::{Error, Result};

use super::types::{PortableBounds, PortableSourceMesh, PortableSourceVertex};

pub(crate) fn bounds_contains(outer: PortableBounds, inner: PortableBounds) -> bool {
    (0..3).all(|axis| {
        outer.min_bits[axis] <= inner.min_bits[axis] && outer.max_bits[axis] >= inner.max_bits[axis]
    })
}

pub(crate) fn fixed_bits(value: f32) -> i32 {
    (value.clamp(-32_768.0, 32_767.999_984_74) * 65_536.0).round() as i32
}

pub(crate) fn error_bits(value: f32) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        (value.min(65_535.0) * 65_536.0).round() as u32
    }
}

pub(crate) fn u32_len(length: usize) -> Result<u32> {
    u32::try_from(length).map_err(|_| Error::NumericOverflow)
}

pub(crate) fn format_error(format: &'static str, field: &str) -> Error {
    Error::HierarchyFormat {
        format,
        field: field.to_owned(),
    }
}

pub(crate) fn oct_encode(vector: [f32; 3]) -> [i16; 2] {
    let length = vector[0].abs() + vector[1].abs() + vector[2].abs();
    if length <= f32::EPSILON {
        return [0, 0];
    }
    let mut x = vector[0] / length;
    let mut y = vector[1] / length;
    let z = vector[2] / length;
    if z < 0.0 {
        let previous_x = x;
        x = (1.0 - y.abs()).copysign(previous_x);
        y = (1.0 - previous_x.abs()).copysign(y);
    }
    [snorm16(x), snorm16(y)]
}

pub(crate) fn snorm16(value: f32) -> i16 {
    (value.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

pub(crate) fn multiply_unit(first: u16, second: u16) -> u16 {
    ((u32::from(first) * u32::from(second) + u32::from(u16::MAX) / 2) / u32::from(u16::MAX)) as u16
}

pub(crate) fn bounds_for_positions(vertices: &[PortableSourceVertex]) -> Result<PortableBounds> {
    let first = vertices
        .first()
        .ok_or_else(|| format_error("portable hierarchy", "mesh.vertices"))?;
    let mut bounds = PortableBounds {
        min_bits: first.position_bits,
        max_bits: first.position_bits,
    };
    for vertex in &vertices[1..] {
        bounds = bounds.union(PortableBounds {
            min_bits: vertex.position_bits,
            max_bits: vertex.position_bits,
        });
    }
    Ok(bounds)
}

pub(crate) fn bounds_for_vertex_indices(
    mesh: &PortableSourceMesh,
    indices: &[u32],
) -> Result<PortableBounds> {
    let first = *indices
        .first()
        .ok_or_else(|| format_error("portable hierarchy", "bounds.indices"))?;
    let position = mesh
        .vertices
        .get(first as usize)
        .ok_or_else(|| format_error("portable hierarchy", "bounds.vertex"))?
        .position_bits;
    let mut bounds = PortableBounds {
        min_bits: position,
        max_bits: position,
    };
    for &index in &indices[1..] {
        let position = mesh
            .vertices
            .get(index as usize)
            .ok_or_else(|| format_error("portable hierarchy", "bounds.vertex"))?
            .position_bits;
        bounds = bounds.union(PortableBounds {
            min_bits: position,
            max_bits: position,
        });
    }
    Ok(bounds)
}

pub(crate) fn bounds_for_indexed(
    mesh: &PortableSourceMesh,
    indices: &[u32],
) -> Result<PortableBounds> {
    let unique = indices.iter().copied().collect::<BTreeSet<_>>();
    bounds_for_vertex_indices(mesh, &unique.into_iter().collect::<Vec<_>>())
}
