//! Format-erased source snapshots, selection, and part partitioning.

use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_geometry::glam::Mat4;
use saffron_geometry::{Mesh, VertexSkin};

use crate::{
    AlphaClassification, CoverageSource, MaterialSurface, PlantFamilyAsset,
    PlantManualSemanticTarget, PlantSemanticDestination, PlantSourceSelector,
};

/// Hard bounds applied before plant-source normalization allocates output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantCompileLimits {
    /// Maximum imported source records.
    pub sources: u32,
    /// Maximum selected source meshes.
    pub meshes: u32,
    /// Maximum vertices across selected meshes.
    pub vertices: u64,
    /// Maximum indices across selected meshes.
    pub indices: u64,
    /// Maximum structural joints across source snapshots.
    pub joints: u32,
    /// Maximum diagnostics retained in one result.
    pub diagnostics: u32,
}

impl Default for PlantCompileLimits {
    fn default() -> Self {
        Self {
            sources: 1_024,
            meshes: 65_536,
            vertices: 100_000_000,
            indices: 300_000_000,
            joints: 65_535,
            diagnostics: 4_096,
        }
    }
}

/// One format-erased mesh element resolved by `saffron-assets`.
#[derive(Clone, Debug, PartialEq)]
pub struct PlantSourceMeshSnapshot {
    /// Stable source element selector.
    pub selector: PlantSourceSelector,
    /// Source-element transform into its source root.
    pub transform: Mat4,
    /// Canonical CPU mesh bytes decoded by `saffron-geometry`.
    pub mesh: Mesh,
    /// Optional skin stream parallel to the mesh vertices.
    pub skin: Vec<VertexSkin>,
    /// Catalog material identity for each source material slot.
    pub material_slots: Vec<Uuid>,
}

/// One format-erased material dependency resolved by `saffron-assets`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantSourceMaterialSnapshot {
    /// Stable source element selector.
    pub selector: PlantSourceSelector,
    /// Catalog material identity.
    pub material: Uuid,
    /// SHA-256 over the complete resolved material dependency closure.
    pub content_hash: [u8; 32],
    /// Physically defined material surface contract.
    pub surface: MaterialSurface,
    /// Coverage classification after resolving the complete material.
    pub alpha_classification: AlphaClassification,
    /// Canonical coverage source after resolving the complete material.
    pub coverage_source: CoverageSource,
}

/// One structural joint resolved from a source model.
#[derive(Clone, Debug, PartialEq)]
pub struct PlantSourceJointSnapshot {
    /// Stable joint selector.
    pub selector: PlantSourceSelector,
    /// Stable parent joint selector.
    pub parent: Option<PlantSourceSelector>,
    /// Joint rest transform into the source root.
    pub rest_transform: Mat4,
}

/// Complete canonical snapshot for one authored source reference.
#[derive(Clone, Debug, PartialEq)]
pub struct PlantSourceSnapshot {
    /// Recipe source identity.
    pub source: u128,
    /// SHA-256 of the complete logical source contribution.
    pub content_hash: [u8; 32],
    /// Mesh elements available to geometry/collision/navigation roles.
    pub meshes: Vec<PlantSourceMeshSnapshot>,
    /// Resolved material elements and dependency hashes.
    pub materials: Vec<PlantSourceMaterialSnapshot>,
    pub joints: Vec<PlantSourceJointSnapshot>,
    /// Additional stable semantic elements without a mesh/material/joint payload.
    pub semantic_elements: Vec<PlantSourceSelector>,
}

impl PlantSourceSnapshot {
    pub(super) fn contains_selector(&self, selector: &PlantSourceSelector) -> bool {
        match selector {
            PlantSourceSelector::Whole => true,
            PlantSourceSelector::Element { .. } => {
                self.meshes
                    .iter()
                    .any(|mesh| selector_element_matches(selector, &mesh.selector))
                    || self
                        .materials
                        .iter()
                        .any(|material| selector_element_matches(selector, &material.selector))
                    || self
                        .joints
                        .iter()
                        .any(|joint| selector_element_matches(selector, &joint.selector))
                    || self.semantic_elements.contains(selector)
            }
            PlantSourceSelector::Submesh { element, index } => self.meshes.iter().any(|mesh| {
                selector_element_id(&mesh.selector) == Some(*element)
                    && (*index as usize) < mesh.mesh.submeshes.len()
            }),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct SelectedMesh<'a> {
    pub(super) snapshot: &'a PlantSourceMeshSnapshot,
    pub(super) submesh: Option<usize>,
}

impl SelectedMesh<'_> {
    pub(super) fn output_selector(self) -> PlantSourceSelector {
        match self.submesh {
            Some(index) => PlantSourceSelector::Submesh {
                element: selector_element_id(&self.snapshot.selector).unwrap_or(0),
                index: index as u32,
            },
            None => self.snapshot.selector.clone(),
        }
    }
}

pub(super) fn selected_meshes<'a>(
    snapshot: &'a PlantSourceSnapshot,
    selector: &PlantSourceSelector,
) -> Vec<SelectedMesh<'a>> {
    match selector {
        PlantSourceSelector::Whole => snapshot
            .meshes
            .iter()
            .map(|snapshot| SelectedMesh {
                snapshot,
                submesh: None,
            })
            .collect(),
        PlantSourceSelector::Element { .. } => snapshot
            .meshes
            .iter()
            .filter(|mesh| selector_element_matches(selector, &mesh.selector))
            .map(|snapshot| SelectedMesh {
                snapshot,
                submesh: None,
            })
            .collect(),
        PlantSourceSelector::Submesh { element, index } => snapshot
            .meshes
            .iter()
            .filter(|mesh| selector_element_id(&mesh.selector) == Some(*element))
            .filter_map(|mesh| {
                ((*index as usize) < mesh.mesh.submeshes.len()).then_some(SelectedMesh {
                    snapshot: mesh,
                    submesh: Some(*index as usize),
                })
            })
            .collect(),
    }
}

pub(super) struct PartPartitionError {
    pub(super) path: &'static str,
    pub(super) message: String,
}

/// Splits a geometry selection into per-submesh rows wherever Part-destination submesh targets
/// partition an element. A row bound to no target is placed by every part referencing the source,
/// so more than one such part would draw it twice.
pub(super) fn partition_selected_by_parts<'a>(
    asset: &PlantFamilyAsset,
    source: u128,
    selected: Vec<SelectedMesh<'a>>,
    targets: Option<&Vec<&PlantManualSemanticTarget>>,
) -> std::result::Result<Vec<SelectedMesh<'a>>, PartPartitionError> {
    let part_targets = targets
        .into_iter()
        .flatten()
        .copied()
        .filter(|target| {
            matches!(target.destination, PlantSemanticDestination::Part(_))
                && matches!(target.selector, PlantSourceSelector::Submesh { .. })
        })
        .collect::<Vec<_>>();
    let referencing_parts = asset
        .parts
        .iter()
        .filter(|part| part.sources.contains(&source))
        .count();
    let mut output = Vec::with_capacity(selected.len());
    for row in selected {
        let partitioned = row.submesh.is_none()
            && selector_element_id(&row.snapshot.selector).is_some_and(|element| {
                part_targets.iter().any(|target| {
                    matches!(&target.selector,
                        PlantSourceSelector::Submesh { element: target_element, .. }
                            if *target_element == element)
                })
            });
        if partitioned {
            for index in 0..row.snapshot.mesh.submeshes.len() {
                output.push(SelectedMesh {
                    snapshot: row.snapshot,
                    submesh: Some(index),
                });
            }
        } else {
            output.push(row);
        }
    }
    for row in &output {
        let selector = row.output_selector();
        let bound = part_targets
            .iter()
            .any(|target| target.selector == selector);
        if !bound && referencing_parts > 1 {
            return Err(PartPartitionError {
                path: "source.imported.semanticTargets",
                message: format!(
                    "several parts reference this source, and selection {selector:?} is bound \
                     to none of them; bind every submesh to its part with a semantic target"
                ),
            });
        }
    }
    Ok(output)
}

pub(super) fn selected_joints<'a>(
    snapshot: &'a PlantSourceSnapshot,
    selector: &PlantSourceSelector,
) -> Vec<&'a PlantSourceJointSnapshot> {
    snapshot
        .joints
        .iter()
        .filter(|joint| match selector {
            PlantSourceSelector::Whole => true,
            PlantSourceSelector::Element { .. } => {
                selector_element_matches(selector, &joint.selector)
            }
            PlantSourceSelector::Submesh { .. } => false,
        })
        .collect()
}

pub(super) fn semantic_targets_by_source(
    targets: &[PlantManualSemanticTarget],
) -> BTreeMap<u128, Vec<&PlantManualSemanticTarget>> {
    let mut grouped = BTreeMap::<u128, Vec<_>>::new();
    for target in targets {
        grouped.entry(target.source).or_default().push(target);
    }
    grouped
}

pub(super) fn snapshot_selector_has_material(
    snapshot: &PlantSourceSnapshot,
    selector: &PlantSourceSelector,
) -> bool {
    snapshot.materials.iter().any(|material| match selector {
        PlantSourceSelector::Whole => true,
        PlantSourceSelector::Element { .. } => {
            selector_element_matches(selector, &material.selector)
        }
        PlantSourceSelector::Submesh { .. } => false,
    })
}

pub(super) fn selector_element_matches(
    first: &PlantSourceSelector,
    second: &PlantSourceSelector,
) -> bool {
    selector_element_id(first).is_some_and(|id| selector_element_id(second) == Some(id))
}

pub(super) fn selector_element_id(selector: &PlantSourceSelector) -> Option<u128> {
    match selector {
        PlantSourceSelector::Whole => None,
        PlantSourceSelector::Element { id, .. } => Some(*id),
        PlantSourceSelector::Submesh { element, .. } => Some(*element),
    }
}

pub(super) fn selector_matches_output(
    requested: &PlantSourceSelector,
    output: &PlantSourceSelector,
) -> bool {
    match requested {
        PlantSourceSelector::Whole => true,
        PlantSourceSelector::Element { .. } => selector_element_matches(requested, output),
        PlantSourceSelector::Submesh { .. } => requested == output,
    }
}
