//! The decode mirrors of the `.splantc` section writers.

use saffron_core::Uuid;
use saffron_vegetation::PlantSourceSelector;

use crate::{Error, Result};

/// One decoded Geometry-section mesh row — the prototype-order source of the family's
/// flattened render vertex stream. The decode mirrors [`append_normalized_mesh`]
/// field-for-field so the two stay in lockstep.
pub(crate) struct PlantGeometryMesh {
    /// Recipe source identity.
    pub source: u128,
    /// Exact selected source element or submesh.
    pub selector: PlantSourceSelector,
    /// Quantized family-local vertices.
    pub vertices: Vec<saffron_vegetation::NormalizedPlantVertex>,
    /// Prototype-local triangle indices.
    pub indices: Vec<u32>,
    /// Material-homogeneous draw ranges.
    pub submeshes: Vec<saffron_vegetation::NormalizedPlantSubmesh>,
}

/// One decoded MaterialsCoverage-section row: the material identity plus its resolved
/// `.smat` document bytes.
pub(crate) struct PlantMaterialRow {
    /// The referenced material asset identity.
    pub material: Uuid,
    /// The resolved `.smat` JSON document.
    pub document: Vec<u8>,
}

/// A big-endian, length-prefixed section reader — the decode mirror of the `append_*`
/// writers above.
pub(super) struct SectionReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> SectionReader<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| Error::Io("plant section payload is truncated".to_owned()))?;
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    pub(super) fn read_u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn read_i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn read_i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    fn read_u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn read_u128(&mut self) -> Result<u128> {
        Ok(u128::from_be_bytes(
            self.take(16)?.try_into().expect("16 bytes"),
        ))
    }

    fn read_length(&mut self) -> Result<usize> {
        let value = self.read_u64()?;
        usize::try_from(value)
            .ok()
            .filter(|count| *count <= self.bytes.len())
            .ok_or_else(|| Error::Io("plant section length exceeds its payload".to_owned()))
    }

    pub(super) fn read_bytes(&mut self) -> Result<&'a [u8]> {
        let count = self.read_length()?;
        self.take(count)
    }

    pub(super) fn read_string(&mut self) -> Result<String> {
        String::from_utf8(self.read_bytes()?.to_vec())
            .map_err(|_| Error::Io("plant section string is not UTF-8".to_owned()))
    }

    fn read_selector(&mut self) -> Result<PlantSourceSelector> {
        match self.read_u8()? {
            0 => Ok(PlantSourceSelector::Whole),
            1 => Ok(PlantSourceSelector::Element {
                id: self.read_u128()?,
                path: self.read_string()?,
            }),
            2 => Ok(PlantSourceSelector::Submesh {
                element: self.read_u128()?,
                index: self.read_u32()?,
            }),
            _ => Err(Error::Io(
                "plant section selector tag is unknown".to_owned(),
            )),
        }
    }

    pub(super) fn expect_domain(&mut self, domain: &[u8]) -> Result<()> {
        if self.read_bytes()? != domain {
            return Err(Error::Io(
                "plant section domain does not match its kind".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Decodes one Geometry-section payload into its prototype-ordered mesh rows.
pub(crate) fn decode_mesh_section(bytes: &[u8]) -> Result<Vec<PlantGeometryMesh>> {
    let mut reader = SectionReader::new(bytes);
    reader.expect_domain(b"saffron-anima/splantc/mesh-facet/v1")?;
    let mesh_count = reader.read_length()?;
    let mut meshes = Vec::with_capacity(mesh_count);
    for _ in 0..mesh_count {
        let source = reader.read_u128()?;
        let selector = reader.read_selector()?;
        let vertex_count = reader.read_length()?;
        let mut vertices = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            let mut vertex = saffron_vegetation::NormalizedPlantVertex::default();
            for value in &mut vertex.position_bits {
                *value = reader.read_i32()?;
            }
            for value in &mut vertex.normal_snorm {
                *value = reader.read_i16()?;
            }
            for value in &mut vertex.uv_bits {
                *value = reader.read_i32()?;
            }
            for value in &mut vertex.tangent_snorm {
                *value = reader.read_i16()?;
            }
            vertices.push(vertex);
        }
        let index_count = reader.read_length()?;
        let mut indices = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            indices.push(reader.read_u32()?);
        }
        let submesh_count = reader.read_length()?;
        let mut submeshes = Vec::with_capacity(submesh_count);
        for _ in 0..submesh_count {
            submeshes.push(saffron_vegetation::NormalizedPlantSubmesh {
                first_index: reader.read_u32()?,
                index_count: reader.read_u32()?,
                material_slot: reader.read_u32()?,
            });
        }
        let skin_count = reader.read_length()?;
        for _ in 0..skin_count {
            // 4 joint u16s + 4 weight u16s per record; the render loader has no use for
            // them (structural deformation binds through the skeleton section).
            reader.take(16)?;
        }
        meshes.push(PlantGeometryMesh {
            source,
            selector,
            vertices,
            indices,
            submeshes,
        });
    }
    Ok(meshes)
}

/// One decoded phenotype row: the identity, its variation, and the material remap.
pub(crate) struct PlantPhenotypeRow {
    /// Stable family-local phenotype identity.
    pub id: u32,
    /// Semantic role.
    pub role: saffron_vegetation::PhenotypeRole,
    /// Authored seasonal window in per-mille of the year, wrapping through 1000.
    pub season_window: Option<(u16, u16)>,
    /// The variation the phenotype renders.
    pub variation: u32,
    /// Material slot remap `(from, to)`.
    pub material_remap: Vec<(u32, u32)>,
}

/// Decodes one Phenotypes-section payload — the decode mirror of
/// [`phenotype_section`]. Active-part sets are cook inputs (baked into the assembly
/// masks) and are skipped.
pub(crate) fn decode_phenotype_section(bytes: &[u8]) -> Result<Vec<PlantPhenotypeRow>> {
    let mut reader = SectionReader::new(bytes);
    reader.expect_domain(b"saffron-anima/splantc/phenotypes/v1")?;
    let count = reader.read_length()?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let id = reader.read_u32()?;
        let role = match reader.read_u8()? {
            0 => saffron_vegetation::PhenotypeRole::Healthy,
            1 => saffron_vegetation::PhenotypeRole::Harvested,
            2 => saffron_vegetation::PhenotypeRole::Damaged,
            3 => saffron_vegetation::PhenotypeRole::Burned,
            4 => saffron_vegetation::PhenotypeRole::Dead,
            5 => saffron_vegetation::PhenotypeRole::Flowering,
            6 => saffron_vegetation::PhenotypeRole::Fruiting,
            7 => saffron_vegetation::PhenotypeRole::Senescent,
            8 => saffron_vegetation::PhenotypeRole::Wet,
            other => {
                return Err(Error::Io(format!(
                    "compiled plant phenotype role {other} is unknown"
                )));
            }
        };
        let season_window = match reader.read_u8()? {
            0 => None,
            1 => {
                let start = u16::from_le_bytes(reader.take(2)?.try_into().expect("two bytes"));
                let end = u16::from_le_bytes(reader.take(2)?.try_into().expect("two bytes"));
                Some((start, end))
            }
            other => {
                return Err(Error::Io(format!(
                    "compiled plant phenotype window flag {other} is unknown"
                )));
            }
        };
        let variation = reader.read_u32()?;
        let remap_count = reader.read_length()?;
        let mut material_remap = Vec::with_capacity(remap_count);
        for _ in 0..remap_count {
            material_remap.push((reader.read_u32()?, reader.read_u32()?));
        }
        let active_count = reader.read_length()?;
        for _ in 0..active_count {
            reader.take(16)?;
        }
        rows.push(PlantPhenotypeRow {
            id,
            role,
            season_window,
            variation,
            material_remap,
        });
    }
    Ok(rows)
}

/// Decodes one MaterialsCoverage-section payload into its material rows and the packed atlas's
/// layout. The atlas texels are the texture-container section; [`decode_family_atlas`] pairs them.
pub(crate) fn decode_material_section(
    bytes: &[u8],
) -> Result<(Vec<PlantMaterialRow>, Option<crate::AtlasLayout>)> {
    let mut reader = SectionReader::new(bytes);
    reader.expect_domain(b"saffron-anima/splantc/materials-coverage/v3")?;
    let material_count = reader.read_length()?;
    let mut materials = Vec::with_capacity(material_count);
    for _ in 0..material_count {
        let material = Uuid(reader.read_u64()?);
        // The 32-byte content hash pins the resolved document; the loader trusts the
        // artifact's own validation and keeps only the document.
        reader.take(32)?;
        let document = reader.read_bytes()?.to_vec();
        materials.push(PlantMaterialRow { material, document });
    }
    if reader.read_u32()? == 0 {
        return Ok((materials, None));
    }
    let width = reader.read_u32()?;
    let height = reader.read_u32()?;
    let gutter = reader.read_u32()?;
    let placement_count = reader.read_length()?;
    let mut placements = Vec::with_capacity(placement_count);
    for _ in 0..placement_count {
        placements.push(crate::AtlasPlacement {
            slot: reader.read_u32()?,
            x: reader.read_u32()?,
            y: reader.read_u32()?,
            width: reader.read_u32()?,
            height: reader.read_u32()?,
        });
    }
    Ok((
        materials,
        Some(crate::AtlasLayout {
            width,
            height,
            placements,
            gutter,
        }),
    ))
}

/// Pairs the atlas layout from the MaterialsCoverage section with the texels from the
/// texture-container section into the one atlas the family's UVs address.
///
/// # Errors
///
/// Returns [`Error::Io`] when only one half is present, when the container's extent disagrees with
/// the layout, or when its chain stops short of 1×1.
pub(crate) fn decode_family_atlas(
    layout: Option<crate::AtlasLayout>,
    container: &[u8],
) -> Result<Option<crate::FamilyAtlas>> {
    let Some(texture) = paired_atlas_texture(layout.as_ref(), container)? else {
        return Ok(None);
    };
    let levels = (0..texture.levels.len())
        .map(|level| {
            let (width, height) = texture
                .level_extent(level)
                .ok_or_else(|| Error::Io("compiled plant atlas level is missing".to_owned()))?;
            Ok(crate::CoverageMip {
                width,
                height,
                rgba: texture.levels[level].to_vec(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(crate::FamilyAtlas {
        layout: layout.expect("a paired atlas carries its layout"),
        format: texture.format,
        levels,
    }))
}

/// Checks the two atlas halves against each other without copying the texels.
///
/// # Errors
///
/// Returns [`Error::Io`] for the same disagreements [`decode_family_atlas`] rejects.
pub(crate) fn validate_family_atlas(
    layout: Option<&crate::AtlasLayout>,
    container: &[u8],
) -> Result<()> {
    paired_atlas_texture(layout, container).map(drop)
}

fn paired_atlas_texture<'a>(
    layout: Option<&crate::AtlasLayout>,
    container: &'a [u8],
) -> Result<Option<saffron_vegetation::PlantTextureContainer<'a>>> {
    match (layout, container.is_empty()) {
        (None, true) => Ok(None),
        (Some(layout), false) => {
            let texture = saffron_vegetation::read_plant_texture_container(container)?;
            if texture.width != layout.width || texture.height != layout.height {
                return Err(Error::Io(format!(
                    "compiled plant atlas layout is {}x{} and its texture container is {}x{}",
                    layout.width, layout.height, texture.width, texture.height
                )));
            }
            // Every level below the base is sampled at distance, so a chain that stops short leaves
            // the sampler reading a level the family never cooked.
            if !texture.is_mip_complete() {
                return Err(Error::Io(
                    "compiled plant atlas texture container stops short of a 1x1 level".to_owned(),
                ));
            }
            Ok(Some(texture))
        }
        _ => Err(Error::Io(
            "compiled plant artifact carries one half of its family atlas".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{fixture_server, imported_family, options, save_family};
    use super::super::{PlantRecookOutcome, decode_plant_material_document, recook_plant_family};
    use super::decode_family_atlas;
    use saffron_vegetation::{
        PlantTextureContainer, PlantTextureFormat, write_plant_texture_container,
    };

    fn layout() -> crate::AtlasLayout {
        crate::AtlasLayout {
            width: 4,
            height: 4,
            gutter: 1,
            placements: vec![crate::AtlasPlacement {
                slot: 0,
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            }],
        }
    }

    fn container(width: u32, height: u32, levels: usize) -> Vec<u8> {
        let payloads: Vec<Vec<u8>> = (0..levels)
            .map(|level| {
                let extent = |value: u32| (value >> level).max(1) as usize;
                vec![u8::try_from(level + 1).unwrap(); extent(width) * extent(height) * 4]
            })
            .collect();
        write_plant_texture_container(&PlantTextureContainer {
            format: PlantTextureFormat::Rgba8Srgb,
            width,
            height,
            levels: payloads.iter().map(Vec::as_slice).collect(),
        })
        .expect("encode")
    }

    #[test]
    fn a_family_atlas_published_as_one_half_is_refused() {
        // The layout and the texels are one atlas carried by two sections. Each half is
        // well-formed on its own, so pairing them is the only place a disagreement shows: a
        // family sampling an atlas the cook never wrote renders untextured foliage with nothing
        // upstream reporting a fault.
        let paired = decode_family_atlas(Some(layout()), &container(4, 4, 3))
            .expect("the halves agree")
            .expect("an atlas");
        assert_eq!(paired.levels.len(), 3);
        assert_eq!(
            (paired.levels[1].width, paired.levels[1].height),
            (2, 2),
            "level extents derive from the base extent"
        );
        assert_eq!(paired.levels[2].rgba, vec![3; 4]);
        assert!(paired.format.is_srgb());

        assert!(
            decode_family_atlas(None, &[])
                .expect("no atlas is a valid family")
                .is_none()
        );
        assert!(
            decode_family_atlas(Some(layout()), &[]).is_err(),
            "a layout without texels"
        );
        assert!(
            decode_family_atlas(None, &container(4, 4, 3)).is_err(),
            "texels without a layout"
        );
        assert!(
            decode_family_atlas(
                Some(crate::AtlasLayout {
                    width: 8,
                    ..layout()
                }),
                &container(4, 4, 3)
            )
            .is_err(),
            "a layout addressing an extent the texels do not have"
        );
        assert!(
            decode_family_atlas(Some(layout()), &container(4, 4, 2)).is_err(),
            "a chain that stops before 1x1"
        );
    }

    #[test]
    fn render_decode_flattens_the_published_artifact_against_its_prototypes() {
        let (_scratch, mut assets, material, mesh) = fixture_server("render-decode");
        let family = save_family(&mut assets, imported_family(material, mesh));
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };
        let bytes = std::fs::read(&published.publication.path).expect("artifact");
        let decoded =
            crate::plant_render::decode_plant_render_sections(&bytes).expect("render decode");

        // The flat vertex stream is the prototypes' streams concatenated in id order.
        let vertex_total: usize = decoded
            .hierarchy
            .prototypes
            .iter()
            .map(|prototype| prototype.vertex_count as usize)
            .sum();
        assert_eq!(decoded.mesh.vertices.len(), vertex_total);
        assert!(!decoded.mesh.indices.is_empty());
        assert!(
            decoded
                .mesh
                .indices
                .iter()
                .all(|index| (*index as usize) < decoded.mesh.vertices.len()),
            "flattened indices are stream-global"
        );
        // Every prototype is placed by at least one use, and every use names a live
        // prototype — the assembly upload's contract.
        assert!(!decoded.hierarchy.micro_instances.is_empty());
        assert!(
            decoded
                .hierarchy
                .micro_instances
                .iter()
                .all(|instance| (instance.prototype as usize) < decoded.hierarchy.prototypes.len())
        );
        // The fixture family resolves one material slot; its pinned document decodes to
        // the resolved material asset.
        assert_eq!(decoded.material_slots, vec![material]);
        assert_eq!(decoded.material_documents.len(), 1);
        decode_plant_material_document(&decoded.material_documents[0].1)
            .expect("pinned material resolves");
    }
}
