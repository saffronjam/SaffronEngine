//! The format-neutral value vocabulary the hierarchy cooker consumes and produces.

use std::collections::BTreeSet;

use saffron_material::{AlphaClassification, MaterialSurface, VoxelMaterialMoments};
use sha2::{Digest, Sha256};

use crate::{Error, Mesh, Result, VertexSkin};

use super::support::{bounds_for_positions, fixed_bits, format_error, snorm16, u32_len};

/// Portable upper bound shared by mesh and indexed-compute execution paths.
pub const PORTABLE_CLUSTER_MAX_VERTICES: usize = 64;
/// Portable primitive bound shared by mesh and indexed-compute execution paths.
pub const PORTABLE_CLUSTER_MAX_TRIANGLES: usize = 124;
/// Canonical aggregate brick edge in voxels.
pub const PORTABLE_VOXEL_BRICK_EDGE: u8 = 8;
/// Largest child set one hierarchy node may carry.
///
/// The GPU cut walk keeps a fixed-size per-thread page stack, so a node's whole child set
/// has to fit beside the path already pushed. Four keeps a million-cluster prototype inside
/// that stack and gives the cut real intermediate levels to stop at, instead of one coarse
/// stand-in for the entire prototype.
pub const PORTABLE_HIERARCHY_MAX_CHILDREN: usize = 4;
/// Canonical envelope version for the five portable hierarchy sections.
pub const PORTABLE_HIERARCHY_FORMAT_VERSION: u32 = 5;

/// Format-neutral aggregate material moments stored as canonical integer bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortableMaterialMoments {
    /// Occupancy/coverage density in unsigned normalized 16-bit form.
    pub occupancy: u16,
    /// Mean albedo RGB in Q15.16.
    pub albedo_mean: [i32; 3],
    /// Mean roughness in unsigned normalized 16-bit form.
    pub roughness_mean: u16,
    /// Mean transmitted energy RGB in Q15.16.
    pub transmission_mean: [i32; 3],
    /// Mean thickness in Q15.16 metres.
    pub thickness_mean: i32,
    /// Normal second moments in canonical XX/YY/ZZ/XY/XZ/YZ Q15.16 order.
    pub normal_second_moments: [i32; 6],
}

/// Quantized source vertex consumed by the one portable hierarchy cooker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortableSourceVertex {
    /// Q15.16 local-space position.
    pub position_bits: [i32; 3],
    /// Signed-normalized object-space normal.
    pub normal_snorm: [i16; 3],
    /// Q15.16 UV0.
    pub uv_bits: [i32; 2],
    /// Signed-normalized tangent xyz plus handedness.
    pub tangent_snorm: [i16; 4],
}

/// Quantized source skin record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortableSourceSkin {
    /// Joint indices.
    pub joints: [u16; 4],
    /// Unsigned normalized weights.
    pub weights: [u16; 4],
}

/// Whether disconnected fine geometry should aggregate to voxels early.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PortableAggregationMode {
    /// Simplify to coarser triangle clusters.
    #[default]
    Contiguous,
    /// Aggregate disconnected sheets or fragments to voxels.
    Disconnected,
}

/// One material-homogeneous source range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortableSourceSubmesh {
    /// First index in the source stream.
    pub first_index: u32,
    /// Triangle index count.
    pub index_count: u32,
    /// Complete material contribution for this range.
    pub material: VirtualHierarchyMaterial,
}

/// One format-neutral source mesh.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortableSourceMesh {
    /// Stable source identity.
    pub source: u128,
    /// Stable selector/content identity supplied by the adapter.
    pub selector_hash: [u8; 32],
    /// Quantized vertices.
    pub vertices: Vec<PortableSourceVertex>,
    /// Canonical triangle indices.
    pub indices: Vec<u32>,
    /// Material-homogeneous ranges.
    pub submeshes: Vec<PortableSourceSubmesh>,
    /// Optional quantized joint weights.
    pub skin: Vec<PortableSourceSkin>,
    /// Coarse representation policy derived from content semantics.
    pub aggregation: PortableAggregationMode,
}

/// Adapter-owned deformation kind retained as canonical bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortableDeformationKind(pub u8);

/// Stable render classification retained by clusters, voxels, PSO bins, and RT metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum VirtualMaterialClass {
    /// Conventional fully covered material.
    #[default]
    Opaque = 0,
    /// Conventional canonical-coverage material.
    Masked = 1,
    /// Conventional partial-transmission material.
    Transmissive = 2,
    /// Energy-conserving two-sided thin-sheet foliage.
    ThinSheet = 3,
}

impl VirtualMaterialClass {
    /// Whether a ray may commit a hit without consulting the coverage classifier. Only a fully
    /// covered surface qualifies: masked and thin-sheet surfaces carry cut-out coverage and a
    /// transmissive one attenuates, so all three must surface candidates for the classifier.
    #[must_use]
    pub const fn is_opaque(self) -> bool {
        matches!(self, Self::Opaque)
    }

    pub(crate) fn from_tag(tag: u8, format: &'static str) -> Result<Self> {
        match tag {
            0 => Ok(Self::Opaque),
            1 => Ok(Self::Masked),
            2 => Ok(Self::Transmissive),
            3 => Ok(Self::ThinSheet),
            _ => Err(format_error(format, "materialClass")),
        }
    }
}

/// Complete material contribution needed by hierarchy, aggregate, and RT derivation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VirtualHierarchyMaterial {
    /// Family material slot.
    pub slot: u32,
    /// Shared surface/coverage classification.
    pub class: VirtualMaterialClass,
    /// Aggregate moments sampled by voxel representations.
    pub moments: PortableMaterialMoments,
    /// Optional opacity-micromap derivation is permitted for RT acceleration.
    pub opacity_micromap: bool,
}

impl VirtualHierarchyMaterial {
    /// Creates a conventional fully covered material contribution.
    #[must_use]
    pub const fn opaque(slot: u32) -> Self {
        Self {
            slot,
            class: VirtualMaterialClass::Opaque,
            moments: standard_moments(),
            opacity_micromap: false,
        }
    }

    /// Adapts one canonical material surface to the hierarchy representation.
    #[must_use]
    pub fn from_surface(slot: u32, surface: &MaterialSurface, alpha: AlphaClassification) -> Self {
        match surface {
            MaterialSurface::Standard => Self {
                slot,
                class: match alpha {
                    AlphaClassification::Opaque => VirtualMaterialClass::Opaque,
                    AlphaClassification::Masked => VirtualMaterialClass::Masked,
                    AlphaClassification::Transmissive => VirtualMaterialClass::Transmissive,
                },
                moments: standard_moments(),
                // Only a masked surface qualifies: its coverage collapses to a step at a constant
                // cutoff, so a micro-triangle whose whole footprint sits on one side is provable.
                // Opaque has no coverage to refine and transmissive attenuates rather than cuts.
                opacity_micromap: matches!(alpha, AlphaClassification::Masked),
            },
            MaterialSurface::ThinSheetFoliage(parameters) => Self {
                slot,
                class: VirtualMaterialClass::ThinSheet,
                moments: portable_material_moments(parameters.voxel_moments),
                opacity_micromap: parameters.opacity_micromap.enabled,
            },
        }
    }
}

fn portable_material_moments(moments: VoxelMaterialMoments) -> PortableMaterialMoments {
    PortableMaterialMoments {
        occupancy: moments.occupancy.bits(),
        albedo_mean: moments.albedo_mean.map(|value| value.bits()),
        roughness_mean: moments.roughness_mean.bits(),
        transmission_mean: moments.transmission_mean.map(|value| value.bits()),
        thickness_mean: moments.thickness_mean.bits(),
        normal_second_moments: moments.normal_second_moments.map(|value| value.bits()),
    }
}

/// Aggregates material contributions for a coarsest hierarchy representation.
#[must_use]
pub fn aggregate_virtual_hierarchy_materials(
    materials: &[VirtualHierarchyMaterial],
) -> VirtualHierarchyMaterial {
    if materials.is_empty() {
        return VirtualHierarchyMaterial::opaque(0);
    }
    let count = materials.len() as i128;
    VirtualHierarchyMaterial {
        slot: 0,
        class: materials
            .iter()
            .map(|material| material.class)
            .max()
            .unwrap_or_default(),
        moments: PortableMaterialMoments {
            occupancy: average_u16(
                materials.iter().map(|material| material.moments.occupancy),
                materials.len() as u128,
            ),
            albedo_mean: std::array::from_fn(|axis| {
                average_i32(
                    materials
                        .iter()
                        .map(|material| material.moments.albedo_mean[axis]),
                    count,
                )
            }),
            roughness_mean: average_u16(
                materials
                    .iter()
                    .map(|material| material.moments.roughness_mean),
                materials.len() as u128,
            ),
            transmission_mean: std::array::from_fn(|axis| {
                average_i32(
                    materials
                        .iter()
                        .map(|material| material.moments.transmission_mean[axis]),
                    count,
                )
            }),
            thickness_mean: average_i32(
                materials
                    .iter()
                    .map(|material| material.moments.thickness_mean),
                count,
            ),
            normal_second_moments: std::array::from_fn(|axis| {
                average_i32(
                    materials
                        .iter()
                        .map(|material| material.moments.normal_second_moments[axis]),
                    count,
                )
            }),
        },
        opacity_micromap: materials.iter().any(|material| material.opacity_micromap),
    }
}

fn average_i32(values: impl Iterator<Item = i32>, count: i128) -> i32 {
    let sum = values.map(i128::from).sum::<i128>();
    i32::try_from(sum / count).unwrap_or_else(|_| {
        if sum.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        }
    })
}

fn average_u16(values: impl Iterator<Item = u16>, count: u128) -> u16 {
    let sum = values.map(u128::from).sum::<u128>();
    u16::try_from(sum / count).unwrap_or(u16::MAX)
}

/// Quantized cluster-local vertex usable by mesh shaders or compute expansion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortableClusterVertex {
    /// Cluster-AABB-relative unsigned normalized position.
    pub position_unorm: [u16; 3],
    /// Octahedrally encoded normal.
    pub normal_oct: [i16; 2],
    /// Octahedrally encoded tangent.
    pub tangent_oct: [i16; 2],
    /// Tangent-frame handedness.
    pub tangent_handedness: i8,
    /// Canonical Q15.16 UV0.
    pub uv_bits: [i32; 2],
}

/// Appearance-aware error used for triangle, voxel, and representation transitions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppearanceError {
    /// Geometry/silhouette error in Q15.16 local metres.
    pub silhouette: u32,
    /// Projected coverage-density error.
    pub coverage: u32,
    /// Transmitted-energy error.
    pub transmission: u32,
    /// Albedo/roughness variation error.
    pub material: u32,
    /// Normal-distribution error.
    pub normal_distribution: u32,
    /// Saturating sum used by cut selection.
    pub total: u32,
}

impl AppearanceError {
    pub(crate) fn new(
        silhouette: u32,
        coverage: u32,
        transmission: u32,
        material: u32,
        normal_distribution: u32,
    ) -> Self {
        Self {
            silhouette,
            coverage,
            transmission,
            material,
            normal_distribution,
            total: silhouette
                .saturating_add(coverage)
                .saturating_add(transmission)
                .saturating_add(material)
                .saturating_add(normal_distribution),
        }
    }

    pub(crate) fn max(self, other: Self) -> Self {
        Self::new(
            self.silhouette.max(other.silhouette),
            self.coverage.max(other.coverage),
            self.transmission.max(other.transmission),
            self.material.max(other.material),
            self.normal_distribution.max(other.normal_distribution),
        )
    }
}

/// Conservative source-local bounds in Q15.16 metres.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortableBounds {
    /// Inclusive minimum.
    pub min_bits: [i32; 3],
    /// Inclusive maximum.
    pub max_bits: [i32; 3],
}

impl PortableBounds {
    pub(crate) fn union(self, other: Self) -> Self {
        Self {
            min_bits: std::array::from_fn(|axis| self.min_bits[axis].min(other.min_bits[axis])),
            max_bits: std::array::from_fn(|axis| self.max_bits[axis].max(other.max_bits[axis])),
        }
    }

    pub(crate) fn expanded(self, padding: i32) -> Self {
        Self {
            min_bits: self.min_bits.map(|value| value.saturating_sub(padding)),
            max_bits: self.max_bits.map(|value| value.saturating_add(padding)),
        }
    }
}

/// One optimized portable triangle cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableTriangleCluster {
    /// Stable cluster index.
    pub id: u32,
    /// Shared source-geometry prototype.
    pub prototype: u32,
    /// Material-homogeneous source range within the prototype.
    pub source_submesh: u32,
    /// Family material slot.
    pub material_slot: u32,
    /// Shared material classification.
    pub material_class: VirtualMaterialClass,
    /// Material moments used by representation-transition evaluation and aggregate shading.
    pub material_moments: PortableMaterialMoments,
    /// Optional OMM construction is permitted for derived RT acceleration.
    pub opacity_micromap: bool,
    /// Cluster-local quantized vertices.
    pub vertices: Vec<PortableClusterVertex>,
    /// Prototype vertex indices parallel to `vertices` for indexed execution paths.
    pub source_vertices: Vec<u32>,
    /// Compact local triangle corners.
    pub local_indices: Vec<u8>,
    /// Exact source-vertex bounds.
    pub bounds: PortableBounds,
    /// Conservative structural-deformation bounds.
    pub deformed_bounds: PortableBounds,
    /// Bounding sphere center and radius in Q15.16.
    pub sphere_bits: [i32; 4],
    /// Quantized cone axis and conservative cutoff.
    pub cone: [i8; 4],
    /// Structural joint indices influencing this cluster.
    pub deformation_joints: Vec<u16>,
    /// Parent-before-child content page.
    pub page: u32,
    /// Error of this drawable representation.
    pub appearance_error: AppearanceError,
    /// Parent representation's error.
    pub parent_appearance_error: AppearanceError,
}

/// Shared normalized source mesh retained once for repeated semantic assembly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeometryPrototype {
    /// Stable prototype index.
    pub id: u32,
    /// Source identity.
    pub source: u128,
    /// Canonical selector identity hash.
    pub selector_hash: [u8; 32],
    /// Source vertex count used to validate indexed execution payloads.
    pub vertex_count: u32,
    /// Source material-homogeneous range count.
    pub submesh_count: u32,
    /// First cluster owned by the prototype.
    pub first_cluster: u32,
    /// Number of fine clusters.
    pub cluster_count: u32,
    /// Source bounds.
    pub bounds: PortableBounds,
}

/// One semantic-part use of a shared prototype.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MicroInstance {
    /// Authored semantic part identity.
    pub part: u128,
    /// Shared prototype.
    pub prototype: u32,
    /// Q15.16 row-major local transform.
    pub transform_bits: [i32; 16],
}

/// One (variation, phenotype) combination's active-use mask: bit `u` of word `u / 32` covers
/// micro-instance use `u`, and the word count always equals `ceil(uses / 32)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableUseCombination {
    /// Family variation identity.
    pub variation: u32,
    /// Family phenotype identity.
    pub phenotype: u32,
    /// Packed active-use bits, low bit of word 0 = use 0.
    pub active_words: Vec<u32>,
}

/// Portable indexed aggregate voxel vertex.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortableVoxelVertex {
    /// Family-local Q15.16 position.
    pub position_bits: [i32; 3],
    /// Octahedrally encoded outward normal.
    pub normal_oct: [i16; 2],
}

/// One aggregate voxel brick plus a portable indexed-surface fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableVoxelBrick {
    /// Stable brick index.
    pub id: u32,
    /// Canonical dimensions.
    pub dimensions: [u8; 3],
    /// Brick bounds.
    pub bounds: PortableBounds,
    /// Conservative structural-deformation bounds.
    pub deformed_bounds: PortableBounds,
    /// Bit-packed occupancy, X-major then Y then Z.
    pub occupancy: Vec<u8>,
    /// Aggregate material and normal moments.
    pub moments: PortableMaterialMoments,
    /// Shared material classification used by raster and RT expansion.
    pub material_class: VirtualMaterialClass,
    /// Optional OMM construction is permitted for derived RT acceleration.
    pub opacity_micromap: bool,
    /// Portable indexed surface vertices.
    pub vertices: Vec<PortableVoxelVertex>,
    /// Portable indexed surface triangles.
    pub indices: Vec<u32>,
    /// Parent-before-child content page.
    pub page: u32,
    /// Triangle-to-voxel appearance error.
    pub appearance_error: AppearanceError,
}

/// Drawable representation owned by one hierarchy node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HierarchyRepresentation {
    /// One or more optimized triangle clusters.
    Triangles { first: u32, count: u32 },
    /// One portable aggregate voxel brick.
    Voxel { brick: u32 },
}

/// One parent/child hierarchy node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableHierarchyNode {
    /// Stable node index.
    pub id: u32,
    /// Independently drawable payload.
    pub representation: HierarchyRepresentation,
    /// Parent node; roots have none.
    pub parent: Option<u32>,
    /// Complete child set needed for hole-free refinement.
    pub children: Vec<u32>,
    /// Parent-before-child content page.
    pub page: u32,
    /// Exact static bounds.
    pub bounds: PortableBounds,
    /// Conservative deformed bounds.
    pub deformed_bounds: PortableBounds,
    /// Appearance-aware cut error.
    pub appearance_error: AppearanceError,
}

/// One structural deformation region and its swept bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableDeformationRegion {
    /// Semantic part identity.
    pub part: u128,
    /// Structural role.
    pub semantic: PortableDeformationKind,
    /// Referenced spine/joint identities.
    pub influences: Vec<u128>,
    /// Static source-local bounds.
    pub static_bounds: PortableBounds,
    /// Conservative swept bounds.
    pub swept_bounds: PortableBounds,
}

/// One immutable content page and its parent dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableHierarchyPage {
    /// Stable page number. Parents always have smaller numbers.
    pub id: u32,
    /// Parent page required before this page becomes visible.
    pub dependency: Option<u32>,
    /// Drawable hierarchy node stored by this page.
    pub node: u32,
    /// Static payload bounds.
    pub bounds: PortableBounds,
    /// Swept payload bounds.
    pub deformed_bounds: PortableBounds,
    /// Appearance error this page removes: the error of the representation drawn in its
    /// place, which is its parent node's. Streaming demand is worth exactly that.
    pub transition_error: AppearanceError,
    /// Root pages remain drawable under every pressure condition.
    pub guaranteed_root: bool,
}

/// RT derivation for one triangle cluster or aggregate brick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortableRayTracingRecord {
    /// Hierarchy node owning the geometry.
    pub node: u32,
    /// Shared material classification.
    pub material_class: VirtualMaterialClass,
    /// Optional OMM construction is permitted.
    pub opacity_micromap: bool,
    /// Any-hit coverage remains required when OMM is unavailable.
    pub requires_any_hit: bool,
}

/// Complete final hierarchy cook split across five independently addressable portable sections.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortableVirtualHierarchy {
    /// Per-(variation, phenotype) active-use masks (empty for a plain mesh).
    pub combinations: Vec<PortableUseCombination>,
    /// Shared source prototypes.
    pub prototypes: Vec<GeometryPrototype>,
    /// Repeated semantic-part assembly uses.
    pub micro_instances: Vec<MicroInstance>,
    /// Optimized triangle payloads.
    pub triangle_clusters: Vec<PortableTriangleCluster>,
    /// Aggregate voxel payloads.
    pub voxel_bricks: Vec<PortableVoxelBrick>,
    /// Mixed triangle/voxel cut hierarchy.
    pub nodes: Vec<PortableHierarchyNode>,
    /// Structural deformation regions.
    pub deformation: Vec<PortableDeformationRegion>,
    /// Parent-before-child page directory.
    pub pages: Vec<PortableHierarchyPage>,
    /// Guaranteed drawable root nodes.
    pub roots: Vec<u32>,
    /// RT/coverage derivation metadata.
    pub ray_tracing: Vec<PortableRayTracingRecord>,
    /// Derived opacity micromaps, keyed by the flattened submesh whose BLAS geometry they refine.
    /// A micromap may only ever remove classifier work, so an empty list is always correct.
    pub opacity_micromaps: Vec<PortableOpacityMicromap>,
}

/// One submesh's derived opacity micromap, in the exact shape `vkCmdBuildMicromapsEXT` reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortableOpacityMicromap {
    /// The flattened-mesh submesh whose BLAS geometry this refines.
    pub submesh: u32,
    /// One index per geometry triangle; negatives are the format's uniform-triangle specials.
    pub indices: Vec<i32>,
    /// `(data_offset, subdivision_level, format)` per referenced block.
    pub blocks: Vec<(u32, u16, u16)>,
    /// Packed two-bit micro-triangle states.
    pub data: Vec<u8>,
    /// `(count, subdivision_level, format)` usage rows.
    pub usage: Vec<(u32, u32, u32)>,
    /// Micro-triangles proven opaque, proven transparent, and left unknown.
    pub classes: (u64, u64, u64),
}

/// Complete format-neutral input to the sole portable hierarchy cooker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortableHierarchyInput {
    /// Source meshes, each retained once as a geometry prototype.
    pub meshes: Vec<PortableSourceMesh>,
    /// Semantic or ordinary uses of the source prototypes.
    pub micro_instances: Vec<MicroInstance>,
    /// Per-(variation, phenotype) active-use masks (empty for a plain mesh).
    pub combinations: Vec<PortableUseCombination>,
    /// Adapter-owned deformation regions.
    pub deformation: Vec<PortableDeformationRegion>,
    /// Complete local-space bounds.
    pub bounds: PortableBounds,
    /// Coarsest aggregate material.
    pub root_material: VirtualHierarchyMaterial,
    /// Conservative Q15.16 deformation padding.
    pub deformation_padding: i32,
}

impl PortableHierarchyInput {
    /// Adapts the ordinary engine [`Mesh`] vocabulary to the canonical cooker input.
    pub fn from_mesh(mesh: &Mesh, skin: &[VertexSkin]) -> Result<Self> {
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            return Err(format_error("portable hierarchy input", "mesh"));
        }
        if !skin.is_empty() && skin.len() != mesh.vertices.len() {
            return Err(format_error("portable hierarchy input", "skin"));
        }
        if mesh.vertices.iter().any(|vertex| {
            !vertex.position.is_finite()
                || !vertex.normal.is_finite()
                || !vertex.uv0.is_finite()
                || vertex.tangent.iter().any(|value| !value.is_finite())
        }) {
            return Err(format_error("portable hierarchy input", "vertex.finite"));
        }
        let vertices = mesh
            .vertices
            .iter()
            .map(|vertex| PortableSourceVertex {
                position_bits: vertex.position.to_array().map(fixed_bits),
                normal_snorm: vertex.normal.to_array().map(snorm16),
                uv_bits: vertex.uv0.to_array().map(fixed_bits),
                tangent_snorm: [
                    snorm16(vertex.tangent[0]),
                    snorm16(vertex.tangent[1]),
                    snorm16(vertex.tangent[2]),
                    if vertex.tangent[3] < 0.0 {
                        -32_767
                    } else {
                        32_767
                    },
                ],
            })
            .collect::<Vec<_>>();
        let skin = skin.iter().copied().map(quantize_skin).collect::<Vec<_>>();
        let submeshes = if mesh.submeshes.is_empty() {
            vec![PortableSourceSubmesh {
                first_index: 0,
                index_count: u32_len(mesh.indices.len())?,
                material: VirtualHierarchyMaterial::opaque(0),
            }]
        } else {
            mesh.submeshes
                .iter()
                .map(|submesh| PortableSourceSubmesh {
                    first_index: submesh.first_index,
                    index_count: submesh.index_count,
                    material: VirtualHierarchyMaterial::opaque(submesh.material_slot),
                })
                .collect::<Vec<_>>()
        };
        let indices = adapt_mesh_indices(mesh)?;
        let bounds = bounds_for_positions(&vertices)?;
        let selector_hash = mesh_selector_hash(mesh, &skin);
        Ok(Self {
            combinations: Vec::new(),
            meshes: vec![PortableSourceMesh {
                source: 0,
                selector_hash,
                vertices,
                indices,
                submeshes,
                skin,
                aggregation: PortableAggregationMode::Contiguous,
            }],
            micro_instances: vec![MicroInstance {
                part: 0,
                prototype: 0,
                transform_bits: identity_transform_bits(),
            }],
            deformation: Vec::new(),
            bounds,
            root_material: VirtualHierarchyMaterial::opaque(0),
            deformation_padding: 0,
        })
    }
}

fn quantize_skin(skin: VertexSkin) -> PortableSourceSkin {
    let mut weights = skin.weights.map(|weight| {
        if weight.is_finite() {
            weight.max(0.0)
        } else {
            0.0
        }
    });
    let total = weights.iter().sum::<f32>();
    if total <= f32::EPSILON {
        return PortableSourceSkin {
            joints: skin.joints,
            weights: [0; 4],
        };
    }
    for weight in &mut weights {
        *weight /= total;
    }
    let mut quantized = weights.map(|weight| (weight * f32::from(u16::MAX)).round() as u16);
    let sum = quantized
        .iter()
        .map(|weight| i32::from(*weight))
        .sum::<i32>();
    let largest = weights
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map_or(0, |(index, _)| index);
    quantized[largest] = i32::from(quantized[largest])
        .saturating_add(i32::from(u16::MAX) - sum)
        .clamp(0, i32::from(u16::MAX)) as u16;
    PortableSourceSkin {
        joints: skin.joints,
        weights: quantized,
    }
}

fn adapt_mesh_indices(mesh: &Mesh) -> Result<Vec<u32>> {
    if !mesh.indices.len().is_multiple_of(3) {
        return Err(format_error("portable hierarchy input", "indices"));
    }
    let mut indices = mesh.indices.clone();
    let mut visited = BTreeSet::new();
    for submesh in &mesh.submeshes {
        let begin = usize::try_from(submesh.first_index).map_err(|_| Error::NumericOverflow)?;
        let count = usize::try_from(submesh.index_count).map_err(|_| Error::NumericOverflow)?;
        let end = begin.checked_add(count).ok_or(Error::NumericOverflow)?;
        if !count.is_multiple_of(3) || end > indices.len() {
            return Err(format_error("portable hierarchy input", "submesh.indices"));
        }
        for (offset, index) in indices[begin..end].iter_mut().enumerate() {
            let position = begin + offset;
            if !visited.insert(position) {
                return Err(format_error("portable hierarchy input", "submesh.overlap"));
            }
            let adjusted = i64::from(*index) + i64::from(submesh.vertex_offset);
            *index = u32::try_from(adjusted)
                .ok()
                .filter(|index| (*index as usize) < mesh.vertices.len())
                .ok_or_else(|| format_error("portable hierarchy input", "vertexOffset"))?;
        }
    }
    if mesh.submeshes.is_empty()
        && indices
            .iter()
            .any(|index| *index as usize >= mesh.vertices.len())
    {
        return Err(format_error("portable hierarchy input", "index"));
    }
    Ok(indices)
}

fn mesh_selector_hash(mesh: &Mesh, skin: &[PortableSourceSkin]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"saffron-anima/portable-mesh-source/v1\0");
    hash.update((mesh.vertices.len() as u64).to_be_bytes());
    hash.update(bytemuck::cast_slice(&mesh.vertices));
    hash.update((mesh.indices.len() as u64).to_be_bytes());
    hash.update(bytemuck::cast_slice(&mesh.indices));
    hash.update((mesh.submeshes.len() as u64).to_be_bytes());
    hash.update(bytemuck::cast_slice(&mesh.submeshes));
    hash.update((skin.len() as u64).to_be_bytes());
    for vertex in skin {
        for joint in vertex.joints {
            hash.update(joint.to_be_bytes());
        }
        for weight in vertex.weights {
            hash.update(weight.to_be_bytes());
        }
    }
    hash.finalize().into()
}

pub(crate) const fn identity_transform_bits() -> [i32; 16] {
    [
        65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536,
    ]
}

pub(crate) const fn standard_moments() -> PortableMaterialMoments {
    PortableMaterialMoments {
        occupancy: u16::MAX,
        albedo_mean: [65_536; 3],
        roughness_mean: u16::MAX,
        transmission_mean: [0; 3],
        thickness_mean: 0,
        normal_second_moments: [21_845, 21_845, 21_846, 0, 0, 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_surface_conversion_is_owned_by_geometry() {
        let standard = VirtualHierarchyMaterial::from_surface(
            7,
            &MaterialSurface::Standard,
            AlphaClassification::Transmissive,
        );
        assert_eq!(standard.slot, 7);
        assert_eq!(standard.class, VirtualMaterialClass::Transmissive);
        assert_eq!(standard.moments, standard_moments());
        assert!(!standard.opacity_micromap);

        let mut parameters = saffron_material::ThinSheetFoliageParameters::default();
        parameters.opacity_micromap.enabled = true;
        let thin = VirtualHierarchyMaterial::from_surface(
            9,
            &MaterialSurface::ThinSheetFoliage(parameters.clone()),
            AlphaClassification::Opaque,
        );
        assert_eq!(thin.slot, 9);
        assert_eq!(thin.class, VirtualMaterialClass::ThinSheet);
        assert_eq!(
            thin.moments,
            portable_material_moments(parameters.voxel_moments)
        );
        assert!(thin.opacity_micromap);
    }
}
