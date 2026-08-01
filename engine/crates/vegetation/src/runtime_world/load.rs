//! Worker-owned cell staging: validate an artifact, decode its facets, then publish atomically.

use std::collections::BTreeMap;
use std::sync::Arc;

use saffron_spatial::{GenerationToken, ResidencyMask, WorldCellKey};

use crate::{
    ContentHash, Error, PlantTagId, Result, VEGETATION_ARTIFACT_DECODE_LIMITS,
    VegetationCellArtifactIndex, VegetationCellState, VegetationManifestCell,
    decode_vegetation_cell_facet,
};

use super::VegetationWorld;
use super::generation::{
    CellPersistentOverlay, VegetationCellGeneration, VegetationCellGenerationId,
    effective_macro_points, macro_columns, required_sections,
};
use super::residency::union_masks;

/// Privately staged, fully validated cell replacement.
pub struct StagedVegetationCellGeneration {
    pub(super) token: GenerationToken,
    pub(super) generation: Arc<VegetationCellGeneration>,
}

/// Immutable work packet that can validate and decode a cell on any worker thread.
pub struct VegetationCellLoad {
    token: GenerationToken,
    facets: ResidencyMask,
    manifest_cell: VegetationManifestCell,
    platform_profile: ContentHash,
    manifest_identity: ContentHash,
    current: Arc<VegetationCellGeneration>,
    persistent: Option<VegetationCellState>,
    family_tags: Arc<BTreeMap<u64, Vec<PlantTagId>>>,
}

impl VegetationCellLoad {
    /// Requested generation token used for deterministic cancellation diagnostics.
    #[must_use]
    pub const fn token(&self) -> GenerationToken {
        self.token
    }

    /// Validates and decodes the requested facets without touching the published world.
    pub fn stage(self, bytes: &[u8]) -> Result<StagedVegetationCellGeneration> {
        let index =
            validate_artifact_against_manifest(self.platform_profile, &self.manifest_cell, bytes)?;
        let mut decoded = self.current.facets.clone();
        let resident = union_masks(self.current.resident, self.facets);
        for kind in required_sections(self.facets) {
            let section = index
                .section(bytes, kind)?
                .ok_or_else(|| Error::ArtifactFormat {
                    format: ".svegcell",
                    field: format!("missing runtime facet {}", kind as u16),
                })?;
            decoded.insert(kind, decode_vegetation_cell_facet(kind, section.as_ref())?);
        }
        decoded.retain(|kind, _| required_sections(resident).contains(kind));
        let base = macro_columns(&decoded)?;
        let macro_points = effective_macro_points(base, self.persistent.as_ref())?;
        let overlay = CellPersistentOverlay::from_state(self.persistent.as_ref());
        let generation = Arc::new(VegetationCellGeneration::build(
            VegetationCellGenerationId {
                cell: self.token.cell,
                generation: self.token.generation,
            },
            self.manifest_identity,
            resident,
            decoded,
            macro_points,
            self.family_tags,
            overlay,
        )?);
        Ok(StagedVegetationCellGeneration {
            token: self.token,
            generation,
        })
    }
}

impl VegetationWorld {
    pub fn begin_load(
        &mut self,
        cell: WorldCellKey,
        facets: ResidencyMask,
    ) -> Result<VegetationCellLoad> {
        if facets == ResidencyMask::NONE {
            return Err(Error::ArtifactFormat {
                format: "vegetation runtime load",
                field: "facets".to_owned(),
            });
        }
        let manifest_cell = self.require_manifest_cell(cell)?.clone();
        self.ensure_runtime_cell(cell)?;
        let token = self.cells[&cell]
            .slot
            .begin(self.residency_revision)
            .map_err(Error::from)?;
        Ok(VegetationCellLoad {
            token,
            facets,
            manifest_cell,
            platform_profile: self.manifest.platform.identity()?,
            manifest_identity: self.manifest_identity,
            current: self.cells[&cell].slot.read(),
            persistent: self.effective.cells().get(&cell).cloned(),
            family_tags: Arc::clone(&self.family_tags),
        })
    }

    /// Cancels an in-flight cell load when its token remains current.
    pub fn cancel_load(&self, token: GenerationToken) -> Result<bool> {
        let entry = self
            .cells
            .get(&token.cell)
            .ok_or(Error::UnknownRuntimeCell { cell: token.cell })?;
        entry.slot.cancel(token).map_err(Into::into)
    }

    /// Atomically publishes a complete staged generation; late work is discarded.
    pub fn publish_staged(&mut self, staged: StagedVegetationCellGeneration) -> Result<bool> {
        if staged.generation.manifest_identity != self.manifest_identity {
            return Err(Error::ManifestMismatch);
        }
        let entry = self
            .cells
            .get(&staged.token.cell)
            .ok_or(Error::UnknownRuntimeCell {
                cell: staged.token.cell,
            })?;
        let published = entry.slot.try_publish(staged.token, staged.generation)?;
        if published {
            self.bump_ecology_ground_revision();
        }
        Ok(published)
    }
}

fn validate_artifact_against_manifest(
    platform_profile: ContentHash,
    cell: &VegetationManifestCell,
    bytes: &[u8],
) -> Result<VegetationCellArtifactIndex> {
    if ContentHash::of(bytes) != cell.artifact_hash {
        return Err(Error::ArtifactHashMismatch {
            format: ".svegcell",
            subject: "manifest artifact".to_owned(),
        });
    }
    let index = VegetationCellArtifactIndex::open(bytes, VEGETATION_ARTIFACT_DECODE_LIMITS)?;
    if index.cell != cell.cell || index.payload_hash != cell.payload_hash {
        return Err(Error::ArtifactFormat {
            format: ".svegcell",
            field: "manifest cell identity".to_owned(),
        });
    }
    if index.platform_profile != platform_profile {
        return Err(Error::ArtifactHashMismatch {
            format: ".svegcell",
            subject: "platform profile".to_owned(),
        });
    }
    if index.sections.len() != cell.sections.len() {
        return Err(Error::ArtifactFormat {
            format: ".svegcell",
            field: "manifest sections".to_owned(),
        });
    }
    for (actual, expected) in index.sections.iter().zip(&cell.sections) {
        if actual.kind != expected.kind
            || actual.version != expected.version
            || actual.codec != expected.codec
            || actual.alignment != expected.alignment
            || actual.stored_size != expected.stored_size
            || actual.decoded_size != expected.decoded_size
            || actual.content_hash != expected.content_hash
        {
            return Err(Error::ArtifactFormat {
                format: ".svegcell",
                field: format!("manifest section {}", expected.kind as u16),
            });
        }
    }
    Ok(index)
}
