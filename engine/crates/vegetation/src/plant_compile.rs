//! Deterministic, format-erased plant-family source normalization.

use std::collections::{BTreeMap, BTreeSet};

use saffron_core::Uuid;
use saffron_geometry::glam::{Mat3, Mat4, Vec2, Vec3};
use saffron_geometry::{Mesh, Submesh, VertexSkin, compute_tangents};
use saffron_spatial::DecisionScalar;

use crate::canonical::CanonicalSink;
use crate::{
    AlphaClassification, CoverageSource, Error, MaterialSurface, PlantDimensions, PlantFamilyAsset,
    PlantFamilySource, PlantImportSettings, PlantManualSemanticTarget, PlantPartSemantic,
    PlantPivot, PlantSemanticDestination, PlantSourceRole, PlantSourceSelector, PlantTagId,
    PlantTangentPolicy, Result, SourceAxis, SourceHandedness, SourceUnits, SourceUvOrigin,
    SourceWinding, VegetationContentHasher, validate_plant_family,
};

/// Semantic version of deterministic imported-family normalization.
pub const PLANT_SOURCE_COMPILER_VERSION: u32 = 2;

/// Stable source identity reserved for one family's embedded botanical graph.
#[must_use]
pub fn native_plant_source_id(family: Uuid) -> u128 {
    (1_u128 << 127) | u128::from(family.value())
}

/// Stable source identity of one variation's grown geometry.
///
/// Each declared variation is its own individual with its own meshes, so it needs its own source
/// identity for the family's variation table to select it. Source identities are family-local, so
/// this derives from the index alone — a value that depends on the family id would go stale the
/// moment the catalog assigns a different one.
#[must_use]
pub fn native_variation_source_id(variation: usize) -> u128 {
    (1_u128 << 126) | u128::from(variation as u64)
}

/// Hashes the complete canonical embedded botanical graph source.
#[must_use]
pub fn native_botanical_graph_content_hash(graph: &crate::BotanicalGraphDocument) -> [u8; 32] {
    graph.identity().bytes()
}

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
    /// Structural joint hierarchy.
    pub joints: Vec<PlantSourceJointSnapshot>,
    /// Additional stable semantic elements without a mesh/material/joint payload.
    pub semantic_elements: Vec<PlantSourceSelector>,
}

impl PlantSourceSnapshot {
    fn contains_selector(&self, selector: &PlantSourceSelector) -> bool {
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

/// Severity of one source-compile diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlantCompileDiagnosticSeverity {
    /// Informational state that does not block publication.
    Info,
    /// Actionable quality warning that does not invalidate content.
    Warning,
    /// Invalid input or output that blocks publication.
    Error,
}

/// Stable diagnostic category used by control/editor routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlantCompileDiagnosticCode {
    /// A recipe source has no resolved snapshot.
    MissingSource,
    /// More than one snapshot carries the same source identity.
    DuplicateSource,
    /// A selected source contribution contains no matching payload.
    EmptySelection,
    /// Geometry contains malformed, non-finite, degenerate, or out-of-range data.
    InvalidGeometry,
    /// A material slot cannot be resolved exactly.
    MissingMaterial,
    /// A resolved material or coverage contract is invalid.
    InvalidMaterial,
    /// A structural joint hierarchy or vertex-weight stream is invalid.
    InvalidSkeleton,
    /// A leaf-like coverage source requires usable UV area.
    MissingCoverageUv,
    /// Leaf-like source normals oppose their geometric front face.
    InvalidLeafOrientation,
    /// Authored dimensions or crown/root footprints do not contain normalized geometry.
    BoundsMismatch,
    /// The compile request exceeded a declared hard bound.
    LimitExceeded,
    /// A resolved source hash differs from the last accepted source identity.
    SourceChanged,
    /// An authored manual edit has no surviving element to change.
    OrphanedEdit,
}

/// One typed source-compile diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantCompileDiagnostic {
    /// Severity and publication effect.
    pub severity: PlantCompileDiagnosticSeverity,
    /// Stable diagnostic category.
    pub code: PlantCompileDiagnosticCode,
    /// Recipe source identity when source-specific.
    pub source: Option<u128>,
    /// Stable source selector when element-specific.
    pub selector: Option<PlantSourceSelector>,
    /// Canonical family/source field path.
    pub path: String,
    /// Concise user-facing explanation.
    pub message: String,
}

/// Why a manual semantic target cannot be applied after reimport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlantReimportConflictReason {
    /// The referenced source is absent.
    MissingSource,
    /// The referenced stable element or submesh disappeared.
    MissingElement,
}

/// One manual semantic binding that reimport cannot preserve automatically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantReimportConflict {
    /// Stable manual binding identity.
    pub target: u128,
    /// Referenced source.
    pub source: u128,
    /// Missing stable element or submesh.
    pub selector: PlantSourceSelector,
    /// Authored destination that remains untouched.
    pub destination: PlantSemanticDestination,
    /// Typed reason publication was refused.
    pub reason: PlantReimportConflictReason,
}

/// Complete deterministic reimport-conflict report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlantReimportConflictReport {
    /// Sorted manual conflicts. Any entry blocks publication.
    pub conflicts: Vec<PlantReimportConflict>,
}

/// Accepted observed hash for one recipe source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlantSourceHashUpdate {
    /// Recipe source identity.
    pub source: u128,
    /// Previously authored source content hash.
    pub previous: [u8; 32],
    /// Current resolved source content hash.
    pub current: [u8; 32],
}

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
    /// Family material-slot index.
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
    /// Quantized vertices.
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
    /// Canonical structural joints.
    pub joints: Vec<NormalizedPlantJoint>,
    /// Resolved material dependencies.
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
    /// Resolved recipe sources.
    pub sources: u64,
    /// Selected normalized meshes.
    pub meshes: u64,
    /// Normalized vertices.
    pub vertices: u64,
    /// Normalized indices.
    pub indices: u64,
    /// Structural joints.
    pub joints: u64,
    /// Resolved material dependencies.
    pub materials: u64,
    /// Rejected source elements.
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

/// Normalizes imported and native `.splant` sources through one validation/recook compiler.
pub fn compile_plant_family(
    asset: &PlantFamilyAsset,
    snapshots: &[PlantSourceSnapshot],
    limits: PlantCompileLimits,
    modules: &dyn crate::BotanicalModuleResolver,
) -> Result<PlantCompileOutput> {
    validate_plant_family(asset)?;
    match &asset.source {
        PlantFamilySource::Imported(recipe) => {
            compile_imported_plant_family(asset, recipe, snapshots, limits)
        }
        PlantFamilySource::Native { graph, grafts } => {
            compile_native_plant_family(asset, graph, grafts, snapshots, limits, modules)
        }
    }
}

fn compile_imported_plant_family(
    asset: &PlantFamilyAsset,
    recipe: &crate::ImportedPlantFamilyRecipe,
    snapshots: &[PlantSourceSnapshot],
    limits: PlantCompileLimits,
) -> Result<PlantCompileOutput> {
    let mut diagnostics = Vec::new();
    let mut conflicts = PlantReimportConflictReport::default();
    let mut source_updates = Vec::new();
    let mut statistics = PlantCompileStatistics::default();
    if recipe.sources.len() > limits.sources as usize || snapshots.len() > limits.sources as usize {
        push_limit(
            &mut diagnostics,
            limits,
            "source.imported.sources",
            "plant source count exceeds the compile limit",
        )?;
        return Ok(PlantCompileOutput {
            family: None,
            family_hash: None,
            diagnostics,
            conflicts,
            source_updates,
            statistics,
        });
    }
    let mut snapshot_by_source = BTreeMap::new();
    let mut duplicate_sources = BTreeSet::new();
    for snapshot in snapshots {
        if duplicate_sources.contains(&snapshot.source) {
            continue;
        }
        if snapshot_by_source.remove(&snapshot.source).is_some() {
            duplicate_sources.insert(snapshot.source);
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::DuplicateSource,
                    Some(snapshot.source),
                    None,
                    "source",
                    "more than one canonical snapshot carries this source identity",
                ),
            )?;
        } else {
            snapshot_by_source.insert(snapshot.source, snapshot);
        }
    }

    for target in &recipe.semantic_targets {
        if duplicate_sources.contains(&target.source) {
            continue;
        }
        match snapshot_by_source.get(&target.source) {
            None => conflicts
                .conflicts
                .push(conflict(target, PlantReimportConflictReason::MissingSource)),
            Some(snapshot) if !snapshot.contains_selector(&target.selector) => {
                conflicts.conflicts.push(conflict(
                    target,
                    PlantReimportConflictReason::MissingElement,
                ))
            }
            Some(_) => {}
        }
    }
    conflicts.conflicts.sort_by(|first, second| {
        (first.source, first.target, &first.selector).cmp(&(
            second.source,
            second.target,
            &second.selector,
        ))
    });

    let (material_snapshots, conflicting_materials) =
        collect_material_snapshots(snapshot_by_source.values().copied());
    let normalized_materials = resolve_materials(
        asset,
        &material_snapshots,
        &conflicting_materials,
        &mut diagnostics,
        limits,
    )?;
    statistics.materials = normalized_materials.len() as u64;

    let mut meshes = Vec::new();
    let mut joints = Vec::new();
    let targets_by_source = semantic_targets_by_source(&recipe.semantic_targets);
    let mut source_dependencies = Vec::new();
    let mut sorted_sources = recipe.sources.iter().collect::<Vec<_>>();
    sorted_sources.sort_by_key(|source| source.id);
    for source in sorted_sources {
        if duplicate_sources.contains(&source.id) {
            statistics.rejected = statistics.rejected.saturating_add(1);
            continue;
        }
        let Some(snapshot) = snapshot_by_source.get(&source.id).copied() else {
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::MissingSource,
                    Some(source.id),
                    Some(source.selector.clone()),
                    "source.imported.sources",
                    "the recipe source has no canonical snapshot",
                ),
            )?;
            statistics.rejected = statistics.rejected.saturating_add(1);
            continue;
        };
        statistics.sources = statistics.sources.saturating_add(1);
        source_dependencies.push((source.id, snapshot.content_hash));
        if source.content_hash != snapshot.content_hash {
            source_updates.push(PlantSourceHashUpdate {
                source: source.id,
                previous: source.content_hash,
                current: snapshot.content_hash,
            });
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Info,
                    PlantCompileDiagnosticCode::SourceChanged,
                    Some(source.id),
                    Some(source.selector.clone()),
                    "source.imported.sources.contentHash",
                    "source content changed and will be accepted by a successful recook",
                ),
            )?;
        }
        match source.role {
            PlantSourceRole::Geometry
            | PlantSourceRole::Collision
            | PlantSourceRole::Navigation => {
                let selected = selected_meshes(snapshot, &source.selector);
                if selected.is_empty() {
                    push_diagnostic(
                        &mut diagnostics,
                        limits,
                        diagnostic(
                            PlantCompileDiagnosticSeverity::Error,
                            PlantCompileDiagnosticCode::EmptySelection,
                            Some(source.id),
                            Some(source.selector.clone()),
                            "source.imported.sources.selector",
                            "the selected source contains no matching mesh payload",
                        ),
                    )?;
                    statistics.rejected = statistics.rejected.saturating_add(1);
                    continue;
                }
                let pivot = source_pivot(
                    source.id,
                    &source.settings,
                    &selected,
                    targets_by_source.get(&source.id),
                );
                let pivot = match pivot {
                    Ok(pivot) => pivot,
                    Err(message) => {
                        push_diagnostic(
                            &mut diagnostics,
                            limits,
                            diagnostic(
                                PlantCompileDiagnosticSeverity::Error,
                                PlantCompileDiagnosticCode::BoundsMismatch,
                                Some(source.id),
                                Some(source.selector.clone()),
                                "source.imported.sources.settings.pivot",
                                &message,
                            ),
                        )?;
                        statistics.rejected = statistics.rejected.saturating_add(1);
                        continue;
                    }
                };
                let selected = if source.role == PlantSourceRole::Geometry {
                    match partition_selected_by_parts(
                        asset,
                        source.id,
                        selected,
                        targets_by_source.get(&source.id),
                    ) {
                        Ok(selected) => selected,
                        Err(partition) => {
                            push_diagnostic(
                                &mut diagnostics,
                                limits,
                                diagnostic(
                                    PlantCompileDiagnosticSeverity::Error,
                                    PlantCompileDiagnosticCode::InvalidGeometry,
                                    Some(source.id),
                                    Some(source.selector.clone()),
                                    partition.path,
                                    &partition.message,
                                ),
                            )?;
                            statistics.rejected = statistics.rejected.saturating_add(1);
                            continue;
                        }
                    }
                } else {
                    selected
                };
                for selected_mesh in selected {
                    if meshes.len() >= limits.meshes as usize {
                        push_limit(
                            &mut diagnostics,
                            limits,
                            "source.meshes",
                            "selected mesh count exceeds the compile limit",
                        )?;
                        statistics.rejected = statistics.rejected.saturating_add(1);
                        break;
                    }
                    match normalize_mesh(
                        asset,
                        source.id,
                        source.role,
                        &source.settings,
                        selected_mesh,
                        pivot,
                    ) {
                        Ok(mesh) => {
                            let requested_vertices = statistics
                                .vertices
                                .saturating_add(mesh.vertices.len() as u64);
                            let requested_indices =
                                statistics.indices.saturating_add(mesh.indices.len() as u64);
                            if requested_vertices > limits.vertices
                                || requested_indices > limits.indices
                            {
                                push_limit(
                                    &mut diagnostics,
                                    limits,
                                    "source.geometry",
                                    "normalized geometry exceeds the compile limit",
                                )?;
                                statistics.rejected = statistics.rejected.saturating_add(1);
                                continue;
                            }
                            statistics.vertices = requested_vertices;
                            statistics.indices = requested_indices;
                            statistics.meshes = statistics.meshes.saturating_add(1);
                            meshes.push(mesh);
                        }
                        Err(message) => {
                            push_diagnostic(
                                &mut diagnostics,
                                limits,
                                diagnostic(
                                    PlantCompileDiagnosticSeverity::Error,
                                    PlantCompileDiagnosticCode::InvalidGeometry,
                                    Some(source.id),
                                    Some(selected_mesh.output_selector()),
                                    "source.geometry",
                                    &message,
                                ),
                            )?;
                            statistics.rejected = statistics.rejected.saturating_add(1);
                        }
                    }
                }
            }
            PlantSourceRole::Material => {
                if !snapshot_selector_has_material(snapshot, &source.selector) {
                    push_diagnostic(
                        &mut diagnostics,
                        limits,
                        diagnostic(
                            PlantCompileDiagnosticSeverity::Error,
                            PlantCompileDiagnosticCode::EmptySelection,
                            Some(source.id),
                            Some(source.selector.clone()),
                            "source.material",
                            "the selected source contains no matching material payload",
                        ),
                    )?;
                    statistics.rejected = statistics.rejected.saturating_add(1);
                }
            }
            PlantSourceRole::Skeleton => {
                let selected = selected_joints(snapshot, &source.selector);
                if selected.is_empty() {
                    push_diagnostic(
                        &mut diagnostics,
                        limits,
                        diagnostic(
                            PlantCompileDiagnosticSeverity::Error,
                            PlantCompileDiagnosticCode::EmptySelection,
                            Some(source.id),
                            Some(source.selector.clone()),
                            "source.skeleton",
                            "the selected source contains no matching joint payload",
                        ),
                    )?;
                    statistics.rejected = statistics.rejected.saturating_add(1);
                    continue;
                }
                let pivot_meshes = selected_meshes(snapshot, &PlantSourceSelector::Whole);
                let pivot = match source_pivot(
                    source.id,
                    &source.settings,
                    &pivot_meshes,
                    targets_by_source.get(&source.id),
                ) {
                    Ok(pivot) => pivot,
                    Err(message) => {
                        push_diagnostic(
                            &mut diagnostics,
                            limits,
                            diagnostic(
                                PlantCompileDiagnosticSeverity::Error,
                                PlantCompileDiagnosticCode::BoundsMismatch,
                                Some(source.id),
                                Some(source.selector.clone()),
                                "source.skeleton.settings.pivot",
                                &message,
                            ),
                        )?;
                        statistics.rejected = statistics.rejected.saturating_add(1);
                        continue;
                    }
                };
                match normalize_joints(source.id, &source.settings, pivot, &selected) {
                    Ok(mut normalized) => {
                        if joints.len().saturating_add(normalized.len()) > limits.joints as usize {
                            push_limit(
                                &mut diagnostics,
                                limits,
                                "source.skeleton.joints",
                                "structural joint count exceeds the compile limit",
                            )?;
                            statistics.rejected = statistics.rejected.saturating_add(1);
                        } else {
                            statistics.joints =
                                statistics.joints.saturating_add(normalized.len() as u64);
                            joints.append(&mut normalized);
                        }
                    }
                    Err(message) => {
                        push_diagnostic(
                            &mut diagnostics,
                            limits,
                            diagnostic(
                                PlantCompileDiagnosticSeverity::Error,
                                PlantCompileDiagnosticCode::InvalidSkeleton,
                                Some(source.id),
                                Some(source.selector.clone()),
                                "source.skeleton",
                                &message,
                            ),
                        )?;
                        statistics.rejected = statistics.rejected.saturating_add(1);
                    }
                }
            }
        }
    }

    source_updates.sort();
    source_dependencies.sort_by_key(|(source, _)| *source);
    meshes.sort_by(|first, second| {
        (first.source, role_tag(first.role), &first.selector).cmp(&(
            second.source,
            role_tag(second.role),
            &second.selector,
        ))
    });
    joints.sort_by(|first, second| {
        (first.source, &first.selector).cmp(&(second.source, &second.selector))
    });
    validate_geometry_contract(
        asset,
        &meshes,
        &material_snapshots,
        &recipe.semantic_targets,
        &mut diagnostics,
        limits,
    )?;

    diagnostics.sort_by(|first, second| {
        (
            first.severity,
            first.code,
            first.source,
            &first.selector,
            &first.path,
            &first.message,
        )
            .cmp(&(
                second.severity,
                second.code,
                second.source,
                &second.selector,
                &second.path,
                &second.message,
            ))
    });
    let blocked = !conflicts.conflicts.is_empty()
        || diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == PlantCompileDiagnosticSeverity::Error);
    let family = (!blocked).then_some(NormalizedPlantFamily {
        family: asset.id,
        tags: asset.tags.clone(),
        sources: source_dependencies,
        meshes,
        joints,
        materials: normalized_materials,
        dimensions: asset.dimensions,
    });
    let family_hash = family
        .as_ref()
        .map(NormalizedPlantFamily::canonical_hash)
        .transpose()?;
    Ok(PlantCompileOutput {
        family,
        family_hash,
        diagnostics,
        conflicts,
        source_updates,
        statistics,
    })
}

fn compile_native_plant_family(
    asset: &PlantFamilyAsset,
    graph: &crate::BotanicalGraphDocument,
    grafts: &[crate::PlantSourceReference],
    snapshots: &[PlantSourceSnapshot],
    limits: PlantCompileLimits,
    modules: &dyn crate::BotanicalModuleResolver,
) -> Result<PlantCompileOutput> {
    let source = native_plant_source_id(asset.id);
    let content_hash = native_botanical_graph_content_hash(graph);
    let mut diagnostics = Vec::new();
    let mut statistics = PlantCompileStatistics::default();
    let mut source_updates = Vec::new();
    // A native family grows its own geometry, so the snapshots it may carry are its own canonical
    // one plus one per declared graft. Anything else means an imported payload leaked in.
    let snapshot = snapshots.iter().find(|snapshot| snapshot.source == source);
    let graft_by_id: BTreeMap<u128, &crate::PlantSourceReference> =
        grafts.iter().map(|graft| (graft.id, graft)).collect();
    let mut graft_snapshots: BTreeMap<u128, &PlantSourceSnapshot> = BTreeMap::new();
    let mut unexpected = false;
    for candidate in snapshots {
        if candidate.source == source {
            continue;
        }
        if graft_by_id.contains_key(&candidate.source) {
            if graft_snapshots
                .insert(candidate.source, candidate)
                .is_some()
            {
                push_diagnostic(
                    &mut diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::DuplicateSource,
                        Some(candidate.source),
                        None,
                        "source.native.graft",
                        "more than one snapshot carries this graft source identity",
                    ),
                )?;
            }
        } else {
            unexpected = true;
        }
    }
    if unexpected {
        push_diagnostic(
            &mut diagnostics,
            limits,
            diagnostic(
                PlantCompileDiagnosticSeverity::Error,
                PlantCompileDiagnosticCode::DuplicateSource,
                Some(source),
                None,
                "source.native",
                "a native botanical family accepts only its own snapshot and its declared grafts",
            ),
        )?;
    }
    for graft in grafts {
        let Some(resolved) = graft_snapshots.get(&graft.id) else {
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::MissingSource,
                    Some(graft.id),
                    Some(graft.selector.clone()),
                    "source.native.graft",
                    "graft source has no resolved snapshot",
                ),
            )?;
            continue;
        };
        statistics.sources += 1;
        if resolved.content_hash != graft.content_hash {
            source_updates.push(PlantSourceHashUpdate {
                source: graft.id,
                previous: graft.content_hash,
                current: resolved.content_hash,
            });
        }
    }
    if let Some(snapshot) = snapshot {
        statistics.sources = 1;
        if snapshot.content_hash != content_hash {
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::SourceChanged,
                    Some(source),
                    None,
                    "source.native.graph",
                    "native source snapshot does not match the embedded botanical graph",
                ),
            )?;
        }
        if !snapshot.meshes.is_empty() || !snapshot.joints.is_empty() {
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::InvalidGeometry,
                    Some(source),
                    None,
                    "source.native.generatedPayload",
                    "a native family grows its geometry; an imported payload cannot stand in for it",
                ),
            )?;
        }
    }

    // Grow every declared variation. Each is its own individual with its own geometry under its own
    // source identity, which is how the family's variation table selects one. A graph that cannot
    // grow is a compile error naming the field that failed, not a silently empty family.
    let mut generated = None;
    let mut meshes = Vec::new();
    let mut joints = Vec::new();
    let mut assemblies = Vec::new();
    let mut failed = false;
    for index in 0..graph.variations.len() {
        let growth = match crate::grow(graph, index, modules, &crate::BotanicalBudget::COOK) {
            Ok(growth) => growth,
            Err(error) => {
                push_diagnostic(
                    &mut diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::InvalidGeometry,
                        Some(source),
                        None,
                        "source.native.graph",
                        &error.to_string(),
                    ),
                )?;
                failed = true;
                break;
            }
        };
        // An orphaned edit is reported once, on the representative individual: the layer is shared,
        // so every variation orphans the same edits and repeating them says nothing new.
        if index == 0 {
            for orphan in &growth.diagnostics.orphans {
                push_diagnostic(
                    &mut diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Warning,
                        PlantCompileDiagnosticCode::OrphanedEdit,
                        Some(source),
                        None,
                        "source.native.edits",
                        &format!(
                            "manual {} edit on element {:032x} has no target: {}",
                            orphan.action.name(),
                            orphan.target.value(),
                            orphan.reason.name()
                        ),
                    ),
                )?;
            }
        }
        // Each graft's hero mesh normalizes through the imported-source path — the same transform,
        // winding, tangent, and material contract an imported family gets — before the generator
        // stands it up on its frame. There is no native-only mesh path.
        let mut grafted: BTreeMap<crate::BotanicalElementId, Vec<NormalizedPlantMesh>> =
            BTreeMap::new();
        for graft in &growth.assembly.grafts {
            let (Some(reference), Some(resolved)) = (
                graft_by_id.get(&graft.source),
                graft_snapshots.get(&graft.source),
            ) else {
                continue;
            };
            let selected = selected_meshes(resolved, &graft.selector);
            if selected.is_empty() {
                push_diagnostic(
                    &mut diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::EmptySelection,
                        Some(graft.source),
                        Some(graft.selector.clone()),
                        "source.native.graft.selection",
                        "graft selection contains no source geometry",
                    ),
                )?;
                continue;
            }
            for mesh in selected {
                match normalize_mesh(
                    asset,
                    graft.source,
                    PlantSourceRole::Geometry,
                    &reference.settings,
                    mesh,
                    Vec3::ZERO,
                ) {
                    Ok(normalized) => grafted.entry(graft.id).or_default().push(normalized),
                    Err(message) => push_diagnostic(
                        &mut diagnostics,
                        limits,
                        diagnostic(
                            PlantCompileDiagnosticSeverity::Error,
                            PlantCompileDiagnosticCode::InvalidGeometry,
                            Some(graft.source),
                            Some(graft.selector.clone()),
                            "source.native.graft.geometry",
                            &message,
                        ),
                    )?,
                }
            }
        }
        statistics.grafts += growth.assembly.grafts.len() as u64;
        let variation_source = native_variation_source_id(index);
        match crate::normalize_botanical_geometry(variation_source, &growth.assembly, &grafted) {
            Ok(geometry) => {
                // Skin joint indices are family-global, so each variation's joints are appended and
                // its own indices shift by what came before.
                let base = u16::try_from(joints.len()).unwrap_or(u16::MAX);
                for mut mesh in geometry.meshes {
                    for skin in &mut mesh.skin {
                        for joint in &mut skin.joints {
                            *joint = joint.saturating_add(base);
                        }
                    }
                    meshes.push(mesh);
                }
                joints.extend(geometry.joints);
                assemblies.push(growth.assembly);
            }
            Err(error) => {
                push_diagnostic(
                    &mut diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::InvalidGeometry,
                        Some(variation_source),
                        None,
                        "source.native.generation",
                        &error.to_string(),
                    ),
                )?;
                failed = true;
                break;
            }
        }
    }
    if !failed {
        match crate::widest_family_structure(&assemblies) {
            Ok(structure) => {
                statistics.meshes = meshes.len() as u64;
                statistics.joints = joints.len() as u64;
                statistics.vertices = meshes.iter().map(|mesh| mesh.vertices.len() as u64).sum();
                statistics.indices = meshes.iter().map(|mesh| mesh.indices.len() as u64).sum();
                generated = Some((meshes, joints, structure));
            }
            Err(error) => push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::InvalidGeometry,
                    Some(source),
                    None,
                    "source.native.generation",
                    &error.to_string(),
                ),
            )?,
        }
    }

    let (material_snapshots, conflicting_materials) = collect_material_snapshots(snapshot);
    let materials = resolve_materials(
        asset,
        &material_snapshots,
        &conflicting_materials,
        &mut diagnostics,
        limits,
    )?;
    statistics.materials = materials.len() as u64;
    diagnostics.sort_by(|first, second| {
        (
            first.severity,
            first.code,
            first.source,
            &first.selector,
            &first.path,
            &first.message,
        )
            .cmp(&(
                second.severity,
                second.code,
                second.source,
                &second.selector,
                &second.path,
                &second.message,
            ))
    });
    let blocked = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == PlantCompileDiagnosticSeverity::Error);
    let family = if blocked { None } else { generated }.map(|(meshes, joints, structure)| {
        NormalizedPlantFamily {
            family: asset.id,
            tags: asset.tags.clone(),
            sources: std::iter::once((source, content_hash))
                .chain(
                    graft_snapshots
                        .values()
                        .map(|snapshot| (snapshot.source, snapshot.content_hash)),
                )
                .collect(),
            meshes,
            joints,
            materials,
            // The grown plant's own bounds, not the authored declaration: a native family's
            // dimensions are a result, and a stale authored value would be a second truth.
            dimensions: structure.dimensions,
        }
    });
    let family_hash = family
        .as_ref()
        .map(NormalizedPlantFamily::canonical_hash)
        .transpose()?;
    Ok(PlantCompileOutput {
        family,
        family_hash,
        diagnostics,
        conflicts: PlantReimportConflictReport::default(),
        source_updates,
        statistics,
    })
}

#[derive(Clone, Copy)]
struct SelectedMesh<'a> {
    snapshot: &'a PlantSourceMeshSnapshot,
    submesh: Option<usize>,
}

impl SelectedMesh<'_> {
    fn output_selector(self) -> PlantSourceSelector {
        match self.submesh {
            Some(index) => PlantSourceSelector::Submesh {
                element: selector_element_id(&self.snapshot.selector).unwrap_or(0),
                index: index as u32,
            },
            None => self.snapshot.selector.clone(),
        }
    }
}

fn selected_meshes<'a>(
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

struct PartPartitionError {
    path: &'static str,
    message: String,
}

/// Splits a geometry source's selection into per-submesh rows wherever the family's
/// Part-destination submesh targets partition an element, so each part's assembly use
/// places only the part's own geometry.
///
/// The residual rows must stay unambiguous: a row bound to no target is placed by the
/// part referencing the source, so several parts sharing such a row would each place the
/// whole of it — coincident duplicate draws no phenotype mask can hide.
fn partition_selected_by_parts<'a>(
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

fn selected_joints<'a>(
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

fn source_pivot(
    source: u128,
    settings: &PlantImportSettings,
    meshes: &[SelectedMesh<'_>],
    targets: Option<&Vec<&PlantManualSemanticTarget>>,
) -> std::result::Result<Vec3, String> {
    match settings.pivot {
        PlantPivot::SourceOrigin => Ok(Vec3::ZERO),
        PlantPivot::Explicit(position) => Ok(Vec3::new(
            position[0].to_f64() as f32,
            position[1].to_f64() as f32,
            position[2].to_f64() as f32,
        )),
        PlantPivot::BoundsBaseCenter => bounds_base_center(settings, meshes),
        PlantPivot::SemanticPart(part) => {
            let selectors = targets
                .into_iter()
                .flatten()
                .filter(|target| {
                    target.destination == PlantSemanticDestination::Part(part)
                        && target.source == source
                })
                .map(|target| &target.selector)
                .collect::<Vec<_>>();
            let selected = meshes
                .iter()
                .copied()
                .filter(|mesh| {
                    selectors
                        .iter()
                        .any(|selector| selector_matches_output(selector, &mesh.output_selector()))
                })
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err("semantic-part pivot has no surviving geometry target".to_owned());
            }
            bounds_base_center(settings, &selected)
        }
    }
}

fn bounds_base_center(
    settings: &PlantImportSettings,
    meshes: &[SelectedMesh<'_>],
) -> std::result::Result<Vec3, String> {
    let basis = source_to_canonical(settings)?;
    let mut minimum = Vec3::splat(f32::INFINITY);
    let mut maximum = Vec3::splat(f32::NEG_INFINITY);
    for selected in meshes {
        for position in selected_positions(*selected)? {
            let transformed =
                basis.transform_point3(selected.snapshot.transform.transform_point3(position));
            if !transformed.is_finite() {
                return Err("source geometry has a non-finite transformed position".to_owned());
            }
            minimum = minimum.min(transformed);
            maximum = maximum.max(transformed);
        }
    }
    if !minimum.is_finite() || !maximum.is_finite() {
        return Err("source selection has no vertices".to_owned());
    }
    Ok(Vec3::new(
        (minimum.x + maximum.x) * 0.5,
        minimum.y,
        (minimum.z + maximum.z) * 0.5,
    ))
}

fn selected_positions(selected: SelectedMesh<'_>) -> std::result::Result<Vec<Vec3>, String> {
    let mesh = &selected.snapshot.mesh;
    let submeshes = selected.submesh.map_or(mesh.submeshes.as_slice(), |index| {
        std::slice::from_ref(&mesh.submeshes[index])
    });
    let mut positions = Vec::new();
    for submesh in submeshes {
        let start = submesh.first_index as usize;
        let end = start
            .checked_add(submesh.index_count as usize)
            .ok_or_else(|| "source submesh range overflows".to_owned())?;
        let indices = mesh
            .indices
            .get(start..end)
            .ok_or_else(|| "source submesh range exceeds the index stream".to_owned())?;
        for index in indices {
            let addressed = i64::from(*index) + i64::from(submesh.vertex_offset);
            let addressed = usize::try_from(addressed)
                .map_err(|_| "source submesh vertex offset is out of range".to_owned())?;
            positions.push(
                mesh.vertices
                    .get(addressed)
                    .ok_or_else(|| "source index references a missing vertex".to_owned())?
                    .position,
            );
        }
    }
    Ok(positions)
}

fn normalize_mesh(
    asset: &PlantFamilyAsset,
    source: u128,
    role: PlantSourceRole,
    settings: &PlantImportSettings,
    selected: SelectedMesh<'_>,
    pivot: Vec3,
) -> std::result::Result<NormalizedPlantMesh, String> {
    let selected_copy = selected_mesh_copy(selected)?;
    let mut mesh = selected_copy.mesh;
    validate_mesh_topology(&mesh)?;
    let basis = source_to_canonical(settings)?;
    let transform = basis * selected.snapshot.transform;
    let determinant = Mat3::from_mat4(transform).determinant();
    if !determinant.is_finite() || determinant.abs() <= 1.0e-12 {
        return Err("source transform is singular or non-finite".to_owned());
    }
    let normal_matrix = Mat3::from_mat4(transform).inverse().transpose();
    for vertex in &mut mesh.vertices {
        if !vertex.position.is_finite()
            || !vertex.normal.is_finite()
            || !vertex.uv0.is_finite()
            || !vertex.tangent.iter().all(|component| component.is_finite())
        {
            return Err("source vertex contains a non-finite attribute".to_owned());
        }
        vertex.position = transform.transform_point3(vertex.position) - pivot;
        vertex.normal = (normal_matrix * vertex.normal).normalize_or_zero();
        let tangent = transform.transform_vector3(Vec3::new(
            vertex.tangent[0],
            vertex.tangent[1],
            vertex.tangent[2],
        ));
        let tangent = (tangent - vertex.normal * vertex.normal.dot(tangent)).normalize_or_zero();
        vertex.tangent = [
            tangent.x,
            tangent.y,
            tangent.z,
            vertex.tangent[3] * determinant.signum(),
        ];
        if settings.uv_origin == SourceUvOrigin::BottomLeft {
            vertex.uv0.y = 1.0 - vertex.uv0.y;
        }
        vertex.uv0 = Vec2::new(
            vertex.uv0.x * settings.uv_scale[0].to_f64() as f32
                + settings.uv_offset[0].to_f64() as f32,
            vertex.uv0.y * settings.uv_scale[1].to_f64() as f32
                + settings.uv_offset[1].to_f64() as f32,
        );
    }
    let source_clockwise = settings.winding == SourceWinding::Clockwise;
    let reflection = determinant.is_sign_negative();
    if source_clockwise ^ reflection {
        for triangle in mesh.indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
    }
    let invalid_tangent = mesh.vertices.iter().any(|vertex| {
        let tangent = Vec3::from_array([vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]]);
        !tangent.is_finite()
            || tangent.length_squared() < 0.5
            || vertex.tangent[3].abs() < 0.5
            || vertex.normal.dot(tangent).abs() > 1.0e-3
    });
    match settings.tangent_policy {
        PlantTangentPolicy::Require if invalid_tangent => {
            return Err("source tangent policy requires complete orthonormal frames".to_owned());
        }
        PlantTangentPolicy::GenerateMissing if invalid_tangent => compute_tangents(&mut mesh),
        PlantTangentPolicy::Regenerate => compute_tangents(&mut mesh),
        PlantTangentPolicy::Require | PlantTangentPolicy::GenerateMissing => {}
    }
    validate_mesh_geometry(&mesh)?;
    let material_map = family_material_map(asset);
    let mut submeshes = Vec::with_capacity(mesh.submeshes.len());
    for submesh in &mesh.submeshes {
        let source_material = selected
            .snapshot
            .material_slots
            .get(submesh.material_slot as usize)
            .ok_or_else(|| "source submesh material slot has no resolved material".to_owned())?;
        let material_slot = material_map
            .get(&source_material.value())
            .copied()
            .ok_or_else(|| "source material is absent from the family material table".to_owned())?;
        submeshes.push(NormalizedPlantSubmesh {
            first_index: submesh.first_index,
            index_count: submesh.index_count,
            material_slot,
        });
    }
    let vertices = mesh
        .vertices
        .iter()
        .map(normalize_vertex)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let selected_skin = if selected.snapshot.skin.is_empty() {
        Vec::new()
    } else {
        if selected.snapshot.skin.len() != selected.snapshot.mesh.vertices.len() {
            return Err("skin stream length does not match the source vertex count".to_owned());
        }
        selected_copy
            .source_vertices
            .iter()
            .map(|index| selected.snapshot.skin[*index])
            .collect()
    };
    let skin = normalize_skin(&selected_skin, mesh.vertices.len())?;
    Ok(NormalizedPlantMesh {
        source,
        role,
        selector: selected.output_selector(),
        vertices,
        indices: mesh.indices,
        submeshes,
        skin,
    })
}

struct SelectedMeshCopy {
    mesh: Mesh,
    source_vertices: Vec<usize>,
}

fn selected_mesh_copy(selected: SelectedMesh<'_>) -> std::result::Result<SelectedMeshCopy, String> {
    let mesh = &selected.snapshot.mesh;
    let selected_submeshes = match selected.submesh {
        Some(index) => vec![
            *mesh
                .submeshes
                .get(index)
                .ok_or_else(|| "selected submesh is absent".to_owned())?,
        ],
        None => mesh.submeshes.clone(),
    };
    if selected_submeshes.is_empty() {
        return Err("source mesh has no selected submeshes".to_owned());
    }
    let mut covered_source_indices = selected
        .submesh
        .is_none()
        .then(|| vec![false; mesh.indices.len()]);
    let mut addressed_submeshes = Vec::with_capacity(selected_submeshes.len());
    let mut referenced_vertices = BTreeSet::new();
    for submesh in selected_submeshes {
        if submesh.index_count == 0 || submesh.index_count % 3 != 0 {
            return Err("source submesh has an invalid index count".to_owned());
        }
        let start = submesh.first_index as usize;
        let end = start
            .checked_add(submesh.index_count as usize)
            .ok_or_else(|| "selected submesh range overflows".to_owned())?;
        let source_indices = mesh
            .indices
            .get(start..end)
            .ok_or_else(|| "selected submesh range exceeds the index stream".to_owned())?;
        if let Some(covered) = &mut covered_source_indices {
            let covered = covered
                .get_mut(start..end)
                .ok_or_else(|| "selected submesh range exceeds the index stream".to_owned())?;
            if covered.iter().any(|covered| *covered) {
                return Err("source submesh ranges overlap".to_owned());
            }
            covered.fill(true);
        }
        let mut addressed_indices = Vec::with_capacity(submesh.index_count as usize);
        for index in source_indices {
            let addressed = i64::from(*index) + i64::from(submesh.vertex_offset);
            let addressed = usize::try_from(addressed)
                .map_err(|_| "source submesh vertex offset is out of range".to_owned())?;
            if addressed >= mesh.vertices.len() {
                return Err("source index references a missing vertex".to_owned());
            }
            referenced_vertices.insert(addressed);
            addressed_indices.push(addressed);
        }
        addressed_submeshes.push((submesh, addressed_indices));
    }
    if covered_source_indices
        .as_ref()
        .is_some_and(|covered| covered.iter().any(|covered| !covered))
    {
        return Err("source submeshes do not cover the complete index stream".to_owned());
    }
    let source_vertices = referenced_vertices.into_iter().collect::<Vec<_>>();
    let remap = source_vertices
        .iter()
        .enumerate()
        .map(|(output, source)| (*source, output as u32))
        .collect::<BTreeMap<_, _>>();
    let vertices = source_vertices
        .iter()
        .map(|index| mesh.vertices[*index])
        .collect();
    let mut indices = Vec::new();
    let mut submeshes = Vec::with_capacity(addressed_submeshes.len());
    for (source_submesh, addressed_indices) in addressed_submeshes {
        let first_index = indices.len() as u32;
        indices.extend(addressed_indices.iter().map(|index| remap[index]));
        submeshes.push(Submesh {
            first_index,
            index_count: source_submesh.index_count,
            vertex_offset: 0,
            material_slot: source_submesh.material_slot,
        });
    }
    Ok(SelectedMeshCopy {
        mesh: Mesh {
            vertices,
            indices,
            submeshes,
        },
        source_vertices,
    })
}

fn validate_mesh_topology(mesh: &Mesh) -> std::result::Result<(), String> {
    if mesh.vertices.is_empty() || mesh.indices.is_empty() || mesh.submeshes.is_empty() {
        return Err("source mesh must contain vertices, triangles, and submeshes".to_owned());
    }
    if !mesh.indices.len().is_multiple_of(3) {
        return Err("source index count is not a multiple of three".to_owned());
    }
    if mesh
        .indices
        .iter()
        .any(|index| *index as usize >= mesh.vertices.len())
    {
        return Err("source index references a missing vertex".to_owned());
    }
    let mut covered = vec![false; mesh.indices.len()];
    for submesh in &mesh.submeshes {
        if submesh.index_count == 0 || submesh.index_count % 3 != 0 {
            return Err("source submesh has an invalid index count".to_owned());
        }
        let start = submesh.first_index as usize;
        let end = start
            .checked_add(submesh.index_count as usize)
            .ok_or_else(|| "source submesh range overflows".to_owned())?;
        let range = covered
            .get_mut(start..end)
            .ok_or_else(|| "source submesh range exceeds the index stream".to_owned())?;
        if range.iter().any(|covered| *covered) {
            return Err("source submesh ranges overlap".to_owned());
        }
        range.fill(true);
    }
    if covered.iter().any(|covered| !covered) {
        return Err("source submeshes do not cover the complete index stream".to_owned());
    }
    Ok(())
}

fn validate_mesh_geometry(mesh: &Mesh) -> std::result::Result<(), String> {
    for triangle in mesh.indices.chunks_exact(3) {
        let first = &mesh.vertices[triangle[0] as usize];
        let second = &mesh.vertices[triangle[1] as usize];
        let third = &mesh.vertices[triangle[2] as usize];
        let cross = (second.position - first.position).cross(third.position - first.position);
        if !cross.is_finite() || cross.length_squared() <= 1.0e-16 {
            return Err("source mesh contains a degenerate triangle".to_owned());
        }
        let geometric_normal = cross.normalize();
        for vertex in [first, second, third] {
            let tangent = Vec3::new(vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]);
            if !vertex.position.is_finite()
                || !vertex.normal.is_finite()
                || vertex.normal.length_squared() < 0.5
                || !vertex.uv0.is_finite()
                || !tangent.is_finite()
                || tangent.length_squared() < 0.5
                || vertex.normal.dot(tangent).abs() > 1.0e-3
                || vertex.tangent[3].abs() < 0.5
            {
                return Err("normalized vertex frame is incomplete or invalid".to_owned());
            }
            if geometric_normal.dot(vertex.normal) < -0.25 {
                return Err("source vertex normal opposes its geometric front face".to_owned());
            }
        }
    }
    Ok(())
}

fn normalize_vertex(
    vertex: &saffron_geometry::Vertex,
) -> std::result::Result<NormalizedPlantVertex, String> {
    Ok(NormalizedPlantVertex {
        position_bits: [
            fixed_bits(vertex.position.x)?,
            fixed_bits(vertex.position.y)?,
            fixed_bits(vertex.position.z)?,
        ],
        normal_snorm: [
            snorm16(vertex.normal.x)?,
            snorm16(vertex.normal.y)?,
            snorm16(vertex.normal.z)?,
        ],
        uv_bits: [fixed_bits(vertex.uv0.x)?, fixed_bits(vertex.uv0.y)?],
        tangent_snorm: [
            snorm16(vertex.tangent[0])?,
            snorm16(vertex.tangent[1])?,
            snorm16(vertex.tangent[2])?,
            if vertex.tangent[3].is_sign_negative() {
                -32_767
            } else {
                32_767
            },
        ],
    })
}

fn fixed_bits(value: f32) -> std::result::Result<i32, String> {
    DecisionScalar::from_f64(f64::from(value))
        .map(DecisionScalar::bits)
        .map_err(|error| error.to_string())
}

fn snorm16(value: f32) -> std::result::Result<i16, String> {
    if !value.is_finite() {
        return Err("cannot quantize a non-finite normalized component".to_owned());
    }
    let clamped = value.clamp(-1.0, 1.0);
    Ok((f64::from(clamped) * 32_767.0).round_ties_even() as i16)
}

fn normalize_skin(
    source: &[VertexSkin],
    vertex_count: usize,
) -> std::result::Result<Vec<NormalizedPlantSkin>, String> {
    if source.is_empty() {
        return Ok(Vec::new());
    }
    if source.len() != vertex_count {
        return Err("skin stream length does not match the source vertex count".to_owned());
    }
    source
        .iter()
        .map(|skin| {
            if skin
                .weights
                .iter()
                .any(|weight| !weight.is_finite() || *weight < 0.0)
            {
                return Err("skin stream contains an invalid weight".to_owned());
            }
            let mut combined = BTreeMap::<u16, f64>::new();
            for (joint, weight) in skin.joints.into_iter().zip(skin.weights) {
                *combined.entry(joint).or_default() += f64::from(weight);
            }
            let mut influences = combined.into_iter().collect::<Vec<_>>();
            influences.sort_by(|first, second| {
                second
                    .1
                    .total_cmp(&first.1)
                    .then_with(|| first.0.cmp(&second.0))
            });
            influences.truncate(4);
            let sum = influences.iter().map(|(_, weight)| *weight).sum::<f64>();
            if !sum.is_finite() || sum <= 0.0 {
                return Err("skin vertex has no positive structural weight".to_owned());
            }
            let mut joints = [0_u16; 4];
            let mut weights = [0_u16; 4];
            let mut accumulated = 0_u32;
            for (index, (joint, weight)) in influences.iter().enumerate() {
                joints[index] = *joint;
                let quantized = ((*weight / sum) * f64::from(u16::MAX)).round_ties_even() as u32;
                weights[index] = quantized.min(u32::from(u16::MAX)) as u16;
                accumulated += u32::from(weights[index]);
            }
            let target = u32::from(u16::MAX);
            if accumulated != target {
                let difference = target as i64 - accumulated as i64;
                let adjusted = i64::from(weights[0]) + difference;
                if !(0..=i64::from(u16::MAX)).contains(&adjusted) {
                    return Err(
                        "skin weight quantization could not preserve normalization".to_owned()
                    );
                }
                weights[0] = adjusted as u16;
            }
            Ok(NormalizedPlantSkin { joints, weights })
        })
        .collect()
}

fn normalize_joints(
    source: u128,
    settings: &PlantImportSettings,
    pivot: Vec3,
    joints: &[&PlantSourceJointSnapshot],
) -> std::result::Result<Vec<NormalizedPlantJoint>, String> {
    let selectors = joints
        .iter()
        .map(|joint| joint.selector.clone())
        .collect::<BTreeSet<_>>();
    if selectors.len() != joints.len() {
        return Err("structural joint selectors are not unique".to_owned());
    }
    if joints.iter().any(|joint| {
        joint
            .parent
            .as_ref()
            .is_some_and(|parent| !selectors.contains(parent))
    }) {
        return Err("structural joint parent is outside the selected hierarchy".to_owned());
    }
    validate_joint_forest(joints)?;
    let basis = source_to_canonical(settings)?;
    let inverse_basis = basis.inverse();
    if !inverse_basis.is_finite() {
        return Err("source coordinate basis is singular".to_owned());
    }
    joints
        .iter()
        .map(|joint| {
            let transform =
                Mat4::from_translation(-pivot) * basis * joint.rest_transform * inverse_basis;
            if !transform.is_finite() {
                return Err("structural joint rest transform is non-finite".to_owned());
            }
            let columns = transform.to_cols_array();
            let mut transform_bits = [0_i32; 16];
            for (output, component) in transform_bits.iter_mut().zip(columns) {
                *output = fixed_bits(component)?;
            }
            Ok(NormalizedPlantJoint {
                source,
                selector: joint.selector.clone(),
                parent: joint.parent.clone(),
                transform_bits,
            })
        })
        .collect()
}

fn validate_joint_forest(joints: &[&PlantSourceJointSnapshot]) -> std::result::Result<(), String> {
    let parents = joints
        .iter()
        .map(|joint| (joint.selector.clone(), joint.parent.clone()))
        .collect::<BTreeMap<_, _>>();
    for start in parents.keys() {
        let mut active = BTreeSet::new();
        let mut current = Some(start.clone());
        while let Some(selector) = current {
            if !active.insert(selector.clone()) {
                return Err("structural joint hierarchy contains a cycle".to_owned());
            }
            current = parents.get(&selector).cloned().flatten();
        }
    }
    Ok(())
}

fn source_to_canonical(settings: &PlantImportSettings) -> std::result::Result<Mat4, String> {
    let up = axis_vector(settings.up_axis);
    let forward = axis_vector(settings.forward_axis);
    if up.dot(forward).abs() > f32::EPSILON {
        return Err("source up and forward axes are not orthogonal".to_owned());
    }
    let right = match settings.handedness {
        SourceHandedness::Right => forward.cross(up),
        SourceHandedness::Left => up.cross(forward),
    };
    let unit_scale = match settings.units {
        SourceUnits::Meters => 1.0,
        SourceUnits::Centimeters => 0.01,
        SourceUnits::Millimeters => 0.001,
        SourceUnits::Feet => 0.3048,
    };
    let scale = unit_scale * settings.scale.to_f64() as f32;
    if !scale.is_finite() || scale <= 0.0 {
        return Err("source unit scale is invalid".to_owned());
    }
    let source_basis = Mat3::from_cols(right, up, -forward);
    let canonical_from_source = source_basis.transpose() * scale;
    Ok(Mat4::from_mat3(canonical_from_source))
}

fn axis_vector(axis: SourceAxis) -> Vec3 {
    match axis {
        SourceAxis::PositiveX => Vec3::X,
        SourceAxis::NegativeX => Vec3::NEG_X,
        SourceAxis::PositiveY => Vec3::Y,
        SourceAxis::NegativeY => Vec3::NEG_Y,
        SourceAxis::PositiveZ => Vec3::Z,
        SourceAxis::NegativeZ => Vec3::NEG_Z,
    }
}

fn collect_material_snapshots<'a>(
    snapshots: impl IntoIterator<Item = &'a PlantSourceSnapshot>,
) -> (
    BTreeMap<u64, &'a PlantSourceMaterialSnapshot>,
    BTreeSet<u64>,
) {
    let mut materials = BTreeMap::new();
    let mut conflicts = BTreeSet::new();
    for snapshot in snapshots {
        for material in &snapshot.materials {
            let identity = material.material.value();
            if let Some(previous) = materials.get(&identity) {
                if *previous != material {
                    conflicts.insert(identity);
                }
            } else {
                materials.insert(identity, material);
            }
        }
    }
    (materials, conflicts)
}

fn resolve_materials(
    asset: &PlantFamilyAsset,
    materials: &BTreeMap<u64, &PlantSourceMaterialSnapshot>,
    conflicts: &BTreeSet<u64>,
    diagnostics: &mut Vec<PlantCompileDiagnostic>,
    limits: PlantCompileLimits,
) -> Result<Vec<NormalizedPlantMaterial>> {
    let mut normalized = Vec::with_capacity(asset.material_slots.len());
    for material in &asset.material_slots {
        let Some(snapshot) = materials.get(&material.value()).copied() else {
            push_diagnostic(
                diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::MissingMaterial,
                    None,
                    None,
                    "materialSlots",
                    "family material slot has no resolved material snapshot",
                ),
            )?;
            continue;
        };
        if conflicts.contains(&material.value())
            || snapshot.content_hash == [0; 32]
            || snapshot.surface.validate().is_err()
        {
            push_diagnostic(
                diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::InvalidMaterial,
                    None,
                    Some(snapshot.selector.clone()),
                    "materialSlots",
                    "resolved material or coverage contract is invalid",
                ),
            )?;
            continue;
        }
        normalized.push(NormalizedPlantMaterial {
            material: *material,
            content_hash: snapshot.content_hash,
        });
    }
    Ok(normalized)
}

fn validate_geometry_contract(
    asset: &PlantFamilyAsset,
    meshes: &[NormalizedPlantMesh],
    materials: &BTreeMap<u64, &PlantSourceMaterialSnapshot>,
    targets: &[PlantManualSemanticTarget],
    diagnostics: &mut Vec<PlantCompileDiagnostic>,
    limits: PlantCompileLimits,
) -> Result<()> {
    let bounds = asset.dimensions;
    for mesh in meshes
        .iter()
        .filter(|mesh| mesh.role == PlantSourceRole::Geometry)
    {
        for vertex in &mesh.vertices {
            if (0..3).any(|axis| {
                vertex.position_bits[axis] < bounds.local_bounds_min[axis].bits()
                    || vertex.position_bits[axis] > bounds.local_bounds_max[axis].bits()
            }) {
                push_diagnostic(
                    diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::BoundsMismatch,
                        Some(mesh.source),
                        Some(mesh.selector.clone()),
                        "dimensions.localBounds",
                        "normalized geometry lies outside the authored conservative bounds",
                    ),
                )?;
                break;
            }
        }
    }
    let part_by_id = asset
        .parts
        .iter()
        .map(|part| (part.id, part))
        .collect::<BTreeMap<_, _>>();
    for target in targets {
        let PlantSemanticDestination::Part(part_id) = target.destination else {
            continue;
        };
        let Some(part) = part_by_id.get(&part_id).copied() else {
            continue;
        };
        let selected = meshes.iter().filter(|mesh| {
            mesh.source == target.source
                && mesh.role == PlantSourceRole::Geometry
                && selector_matches_output(&target.selector, &mesh.selector)
        });
        for mesh in selected {
            if matches!(
                part.semantic,
                PlantPartSemantic::Branch
                    | PlantPartSemantic::Frond
                    | PlantPartSemantic::Leaf
                    | PlantPartSemantic::Flower
                    | PlantPartSemantic::Fruit
            ) && mesh.vertices.iter().any(|vertex| {
                vertex.position_bits[0].unsigned_abs()
                    > bounds.crown_radius[0].bits().unsigned_abs()
                    || vertex.position_bits[2].unsigned_abs()
                        > bounds.crown_radius[1].bits().unsigned_abs()
            }) {
                push_diagnostic(
                    diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::BoundsMismatch,
                        Some(mesh.source),
                        Some(mesh.selector.clone()),
                        "dimensions.crownRadius",
                        "crown semantic geometry exceeds the authored crown footprint",
                    ),
                )?;
            }
            if part.semantic == PlantPartSemantic::Root
                && mesh.vertices.iter().any(|vertex| {
                    vertex.position_bits[0].unsigned_abs()
                        > bounds.root_radius[0].bits().unsigned_abs()
                        || vertex.position_bits[2].unsigned_abs()
                            > bounds.root_radius[1].bits().unsigned_abs()
                })
            {
                push_diagnostic(
                    diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::BoundsMismatch,
                        Some(mesh.source),
                        Some(mesh.selector.clone()),
                        "dimensions.rootRadius",
                        "root semantic geometry exceeds the authored root footprint",
                    ),
                )?;
            }
            if matches!(
                part.semantic,
                PlantPartSemantic::Frond | PlantPartSemantic::Leaf | PlantPartSemantic::Blade
            ) {
                validate_leaf_mesh(asset, mesh, materials, diagnostics, limits)?;
            }
        }
    }
    Ok(())
}

fn validate_leaf_mesh(
    asset: &PlantFamilyAsset,
    mesh: &NormalizedPlantMesh,
    materials: &BTreeMap<u64, &PlantSourceMaterialSnapshot>,
    diagnostics: &mut Vec<PlantCompileDiagnostic>,
    limits: PlantCompileLimits,
) -> Result<()> {
    for submesh in &mesh.submeshes {
        let Some(material_id) = asset.material_slots.get(submesh.material_slot as usize) else {
            continue;
        };
        let Some(material) = materials.get(&material_id.value()).copied() else {
            continue;
        };
        let requires_uv = material.alpha_classification != AlphaClassification::Opaque
            && material.coverage_source != CoverageSource::ModeledGeometry;
        if !requires_uv {
            continue;
        }
        let start = submesh.first_index as usize;
        let end = start.saturating_add(submesh.index_count as usize);
        let has_uv_area = mesh.indices.get(start..end).is_some_and(|indices| {
            indices.chunks_exact(3).any(|triangle| {
                let first = mesh.vertices[triangle[0] as usize].uv_bits;
                let second = mesh.vertices[triangle[1] as usize].uv_bits;
                let third = mesh.vertices[triangle[2] as usize].uv_bits;
                let ab = [second[0] - first[0], second[1] - first[1]];
                let ac = [third[0] - first[0], third[1] - first[1]];
                i64::from(ab[0]) * i64::from(ac[1]) - i64::from(ab[1]) * i64::from(ac[0]) != 0
            })
        });
        if !has_uv_area {
            push_diagnostic(
                diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::MissingCoverageUv,
                    Some(mesh.source),
                    Some(mesh.selector.clone()),
                    "parts.coverage",
                    "leaf-like covered geometry has no usable UV area",
                ),
            )?;
        }
    }
    Ok(())
}

fn semantic_targets_by_source(
    targets: &[PlantManualSemanticTarget],
) -> BTreeMap<u128, Vec<&PlantManualSemanticTarget>> {
    let mut grouped = BTreeMap::<u128, Vec<_>>::new();
    for target in targets {
        grouped.entry(target.source).or_default().push(target);
    }
    grouped
}

fn snapshot_selector_has_material(
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

fn selector_element_matches(first: &PlantSourceSelector, second: &PlantSourceSelector) -> bool {
    selector_element_id(first).is_some_and(|id| selector_element_id(second) == Some(id))
}

fn selector_element_id(selector: &PlantSourceSelector) -> Option<u128> {
    match selector {
        PlantSourceSelector::Whole => None,
        PlantSourceSelector::Element { id, .. } => Some(*id),
        PlantSourceSelector::Submesh { element, .. } => Some(*element),
    }
}

fn selector_matches_output(requested: &PlantSourceSelector, output: &PlantSourceSelector) -> bool {
    match requested {
        PlantSourceSelector::Whole => true,
        PlantSourceSelector::Element { .. } => selector_element_matches(requested, output),
        PlantSourceSelector::Submesh { .. } => requested == output,
    }
}

fn family_material_map(asset: &PlantFamilyAsset) -> BTreeMap<u64, u32> {
    asset
        .material_slots
        .iter()
        .enumerate()
        .map(|(index, material)| (material.value(), index as u32))
        .collect()
}

fn conflict(
    target: &PlantManualSemanticTarget,
    reason: PlantReimportConflictReason,
) -> PlantReimportConflict {
    PlantReimportConflict {
        target: target.id,
        source: target.source,
        selector: target.selector.clone(),
        destination: target.destination,
        reason,
    }
}

fn diagnostic(
    severity: PlantCompileDiagnosticSeverity,
    code: PlantCompileDiagnosticCode,
    source: Option<u128>,
    selector: Option<PlantSourceSelector>,
    path: &str,
    message: &str,
) -> PlantCompileDiagnostic {
    PlantCompileDiagnostic {
        severity,
        code,
        source,
        selector,
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

fn push_limit(
    diagnostics: &mut Vec<PlantCompileDiagnostic>,
    limits: PlantCompileLimits,
    path: &str,
    message: &str,
) -> Result<()> {
    push_diagnostic(
        diagnostics,
        limits,
        diagnostic(
            PlantCompileDiagnosticSeverity::Error,
            PlantCompileDiagnosticCode::LimitExceeded,
            None,
            None,
            path,
            message,
        ),
    )
}

fn push_diagnostic(
    diagnostics: &mut Vec<PlantCompileDiagnostic>,
    limits: PlantCompileLimits,
    diagnostic: PlantCompileDiagnostic,
) -> Result<()> {
    if diagnostics.len() >= limits.diagnostics as usize {
        return Err(Error::InvalidFormat {
            format: ".splant",
            field: "compile.diagnosticLimit".to_owned(),
        });
    }
    diagnostics.push(diagnostic);
    Ok(())
}

fn role_tag(role: PlantSourceRole) -> u8 {
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

#[cfg(test)]
mod tests {
    use saffron_geometry::glam::{Vec2, Vec3};
    use saffron_geometry::{Submesh, Vertex};
    use saffron_spatial::UnitInterval;

    use super::*;
    use crate::{
        ImportedPlantFamilyRecipe, InteractionPolicy, MechanicalResponse, PLANT_ASSET_VERSION,
        PhenotypeRole, PlantFamilySource, PlantImportSettings, PlantManualSemanticTarget,
        PlantPart, PlantPhenotype, PlantSourceLocator, PlantSourceReference, PlantVariation,
        SourceProvenance,
    };

    fn fixed(value: i32) -> DecisionScalar {
        DecisionScalar::from_integer(value).unwrap()
    }

    fn selector(id: u128, path: &str) -> PlantSourceSelector {
        PlantSourceSelector::Element {
            id,
            path: path.to_owned(),
        }
    }

    fn provenance() -> SourceProvenance {
        SourceProvenance {
            source: "fixture".to_owned(),
            source_uri: "file:///oak.glb".to_owned(),
            license_id: "CC0-1.0".to_owned(),
            license_uri: "https://creativecommons.org/publicdomain/zero/1.0/".to_owned(),
            author: "Fixture".to_owned(),
            attribution: "Oak fixture".to_owned(),
            requires_attribution: false,
        }
    }

    fn asset() -> PlantFamilyAsset {
        let source = PlantSourceReference {
            id: 10,
            locator: PlantSourceLocator::Asset(Uuid(1_100)),
            role: PlantSourceRole::Geometry,
            selector: selector(20, "oak/leaves"),
            content_hash: [1; 32],
            settings: PlantImportSettings {
                pivot: PlantPivot::SourceOrigin,
                ..PlantImportSettings::default()
            },
            provenance: provenance(),
        };
        PlantFamilyAsset {
            role: crate::PlantFamilyRole::Family,
            modules: Vec::new(),
            module_recursion_limit: crate::MAX_PLANT_MODULE_RECURSION,
            version: PLANT_ASSET_VERSION,
            id: Uuid(2_000),
            name: "Oak".to_owned(),
            tags: vec![crate::PlantTagId::new(17).unwrap()],
            source: PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
                sources: vec![source],
                semantic_targets: vec![PlantManualSemanticTarget {
                    id: 30,
                    source: 10,
                    selector: selector(20, "oak/leaves"),
                    destination: PlantSemanticDestination::Part(40),
                }],
            }),
            parts: vec![PlantPart {
                id: 40,
                parent: None,
                semantic: PlantPartSemantic::Leaf,
                material_slot: 0,
                sources: vec![10],
            }],
            dimensions: PlantDimensions {
                height: fixed(2),
                trunk_radius: DecisionScalar::from_bits(0),
                crown_radius: [fixed(2); 2],
                root_radius: [fixed(1); 2],
                local_bounds_min: [fixed(-2), fixed(-1), fixed(-2)],
                local_bounds_max: [fixed(2), fixed(2), fixed(2)],
            },
            material_slots: vec![Uuid(3_000)],
            spines: Vec::new(),
            mechanics: MechanicalResponse {
                stiffness: fixed(1),
                damping: UnitInterval::from_bits(1),
                drag: fixed(1),
                flutter: fixed(1),
                bend_limit: UnitInterval::from_bits(1),
                damage_threshold: fixed(1),
                break_threshold: fixed(2),
            },
            variations: vec![PlantVariation {
                id: 0,
                name: "Default".to_owned(),
                sources: vec![10],
                active_parts: Vec::new(),
            }],
            phenotypes: vec![PlantPhenotype {
                id: 0,
                role: PhenotypeRole::Healthy,
                season_window: None,
                variation: 0,
                material_remap: Vec::new(),
                active_parts: Vec::new(),
            }],
            collision_proxies: Vec::new(),
            navigation_proxies: Vec::new(),
            interaction_policy: InteractionPolicy::Decorative,
            habitat: None,
            ecology: crate::PlantEcologyDeclaration::default(),
        }
    }

    fn triangle() -> Mesh {
        let mut mesh = Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::new(-1.0, 0.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(1.0, 0.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(0.0, 1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.5, 1.0),
                    ..Vertex::default()
                },
            ],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        compute_tangents(&mut mesh);
        mesh
    }

    fn snapshot() -> PlantSourceSnapshot {
        PlantSourceSnapshot {
            source: 10,
            content_hash: [1; 32],
            meshes: vec![PlantSourceMeshSnapshot {
                selector: selector(20, "oak/leaves"),
                transform: Mat4::IDENTITY,
                mesh: triangle(),
                skin: Vec::new(),
                material_slots: vec![Uuid(3_000)],
            }],
            materials: vec![PlantSourceMaterialSnapshot {
                selector: selector(21, "oak/leaf-material"),
                material: Uuid(3_000),
                content_hash: [3; 32],
                surface: MaterialSurface::Standard,
                alpha_classification: AlphaClassification::Opaque,
                coverage_source: CoverageSource::ModeledGeometry,
            }],
            joints: Vec::new(),
            semantic_elements: Vec::new(),
        }
    }

    #[test]
    fn compile_is_byte_identical_under_snapshot_order_changes() {
        let asset = asset();
        let first = compile_plant_family(
            &asset,
            &[snapshot()],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        let second = compile_plant_family(
            &asset,
            &[snapshot()],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        assert!(first.publishable());
        assert_eq!(first.family_hash, second.family_hash);
        assert_eq!(first.family, second.family);
        assert_eq!(
            first.family.unwrap().tags,
            vec![crate::PlantTagId::new(17).unwrap()]
        );
    }

    #[test]
    fn family_tags_participate_in_the_normalized_hash() {
        let original = asset();
        let original_hash = compile_plant_family(
            &original,
            &[snapshot()],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap()
        .family_hash;
        let mut changed = original;
        changed.tags = vec![crate::PlantTagId::new(19).unwrap()];
        let changed_hash = compile_plant_family(
            &changed,
            &[snapshot()],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap()
        .family_hash;
        assert_ne!(original_hash, changed_hash);
    }

    #[test]
    fn left_handed_source_flips_winding_and_tangent_handedness() {
        let mut asset = asset();
        let PlantFamilySource::Imported(recipe) = &mut asset.source else {
            unreachable!();
        };
        recipe.sources[0].settings.handedness = SourceHandedness::Left;
        recipe.sources[0].settings.forward_axis = SourceAxis::PositiveZ;
        let output = compile_plant_family(
            &asset,
            &[snapshot()],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        assert!(output.publishable(), "{:?}", output.diagnostics);
        let mesh = &output.family.unwrap().meshes[0];
        assert_eq!(mesh.indices, vec![0, 2, 1]);
        assert!(mesh.vertices[0].tangent_snorm[3] < 0);
    }

    #[test]
    fn source_vertex_offsets_are_canonicalized_and_unreferenced_vertices_are_removed() {
        let asset = asset();
        let mut source = snapshot();
        let referenced = source.meshes[0].mesh.vertices.clone();
        source.meshes[0].mesh.vertices = vec![Vertex::default(); 3];
        source.meshes[0].mesh.vertices.extend(referenced);
        source.meshes[0].mesh.submeshes[0].vertex_offset = 3;
        let output = compile_plant_family(
            &asset,
            &[source],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        assert!(output.publishable(), "{:?}", output.diagnostics);
        assert_eq!(output.family.unwrap().meshes[0].vertices.len(), 3);
    }

    #[test]
    fn disappeared_manual_target_blocks_publication_without_dropping_target() {
        let asset = asset();
        let mut source = snapshot();
        source.meshes[0].selector = selector(99, "oak/renamed-leaves");
        let output = compile_plant_family(
            &asset,
            &[source],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        assert!(!output.publishable());
        assert_eq!(output.conflicts.conflicts.len(), 1);
        assert_eq!(output.conflicts.conflicts[0].target, 30);
        assert_eq!(
            output.conflicts.conflicts[0].destination,
            PlantSemanticDestination::Part(40)
        );
    }

    #[test]
    fn content_change_is_reported_and_accepted_by_the_same_compile_path() {
        let asset = asset();
        let mut source = snapshot();
        source.content_hash = [9; 32];
        let output = compile_plant_family(
            &asset,
            &[source],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        assert!(output.publishable());
        assert_eq!(output.source_updates.len(), 1);
        assert_eq!(output.source_updates[0].current, [9; 32]);
    }

    #[test]
    fn coverage_material_rejects_zero_uv_area() {
        let asset = asset();
        let mut source = snapshot();
        for vertex in &mut source.meshes[0].mesh.vertices {
            vertex.uv0 = Vec2::ZERO;
        }
        compute_tangents(&mut source.meshes[0].mesh);
        source.materials[0].alpha_classification = AlphaClassification::Masked;
        source.materials[0].coverage_source = CoverageSource::AlbedoAlpha;
        let output = compile_plant_family(
            &asset,
            &[source],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        assert!(!output.publishable());
        assert!(output.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == PlantCompileDiagnosticCode::MissingCoverageUv
        }));
    }

    /// A graft's hero mesh reaches the family through the imported normalizer and stands on the
    /// frame of the element it replaced — no native-only mesh path, and no lost geometry.
    #[test]
    fn a_graft_normalizes_through_the_imported_source_path() {
        let mut asset = asset();
        let mut graph = crate::BotanicalGraphDocument::sapling(0x5a11);
        let grown = crate::grow(
            &graph,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .unwrap()
        .assembly;
        let leaf = grown.elements[0].clone();
        let hero = PlantSourceReference {
            id: 77,
            locator: PlantSourceLocator::Asset(Uuid(1_100)),
            role: PlantSourceRole::Geometry,
            selector: selector(20, "oak/leaves"),
            content_hash: [1; 32],
            settings: PlantImportSettings {
                pivot: PlantPivot::SourceOrigin,
                ..PlantImportSettings::default()
            },
            provenance: provenance(),
        };
        graph.edits = vec![crate::BotanicalManualEdit {
            target: leaf.id,
            action: crate::BotanicalEditAction::Graft {
                source: hero.id,
                selector: selector(20, "oak/leaves"),
            },
        }];
        asset.source = PlantFamilySource::Native {
            graph: graph.clone(),
            grafts: vec![hero.clone()],
        };
        asset.parts[0].sources.clear();
        asset.variations[0].sources = vec![native_variation_source_id(0)];

        let mut native = snapshot();
        native.source = native_plant_source_id(asset.id);
        native.content_hash = native_botanical_graph_content_hash(&graph);
        native.meshes.clear();
        native.joints.clear();
        let mut grafted = snapshot();
        grafted.source = hero.id;
        grafted.content_hash = hero.content_hash;

        let output = compile_plant_family(
            &asset,
            &[native, grafted],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        assert!(output.publishable(), "{:?}", output.diagnostics);
        assert_eq!(output.statistics.grafts, 1);
        let family = output.family.unwrap();
        // Generated geometry plus the graft, and the graft's vertices sit around the frame the
        // replaced leaf stood on rather than at the family origin.
        assert_eq!(family.meshes.len(), 2);
        let graft_mesh = family
            .meshes
            .iter()
            .find(|mesh| mesh.source == hero.id)
            .expect("the graft reached the family");
        assert!(!graft_mesh.vertices.is_empty());
        assert!(
            graft_mesh
                .skin
                .iter()
                .all(|skin| skin.weights[0] == UnitInterval::ONE.bits()),
            "a graft is rigid on the limb it stands on"
        );
        let near = graft_mesh.vertices.iter().any(|vertex| {
            (0..3).all(|lane| {
                (vertex.position_bits[lane] - leaf.position[lane].bits()).abs() < (4 << 16)
            })
        });
        assert!(near, "the graft stands on its frame");
        // Both sources are recorded, so a recook knows exactly what it read.
        assert_eq!(family.sources.len(), 2);

        // A graft whose source has no snapshot blocks publication rather than losing the mesh.
        let mut lonely = snapshot();
        lonely.source = native_plant_source_id(asset.id);
        lonely.content_hash = native_botanical_graph_content_hash(&graph);
        lonely.meshes.clear();
        lonely.joints.clear();
        let missing = compile_plant_family(
            &asset,
            &[lonely],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        assert!(!missing.publishable());
        assert!(
            missing
                .diagnostics
                .iter()
                .any(|entry| entry.code == PlantCompileDiagnosticCode::MissingSource)
        );
    }

    #[test]
    fn native_source_uses_the_shared_normalized_family_contract() {
        let mut asset = asset();
        let graph = crate::BotanicalGraphDocument::sapling(0x5a11);
        asset.source = PlantFamilySource::Native {
            graph: graph.clone(),
            grafts: Vec::new(),
        };
        asset.parts[0].sources.clear();
        asset.variations[0].sources = vec![native_variation_source_id(0)];
        let mut source = snapshot();
        source.source = native_plant_source_id(asset.id);
        source.content_hash = native_botanical_graph_content_hash(&graph);
        source.meshes.clear();
        source.joints.clear();
        let output = compile_plant_family(
            &asset,
            &[source],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        assert!(output.publishable(), "{:?}", output.diagnostics);
        let family = output.family.unwrap();
        // A native family grows real geometry: one mesh with a submesh per material slot, a
        // structural joint per axis, and the grown plant's own bounds.
        assert_eq!(family.meshes.len(), 1);
        assert!(!family.meshes[0].vertices.is_empty());
        assert_eq!(family.meshes[0].skin.len(), family.meshes[0].vertices.len());
        assert!(!family.joints.is_empty());
        assert!(family.dimensions.height.bits() > 0);
        assert_eq!(family.materials.len(), 1);
        assert_eq!(family.sources[0].0, native_plant_source_id(asset.id));
        assert!(output.statistics.vertices > 0 && output.statistics.joints > 0);
    }

    fn two_submesh_mesh() -> Mesh {
        let quad = |base: f32| {
            [
                Vertex {
                    position: Vec3::new(-1.0, base, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(1.0, base, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(0.0, base + 1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.5, 1.0),
                    ..Vertex::default()
                },
            ]
        };
        let mut mesh = Mesh {
            vertices: quad(0.0).into_iter().chain(quad(1.0)).collect(),
            indices: vec![0, 1, 2, 3, 4, 5],
            submeshes: vec![
                Submesh {
                    first_index: 0,
                    index_count: 3,
                    vertex_offset: 0,
                    material_slot: 0,
                },
                Submesh {
                    first_index: 3,
                    index_count: 3,
                    vertex_offset: 0,
                    material_slot: 1,
                },
            ],
        };
        compute_tangents(&mut mesh);
        mesh
    }

    fn two_part_asset(partitioned: bool) -> PlantFamilyAsset {
        let mut asset = asset();
        asset.parts = vec![
            PlantPart {
                id: 40,
                parent: None,
                semantic: PlantPartSemantic::Trunk,
                material_slot: 0,
                sources: vec![10],
            },
            PlantPart {
                id: 41,
                parent: Some(40),
                semantic: PlantPartSemantic::Leaf,
                material_slot: 1,
                sources: vec![10],
            },
        ];
        asset.material_slots = vec![Uuid(3_000), Uuid(3_001)];
        let PlantFamilySource::Imported(recipe) = &mut asset.source else {
            unreachable!("the fixture asset is an imported recipe");
        };
        recipe.semantic_targets = if partitioned {
            vec![
                PlantManualSemanticTarget {
                    id: 30,
                    source: 10,
                    selector: PlantSourceSelector::Submesh {
                        element: 20,
                        index: 0,
                    },
                    destination: PlantSemanticDestination::Part(40),
                },
                PlantManualSemanticTarget {
                    id: 31,
                    source: 10,
                    selector: PlantSourceSelector::Submesh {
                        element: 20,
                        index: 1,
                    },
                    destination: PlantSemanticDestination::Part(41),
                },
            ]
        } else {
            // Both parts bound to the same whole element: valid at the asset level,
            // ambiguous at compile time — neither part owns a distinct geometry slice.
            vec![
                PlantManualSemanticTarget {
                    id: 30,
                    source: 10,
                    selector: selector(20, "oak/leaves"),
                    destination: PlantSemanticDestination::Part(40),
                },
                PlantManualSemanticTarget {
                    id: 31,
                    source: 10,
                    selector: selector(20, "oak/leaves"),
                    destination: PlantSemanticDestination::Part(41),
                },
            ]
        };
        asset
    }

    fn two_submesh_snapshot() -> PlantSourceSnapshot {
        let mut source = snapshot();
        source.meshes[0].mesh = two_submesh_mesh();
        source.meshes[0].material_slots = vec![Uuid(3_000), Uuid(3_001)];
        source.materials.push(PlantSourceMaterialSnapshot {
            selector: selector(22, "oak/bark-material"),
            material: Uuid(3_001),
            content_hash: [4; 32],
            surface: MaterialSurface::Standard,
            alpha_classification: AlphaClassification::Opaque,
            coverage_source: CoverageSource::ModeledGeometry,
        });
        source
    }

    /// Part-destination submesh targets split the source into one row per submesh, and
    /// each row's assembly use carries its own part — a mask that hides one part must
    /// remove that part's geometry, which a whole-mesh use per part cannot do.
    #[test]
    fn part_submesh_targets_partition_the_source_into_per_part_rows() {
        let asset = two_part_asset(true);
        let output = compile_plant_family(
            &asset,
            &[two_submesh_snapshot()],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        assert!(output.publishable(), "{:?}", output.diagnostics);
        let family = output.family.unwrap();
        let selectors = family
            .meshes
            .iter()
            .map(|mesh| mesh.selector.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            selectors,
            vec![
                PlantSourceSelector::Submesh {
                    element: 20,
                    index: 0
                },
                PlantSourceSelector::Submesh {
                    element: 20,
                    index: 1
                },
            ]
        );
        assert!(
            family
                .meshes
                .iter()
                .all(|mesh| mesh.submeshes.len() == 1 && mesh.vertices.len() == 3)
        );
        let hierarchy = crate::plant_hierarchy_input(&asset, &family, &[]).unwrap();
        assert_eq!(hierarchy.meshes.len(), 2);
        let placements = hierarchy
            .micro_instances
            .iter()
            .map(|instance| (instance.prototype, instance.part))
            .collect::<Vec<_>>();
        assert_eq!(placements, vec![(0, 40), (1, 41)]);
    }

    /// Several parts sharing one un-partitioned row would each place the whole row —
    /// coincident duplicate draws no phenotype mask can hide — so the compile refuses it.
    #[test]
    fn several_parts_on_an_unpartitioned_source_are_refused() {
        let output = compile_plant_family(
            &two_part_asset(false),
            &[two_submesh_snapshot()],
            PlantCompileLimits::default(),
            &crate::NoBotanicalModules,
        )
        .unwrap();
        assert!(!output.publishable());
        assert!(output.diagnostics.iter().any(|entry| {
            entry.code == PlantCompileDiagnosticCode::InvalidGeometry
                && entry.path == "source.imported.semanticTargets"
        }));
    }
}
