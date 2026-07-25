//! The cooked-plant render loader: one validated `.splantc` becomes the family's
//! renderable [`GpuMesh`] — the prototypes' vertex/index streams flattened in
//! prototype-id order (the assembly's per-prototype vertex bases), the decoded
//! portable hierarchy (pages + prototype uses), and the family's material slots.
//!
//! The family registers under its own id in the shared mesh + page-payload caches, so
//! the GPU-scene mirror renders it through the same prototype path as every mesh.

use std::sync::Arc;

use saffron_core::Uuid;
use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{
    Mesh, PortableVirtualHierarchy, Submesh, Vertex, decode_portable_virtual_hierarchy_sections,
};
use saffron_rendering::GpuMesh;
use saffron_vegetation::{
    ContentHash, PlantCompiledArtifactIndex, PlantCompiledSectionKind,
    VEGETATION_ARTIFACT_DECODE_LIMITS, plant_prototype_selector_hash,
};

use crate::error::{Error, Result};
use crate::gpu::GpuUploader;
use crate::page_stream::PagePayloadSource;
use crate::plant_cook::{
    PlantGeometryMesh, PlantPhenotypeRow, decode_material_section, decode_mesh_section,
    decode_phenotype_section,
};

/// One loaded plant family's renderable identity: the flattened family mesh and the
/// family material slot table (slot index → material asset id).
#[derive(Clone)]
pub struct PlantFamilyRender {
    /// The uploaded family mesh (assembly table + hierarchy pages included).
    pub mesh: Arc<GpuMesh>,
    /// The family's material assets in slot order.
    pub materials: Arc<[Uuid]>,
    /// The `(variation, phenotype)` identity of each assembly mask combination, in
    /// table order — the adapter resolves an instance's combination index here.
    pub combinations: Arc<[(u32, u32)]>,
    /// Each phenotype's `(id, variation, material slot remap)`.
    pub phenotypes: Arc<[PlantPhenotypeRender]>,
}

/// One phenotype's render-facing identity: its slot remap applies as instance
/// material overrides.
#[derive(Clone, Debug)]
pub struct PlantPhenotypeRender {
    /// Stable family-local phenotype identity.
    pub id: u32,
    /// Semantic role.
    pub role: saffron_vegetation::PhenotypeRole,
    /// Authored seasonal window in per-mille of the year, wrapping through 1000.
    pub season_window: Option<(u16, u16)>,
    /// The variation the phenotype renders.
    pub variation: u32,
    /// Material slot remap `(from, to)`.
    pub material_remap: Arc<[(u32, u32)]>,
}

impl crate::AssetServer {
    /// Registers one family's renderable mesh + retained cooked hierarchy under the
    /// family id, so the mirror's mesh path and the page-stream worker resolve it like
    /// any mesh.
    pub(crate) fn register_family_render(
        &mut self,
        family: Uuid,
        mesh: Arc<GpuMesh>,
        hierarchy: Arc<PortableVirtualHierarchy>,
    ) {
        self.page_source_by_uuid
            .insert(family.value(), PagePayloadSource::Cooked(hierarchy));
        self.mesh_by_uuid.insert(family.value(), Some(mesh));
    }

    /// Loads one plant family's renderable form from its validated `.splantc`, cached by
    /// exact artifact identity. The load registers the family mesh + its cooked page
    /// payload source under the family id, so the mirror's prototype path resolves it
    /// like any mesh; the family's pinned `.smat` documents register under their
    /// material ids for interning.
    pub fn load_plant_family(
        &mut self,
        gpu: &dyn GpuUploader,
        family: Uuid,
        artifact_hash: ContentHash,
    ) -> Option<PlantFamilyRender> {
        if let Some(cached) = self.plant_render_by_hash.get(&artifact_hash) {
            return cached.clone();
        }
        let built = match self.build_plant_family(gpu, family, artifact_hash) {
            Ok(render) => Some(render),
            Err(err) => {
                tracing::warn!("plant family {family:?}: {err}");
                None
            }
        };
        self.plant_render_by_hash
            .insert(artifact_hash, built.clone());
        built
    }

    fn build_plant_family(
        &mut self,
        gpu: &dyn GpuUploader,
        family: Uuid,
        artifact_hash: ContentHash,
    ) -> Result<PlantFamilyRender> {
        let artifact = self.vegetation_artifact_store().read_plant(artifact_hash)?;
        let decoded = decode_plant_render_sections(&artifact)?;
        let mesh = gpu
            .upload_mesh(&decoded.mesh, &decoded.hierarchy, &[], None, None)
            .map_err(|err| Error::Io(format!("plant family mesh upload: {err}")))?;
        for row in &decoded.material_documents {
            // An empty pinned document means the cook resolved the material by identity
            // only — the id resolves through the project catalog like any material.
            if row.1.is_empty() || self.material_by_uuid.contains_key(&row.0.value()) {
                continue;
            }
            let asset = crate::plant_cook::decode_plant_material_document(&row.1)?;
            self.material_by_uuid
                .insert(row.0.value(), Some(Arc::new(asset)));
        }
        self.register_family_render(family, Arc::clone(&mesh), Arc::new(decoded.hierarchy));
        let combinations: Arc<[(u32, u32)]> = mesh
            .assembly
            .as_ref()
            .map(|assembly| assembly.combinations.clone())
            .unwrap_or_default()
            .into();
        Ok(PlantFamilyRender {
            mesh,
            materials: decoded.material_slots.into(),
            combinations,
            phenotypes: decoded
                .phenotypes
                .into_iter()
                .map(|row| PlantPhenotypeRender {
                    id: row.id,
                    role: row.role,
                    season_window: row.season_window,
                    variation: row.variation,
                    material_remap: row.material_remap.into(),
                })
                .collect(),
        })
    }
}

/// The decoded render-facing sections of one `.splantc`.
pub(crate) struct DecodedPlantRender {
    pub(crate) mesh: Mesh,
    pub(crate) hierarchy: PortableVirtualHierarchy,
    pub(crate) material_slots: Vec<Uuid>,
    pub(crate) material_documents: Vec<(Uuid, Vec<u8>)>,
    pub(crate) phenotypes: Vec<PlantPhenotypeRow>,
}

/// Decodes the hierarchy + geometry + material sections and flattens the prototype
/// streams into the family mesh the executor's assembly path fetches from.
pub(crate) fn decode_plant_render_sections(artifact: &[u8]) -> Result<DecodedPlantRender> {
    let index = PlantCompiledArtifactIndex::open(artifact, VEGETATION_ARTIFACT_DECODE_LIMITS)?;
    let read = |kind: PlantCompiledSectionKind| -> Result<std::borrow::Cow<'_, [u8]>> {
        index
            .section(artifact, kind)?
            .ok_or_else(|| Error::Io(format!("compiled plant artifact is missing {kind:?}")))
    };
    let hierarchy = decode_portable_virtual_hierarchy_sections(
        read(PlantCompiledSectionKind::TriangleHierarchy)?.as_ref(),
        read(PlantCompiledSectionKind::VoxelHierarchy)?.as_ref(),
        read(PlantCompiledSectionKind::Deformation)?.as_ref(),
        read(PlantCompiledSectionKind::PageDirectory)?.as_ref(),
        read(PlantCompiledSectionKind::RayTracing)?.as_ref(),
    )
    .map_err(|err| Error::Io(format!("compiled plant hierarchy: {err}")))?;
    let rows = decode_mesh_section(read(PlantCompiledSectionKind::Geometry)?.as_ref())?;
    let phenotypes =
        decode_phenotype_section(read(PlantCompiledSectionKind::Phenotypes)?.as_ref())?;
    let materials =
        decode_material_section(read(PlantCompiledSectionKind::MaterialsCoverage)?.as_ref())?;
    let mesh = flatten_prototype_rows(&hierarchy, &rows)?;
    Ok(DecodedPlantRender {
        mesh,
        hierarchy,
        material_slots: materials.iter().map(|row| row.material).collect(),
        material_documents: materials
            .into_iter()
            .map(|row| (row.material, row.document))
            .collect(),
        phenotypes,
    })
}

/// Concatenates the geometry rows into the family's flat vertex/index streams in
/// prototype-id order, verifying each row against its hierarchy prototype (source,
/// selector hash, vertex count). Indices rebase to the flat stream; the assembly's
/// per-prototype vertex bases rebase the executor's page-driven fetches the same way.
fn flatten_prototype_rows(
    hierarchy: &PortableVirtualHierarchy,
    rows: &[PlantGeometryMesh],
) -> Result<Mesh> {
    if hierarchy.prototypes.len() != rows.len() {
        return Err(Error::Io(format!(
            "compiled plant geometry carries {} meshes for {} hierarchy prototypes",
            rows.len(),
            hierarchy.prototypes.len()
        )));
    }
    let mut mesh = Mesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        submeshes: Vec::new(),
    };
    for (prototype, row) in hierarchy.prototypes.iter().zip(rows) {
        let selector = plant_prototype_selector_hash(row.source, &row.selector)
            .map_err(|err| Error::Io(format!("plant prototype selector: {err}")))?;
        if prototype.source != row.source
            || prototype.selector_hash != selector
            || prototype.vertex_count as usize != row.vertices.len()
            || prototype.submesh_count as usize != row.submeshes.len()
        {
            return Err(Error::Io(
                "compiled plant geometry does not match its hierarchy prototypes".to_owned(),
            ));
        }
        let vertex_base = u32::try_from(mesh.vertices.len())
            .map_err(|_| Error::Io("plant family vertex stream overflow".to_owned()))?;
        let index_base = u32::try_from(mesh.indices.len())
            .map_err(|_| Error::Io("plant family index stream overflow".to_owned()))?;
        mesh.vertices
            .extend(row.vertices.iter().map(|vertex| Vertex {
                position: Vec3::new(
                    vertex.position_bits[0] as f32 / 65_536.0,
                    vertex.position_bits[1] as f32 / 65_536.0,
                    vertex.position_bits[2] as f32 / 65_536.0,
                ),
                normal: Vec3::new(
                    f32::from(vertex.normal_snorm[0]) / 32_767.0,
                    f32::from(vertex.normal_snorm[1]) / 32_767.0,
                    f32::from(vertex.normal_snorm[2]) / 32_767.0,
                ),
                uv0: Vec2::new(
                    vertex.uv_bits[0] as f32 / 65_536.0,
                    vertex.uv_bits[1] as f32 / 65_536.0,
                ),
                tangent: [
                    f32::from(vertex.tangent_snorm[0]) / 32_767.0,
                    f32::from(vertex.tangent_snorm[1]) / 32_767.0,
                    f32::from(vertex.tangent_snorm[2]) / 32_767.0,
                    f32::from(vertex.tangent_snorm[3]) / 32_767.0,
                ],
            }));
        for index in &row.indices {
            let rebased = index
                .checked_add(vertex_base)
                .ok_or_else(|| Error::Io("plant family index stream overflow".to_owned()))?;
            mesh.indices.push(rebased);
        }
        mesh.submeshes
            .extend(row.submeshes.iter().map(|submesh| Submesh {
                first_index: index_base + submesh.first_index,
                index_count: submesh.index_count,
                vertex_offset: 0,
                material_slot: submesh.material_slot,
            }));
    }
    Ok(mesh)
}
