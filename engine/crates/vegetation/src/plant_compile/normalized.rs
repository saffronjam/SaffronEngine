//! The normalized plant-family payload and its pinned canonical encoding.

use saffron_core::Uuid;

use crate::canonical::CanonicalSink;
use crate::{
    Error, PlantDimensions, PlantSourceRole, PlantSourceSelector, PlantTagId, Result,
    VegetationContentHasher,
};

use super::*;

/// Quantized vertex emitted by deterministic source normalization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NormalizedPlantVertex {
    /// Q15.16 family-local metres.
    pub position_bits: [i32; 3],
    /// Signed-normalized object-space normal.
    pub normal_snorm: [i16; 3],
    /// Q15.16 canonical UV0.
    pub uv_bits: [i32; 2],
    /// Signed-normalized tangent xyz plus ±32767 handedness.
    pub tangent_snorm: [i16; 4],
}

/// Quantized four-influence skin record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NormalizedPlantSkin {
    /// Structural joint indices.
    pub joints: [u16; 4],
    /// Normalized weights summing exactly to 65535 when nonzero.
    pub weights: [u16; 4],
}

/// One normalized material-homogeneous submesh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NormalizedPlantSubmesh {
    /// First index in the normalized mesh index stream.
    pub first_index: u32,
    /// Index count, always a multiple of three.
    pub index_count: u32,
    pub material_slot: u32,
}

/// One deterministic source mesh after full coordinate and attribute normalization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedPlantMesh {
    /// Recipe source identity.
    pub source: u128,
    /// Semantic contribution of the source.
    pub role: PlantSourceRole,
    /// Exact selected source element or submesh.
    pub selector: PlantSourceSelector,
    pub vertices: Vec<NormalizedPlantVertex>,
    /// Canonical counter-clockwise triangle indices.
    pub indices: Vec<u32>,
    /// Material-homogeneous draw ranges.
    pub submeshes: Vec<NormalizedPlantSubmesh>,
    /// Optional quantized structural weights.
    pub skin: Vec<NormalizedPlantSkin>,
}

/// One normalized source joint rest transform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedPlantJoint {
    /// Recipe source identity.
    pub source: u128,
    /// Stable joint selector.
    pub selector: PlantSourceSelector,
    /// Stable parent selector.
    pub parent: Option<PlantSourceSelector>,
    /// Q15.16 row-major transform components.
    pub transform_bits: [i32; 16],
}

/// Resolved material identity retained by the compiled family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedPlantMaterial {
    /// Catalog material identity.
    pub material: Uuid,
    /// Complete resolved material dependency hash.
    pub content_hash: [u8; 32],
}

/// Complete deterministic plant-family payload consumed by the artifact writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedPlantFamily {
    /// Authored family identity.
    pub family: Uuid,
    /// Canonically sorted unique family classification identities.
    pub tags: Vec<PlantTagId>,
    /// Exact accepted source dependencies.
    pub sources: Vec<(u128, [u8; 32])>,
    /// Canonical geometry/collision/navigation meshes.
    pub meshes: Vec<NormalizedPlantMesh>,
    pub joints: Vec<NormalizedPlantJoint>,
    pub materials: Vec<NormalizedPlantMaterial>,
    /// Validated authored family bounds and footprints.
    pub dimensions: PlantDimensions,
}

impl NormalizedPlantFamily {
    /// SHA-256 over one pinned canonical encoding of the normalized family.
    pub fn canonical_hash(&self) -> Result<[u8; 32]> {
        let mut sink = VegetationContentHasher::new();
        sink.write(b"saffron-anima/normalized-plant/v2\0")?;
        sink.write(&self.family.value().to_be_bytes())?;
        write_len(&mut sink, self.tags.len())?;
        for tag in &self.tags {
            sink.write(&tag.value().to_be_bytes())?;
        }
        write_len(&mut sink, self.sources.len())?;
        for (source, hash) in &self.sources {
            sink.write(&source.to_be_bytes())?;
            sink.write(hash)?;
        }
        write_len(&mut sink, self.meshes.len())?;
        for mesh in &self.meshes {
            write_normalized_mesh(&mut sink, mesh)?;
        }
        write_len(&mut sink, self.joints.len())?;
        for joint in &self.joints {
            sink.write(&joint.source.to_be_bytes())?;
            write_selector(&mut sink, &joint.selector)?;
            match &joint.parent {
                Some(parent) => {
                    sink.write_byte(1)?;
                    write_selector(&mut sink, parent)?;
                }
                None => sink.write_byte(0)?,
            }
            for component in joint.transform_bits {
                sink.write(&component.to_be_bytes())?;
            }
        }
        write_len(&mut sink, self.materials.len())?;
        for material in &self.materials {
            sink.write(&material.material.value().to_be_bytes())?;
            sink.write(&material.content_hash)?;
        }
        write_dimensions(&mut sink, self.dimensions)?;
        sink.finalize()
    }
}

/// Work and output counts retained for cooker inspection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlantCompileStatistics {
    pub sources: u64,
    pub meshes: u64,
    pub vertices: u64,
    pub indices: u64,
    pub joints: u64,
    pub materials: u64,
    pub rejected: u64,
    /// Hero meshes grafted over generated elements.
    pub grafts: u64,
}

/// Shared result used by validation and recook; only the asset layer decides whether to publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantCompileOutput {
    /// Normalized family, present only when publication is valid.
    pub family: Option<NormalizedPlantFamily>,
    /// SHA-256 of `family` when present.
    pub family_hash: Option<[u8; 32]>,
    /// Sorted diagnostics from the one compile path.
    pub diagnostics: Vec<PlantCompileDiagnostic>,
    /// Manual semantic targets that forbid publication.
    pub conflicts: PlantReimportConflictReport,
    /// Source hashes an accepted recook writes back into `.splant`.
    pub source_updates: Vec<PlantSourceHashUpdate>,
    /// Deterministic work/output counts.
    pub statistics: PlantCompileStatistics,
}

impl PlantCompileOutput {
    /// Whether the result is valid for atomic artifact publication.
    #[must_use]
    pub fn publishable(&self) -> bool {
        self.family.is_some()
            && self.conflicts.conflicts.is_empty()
            && !self
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == PlantCompileDiagnosticSeverity::Error)
    }
}

pub(super) fn role_tag(role: PlantSourceRole) -> u8 {
    match role {
        PlantSourceRole::Geometry => 0,
        PlantSourceRole::Material => 1,
        PlantSourceRole::Skeleton => 2,
        PlantSourceRole::Collision => 3,
        PlantSourceRole::Navigation => 4,
    }
}

fn write_len(sink: &mut impl CanonicalSink, length: usize) -> Result<()> {
    sink.write(
        &u64::try_from(length)
            .map_err(|_| Error::NumericOverflow)?
            .to_be_bytes(),
    )
}

fn write_selector(sink: &mut impl CanonicalSink, selector: &PlantSourceSelector) -> Result<()> {
    match selector {
        PlantSourceSelector::Whole => sink.write_byte(0),
        PlantSourceSelector::Element { id, path } => {
            sink.write_byte(1)?;
            sink.write(&id.to_be_bytes())?;
            write_len(sink, path.len())?;
            sink.write(path.as_bytes())
        }
        PlantSourceSelector::Submesh { element, index } => {
            sink.write_byte(2)?;
            sink.write(&element.to_be_bytes())?;
            sink.write(&index.to_be_bytes())
        }
    }
}

fn write_normalized_mesh(sink: &mut impl CanonicalSink, mesh: &NormalizedPlantMesh) -> Result<()> {
    sink.write(&mesh.source.to_be_bytes())?;
    sink.write_byte(role_tag(mesh.role))?;
    write_selector(sink, &mesh.selector)?;
    write_len(sink, mesh.vertices.len())?;
    for vertex in &mesh.vertices {
        for value in vertex.position_bits {
            sink.write(&value.to_be_bytes())?;
        }
        for value in vertex.normal_snorm {
            sink.write(&value.to_be_bytes())?;
        }
        for value in vertex.uv_bits {
            sink.write(&value.to_be_bytes())?;
        }
        for value in vertex.tangent_snorm {
            sink.write(&value.to_be_bytes())?;
        }
    }
    write_len(sink, mesh.indices.len())?;
    for index in &mesh.indices {
        sink.write(&index.to_be_bytes())?;
    }
    write_len(sink, mesh.submeshes.len())?;
    for submesh in &mesh.submeshes {
        sink.write(&submesh.first_index.to_be_bytes())?;
        sink.write(&submesh.index_count.to_be_bytes())?;
        sink.write(&submesh.material_slot.to_be_bytes())?;
    }
    write_len(sink, mesh.skin.len())?;
    for skin in &mesh.skin {
        for joint in skin.joints {
            sink.write(&joint.to_be_bytes())?;
        }
        for weight in skin.weights {
            sink.write(&weight.to_be_bytes())?;
        }
    }
    Ok(())
}

fn write_dimensions(sink: &mut impl CanonicalSink, dimensions: PlantDimensions) -> Result<()> {
    sink.write(&dimensions.height.bits().to_be_bytes())?;
    sink.write(&dimensions.trunk_radius.bits().to_be_bytes())?;
    for value in dimensions.crown_radius {
        sink.write(&value.bits().to_be_bytes())?;
    }
    for value in dimensions.root_radius {
        sink.write(&value.bits().to_be_bytes())?;
    }
    for value in dimensions.local_bounds_min {
        sink.write(&value.bits().to_be_bytes())?;
    }
    for value in dimensions.local_bounds_max {
        sink.write(&value.bits().to_be_bytes())?;
    }
    Ok(())
}
