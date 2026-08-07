//! The family coverage atlas: packing every slot's coverage image into one texture,
//! rewriting the family's UVs to address it, and deriving opacity micromaps against it.

use saffron_geometry::{PortableVirtualHierarchy, VirtualHierarchyMaterial, VirtualMaterialClass};
use saffron_vegetation::{
    AlphaClassification, MaterialSurface, NormalizedPlantFamily, NormalizedPlantSkin,
    NormalizedPlantVertex, PlantSourceRole,
};

use crate::Result;

use super::decode::decode_mesh_section;
use super::materials::{
    ResolvedCoverageImages, ResolvedMaterialDocuments, decode_plant_material_document,
};
use super::sections::mesh_section;

/// Largest edge a cooked family atlas may reach before packing refuses the set.
///
/// A family that cannot fit is not atlased at all rather than partially packed: a silently dropped
/// slot renders untextured, which reads as a material bug rather than the budget one it is.
const FAMILY_ATLAS_MAX_EDGE: u32 = 4096;

/// Texels of separation between packed slots, carried as edge colour at zero alpha.
///
/// Bilinear filtering reaches past a sub-rectangle's edge, so touching rectangles bleed — a leaf
/// carrying a sliver of bark, visible only at distance.
const FAMILY_ATLAS_GUTTER: u32 = crate::DEFAULT_ATLAS_GUTTER;

/// Derives one opacity micromap per non-opaque flattened submesh, against the atlas alpha plane.
///
/// A micromap may only ever REMOVE classifier work, so an absent or empty one is always correct and
/// any submesh may be skipped for any reason. It reads the ATLAS plane because the family's UVs
/// address the atlas by this point.
pub(super) fn derive_family_micromaps(
    hierarchy: &PortableVirtualHierarchy,
    family: &NormalizedPlantFamily,
    hierarchy_materials: &[VirtualHierarchyMaterial],
    material_documents: &ResolvedMaterialDocuments,
    coverage_images: &ResolvedCoverageImages,
    atlas: &crate::FamilyAtlas,
) -> Result<Vec<saffron_geometry::PortableOpacityMicromap>> {
    let rows = decode_mesh_section(&mesh_section(family, PlantSourceRole::Geometry, true))?;
    let flat = crate::plant_render::flatten_prototype_rows(hierarchy, &rows)?;
    let alpha = atlas.alpha_plane();
    let uvs: Vec<[f32; 2]> = flat
        .vertices
        .iter()
        .map(|vertex| [vertex.uv0.x, vertex.uv0.y])
        .collect();
    let mut micromaps = Vec::new();
    for (index, submesh) in flat.submeshes.iter().enumerate() {
        let Some(material) = hierarchy_materials
            .iter()
            .find(|entry| entry.slot == submesh.material_slot)
        else {
            continue;
        };
        // A fully covered surface commits hits without consulting the classifier, so there is no
        // per-micro-triangle work for a micromap to remove.
        if material.class.is_opaque() || !material.opacity_micromap {
            continue;
        }
        let Some(slot_material) = family.materials.get(submesh.material_slot as usize) else {
            continue;
        };
        let Some(coverage) = coverage_images.get(&slot_material.material.value()) else {
            continue;
        };
        let asset = material_documents
            .get(&slot_material.material.value())
            .and_then(|document| decode_plant_material_document(document).ok());
        // The atlas packs raw decoded texels, so the material's own base alpha has not been
        // folded in. Passing 1.0 would over-estimate coverage and could settle a micro-triangle
        // opaque that the shader renders cut out — the one way a micromap may change a pixel.
        let base_alpha = asset.as_ref().map_or(1.0, |asset| asset.base_color.w);
        // Thin-sheet foliage authors its own derivation policy; a plain masked surface has no
        // field to author one in, so it gets the conservative default. Authored thresholds
        // INTERSECT the provable set — they can only narrow it, never widen it — which is what
        // keeps "a micromap never changes correctness" a property of the code rather than of the
        // numbers someone typed.
        let policy = match asset.as_ref().map(|asset| &asset.surface) {
            Some(MaterialSurface::ThinSheetFoliage(parameters)) => parameters.opacity_micromap,
            _ => default_masked_micromap_policy(),
        };
        let (classification, _) = material_coverage_class(material.class);
        let rule = saffron_geometry::CoverageRule::new(
            classification,
            matches!(material.class, VirtualMaterialClass::ThinSheet),
            f32::from(coverage.cutoff) / 255.0,
        );
        let start = submesh.first_index as usize;
        let end = start.saturating_add(submesh.index_count as usize);
        let Some(indices) = flat.indices.get(start..end.min(flat.indices.len())) else {
            continue;
        };
        let build = saffron_geometry::derive_opacity_micromap(
            indices,
            &uvs,
            saffron_geometry::CoverageSourcePlane {
                alpha: &alpha,
                width: atlas.layout.width,
                height: atlas.layout.height,
                rule,
                base_alpha,
            },
            &policy,
        );
        if build.blocks.is_empty() {
            continue;
        }
        micromaps.push(saffron_geometry::PortableOpacityMicromap {
            submesh: u32::try_from(index).unwrap_or(u32::MAX),
            indices: build.indices,
            blocks: build
                .blocks
                .iter()
                .map(|block| (block.data_offset, block.subdivision_level, block.format))
                .collect(),
            data: build.data,
            usage: build
                .usage
                .iter()
                .map(|row| (row.count, row.subdivision_level, row.format))
                .collect(),
            classes: (
                build.classes.opaque,
                build.classes.transparent,
                build.classes.unknown,
            ),
        });
    }
    Ok(micromaps)
}

/// The derivation policy for a masked surface, which has no field to author one in.
///
/// Enabled with the widest thresholds, so the derivation is bounded only by what it can PROVE:
/// a micro-triangle settles opaque or transparent when the min/max pyramid over its dilated UV
/// footprint puts every point on one side of the cutoff, and stays unknown otherwise.
fn default_masked_micromap_policy() -> saffron_material::OpacityMicromapDerivation {
    saffron_material::OpacityMicromapDerivation {
        enabled: true,
        max_subdivision: 5,
        transparent_threshold: saffron_spatial::UnitInterval::from_bits(0),
        opaque_threshold: saffron_spatial::UnitInterval::from_bits(u16::MAX),
    }
}

/// The alpha classification a cooked material class implies, and whether it transmits.
const fn material_coverage_class(class: VirtualMaterialClass) -> (AlphaClassification, bool) {
    match class {
        VirtualMaterialClass::Opaque => (AlphaClassification::Opaque, false),
        VirtualMaterialClass::Transmissive => (AlphaClassification::Transmissive, true),
        VirtualMaterialClass::Masked | VirtualMaterialClass::ThinSheet => {
            (AlphaClassification::Masked, false)
        }
    }
}

/// Packs the family's coverage images into one atlas and rewrites its UVs to address it, or returns
/// `None` when there are no coverage images or the set does not fit [`FAMILY_ATLAS_MAX_EDGE`] — an
/// un-atlased family keeps slot-local UVs and binds slot-local textures, so the two never
/// half-apply.
///
/// MUST run after `apply_geometry_first_contours`, which re-tessellates alpha cards against
/// slot-local coverage and emits new UVs; remapping first leaves the contour addressing atlas space
/// with a slot-local alpha plane.
pub(super) fn atlas_normalized_family(
    family: &mut NormalizedPlantFamily,
    coverage_images: &ResolvedCoverageImages,
) -> Option<crate::FamilyAtlas> {
    let mut slots = Vec::new();
    for (slot, material) in family.materials.iter().enumerate() {
        let Some(image) = coverage_images.get(&material.material.value()) else {
            continue;
        };
        slots.push(crate::FamilySlotImage {
            slot: u32::try_from(slot).ok()?,
            width: image.width,
            height: image.height,
            rgba: image.rgba.clone(),
        });
    }
    if slots.is_empty() {
        return None;
    }
    // The cutoff the mip chain must preserve coverage against. Slots may declare different ones;
    // the lowest is the conservative choice, since a chain that preserves coverage at the lowest
    // cutoff preserves it at every higher one.
    let cutoff = family
        .materials
        .iter()
        .filter_map(|material| coverage_images.get(&material.material.value()))
        .map(|image| u16::from(image.cutoff) << 8)
        .min()
        .unwrap_or(u16::MAX / 2);
    let atlas =
        crate::generate_family_atlas(&slots, FAMILY_ATLAS_MAX_EDGE, FAMILY_ATLAS_GUTTER, cutoff)?;
    remap_family_uvs(family, &atlas.layout);
    Some(atlas)
}

/// Rewrites every vertex's UV from its slot's own image into atlas space. A vertex reached from two
/// submeshes with different material slots has no single answer, so it is DUPLICATED — one copy per
/// slot, with that slot's remap — and the offending indices rewritten.
fn remap_family_uvs(family: &mut NormalizedPlantFamily, layout: &crate::AtlasLayout) {
    for mesh in &mut family.meshes {
        // Slot claimed by each vertex, and the duplicate minted for any second claimant.
        let mut claimed = vec![u32::MAX; mesh.vertices.len()];
        let mut duplicates = std::collections::BTreeMap::<(u32, u32), u32>::new();
        let mut minted: Vec<NormalizedPlantVertex> = Vec::new();
        let mut minted_skin: Vec<NormalizedPlantSkin> = Vec::new();
        for submesh in &mesh.submeshes {
            let slot = submesh.material_slot;
            let start = submesh.first_index as usize;
            let end = start.saturating_add(submesh.index_count as usize);
            for position in start..end.min(mesh.indices.len()) {
                let vertex = mesh.indices[position];
                let Some(claim) = claimed.get_mut(vertex as usize) else {
                    continue;
                };
                if *claim == u32::MAX {
                    *claim = slot;
                } else if *claim != slot {
                    let next =
                        u32::try_from(mesh.vertices.len() + minted.len()).unwrap_or(u32::MAX);
                    let copy = *duplicates.entry((vertex, slot)).or_insert_with(|| {
                        minted.push(mesh.vertices[vertex as usize]);
                        if let Some(skin) = mesh.skin.get(vertex as usize) {
                            minted_skin.push(*skin);
                        }
                        next
                    });
                    mesh.indices[position] = copy;
                }
            }
        }
        let duplicate_slots: std::collections::BTreeMap<u32, u32> = duplicates
            .iter()
            .map(|(&(_, slot), &copy)| (copy, slot))
            .collect();
        let base = mesh.vertices.len();
        mesh.vertices.extend(minted);
        if !mesh.skin.is_empty() {
            mesh.skin.extend(minted_skin);
        }
        for (index, vertex) in mesh.vertices.iter_mut().enumerate() {
            let slot = if index < base {
                claimed[index]
            } else {
                duplicate_slots
                    .get(&u32::try_from(index).unwrap_or(u32::MAX))
                    .copied()
                    .unwrap_or(u32::MAX)
            };
            let Some(placement) = layout.placement(slot) else {
                continue;
            };
            let uv = [q16_to_f32(vertex.uv_bits[0]), q16_to_f32(vertex.uv_bits[1])];
            let remapped = placement.remap(uv, layout.width, layout.height);
            vertex.uv_bits = [f32_to_q16(remapped[0]), f32_to_q16(remapped[1])];
        }
    }
}

/// Q15.16 fixed point to float, the canonical UV encoding both cook consumers read.
fn q16_to_f32(bits: i32) -> f32 {
    bits as f32 / 65_536.0
}

/// Float to Q15.16, rounding to nearest so a remap round-trips within one ulp of the grid.
fn f32_to_q16(value: f32) -> i32 {
    (value * 65_536.0).round() as i32
}

#[cfg(test)]
mod tests {
    use super::super::decode::{decode_material_section, decode_mesh_section};
    use super::super::test_support::cook_family_with_coverage;
    use super::*;
    use saffron_vegetation::{PlantCompiledArtifactIndex, PlantCompiledSectionKind};

    #[test]
    fn the_published_atlas_reads_back_as_an_image_its_placements_fit_inside() {
        // The inspection surface the Plant workspace shows. It reads the PUBLISHED artifact rather
        // than re-packing: the family's UVs address exactly one layout, and a second packing of the
        // same slots would produce a different one — an atlas view that did that would show an
        // image the plant is not sampling.
        let (_scratch, assets, _bytes, hash) = cook_family_with_coverage("family-atlas-image");
        let image = crate::plant_family_atlas_image(&assets, hash, 0)
            .expect("atlas reads")
            .expect("a family with a coverage texture is atlased");
        assert!(image.level_count > 1, "the chain reached the view");
        assert!(image.width > 0 && image.height > 0);
        // Decodes as a real PNG at the extent it reports, which is what a viewer needs and what a
        // raw-bytes reply could get wrong without anything noticing.
        let decoded = image::load_from_memory(&image.png).expect("the reply decodes as PNG");
        assert_eq!(decoded.width(), image.width);
        assert_eq!(decoded.height(), image.height);
        // Every placement lands inside the atlas it claims to be in — a placement that did not is
        // how a slot samples a neighbour's texels.
        assert!(!image.placements.is_empty());
        for placement in &image.placements {
            assert!(placement.x + placement.width <= image.width);
            assert!(placement.y + placement.height <= image.height);
        }

        // A smaller level is what the renderer samples at distance, so it has to be reachable and
        // has to be smaller.
        let mip = crate::plant_family_atlas_image(&assets, hash, 1)
            .expect("level 1 reads")
            .expect("atlased");
        assert!(mip.width < image.width || mip.height < image.height);
        // And a level past the chain is an error rather than a silent clamp to the last one.
        assert!(crate::plant_family_atlas_image(&assets, hash, image.level_count).is_err());
    }

    #[test]
    fn a_cooked_family_carries_its_atlas_and_uvs_that_address_it() {
        // The atlas and the UVs are one decision, not two. A family that ships a packed atlas but
        // slot-local UVs — or the reverse — samples texels the cook never placed there, and
        // nothing downstream can detect the disagreement because both halves look well-formed.
        // So this asserts they agree, on a real cooked artifact.
        let (_scratch, _assets, bytes, _hash) = cook_family_with_coverage("family-atlas");
        let index = PlantCompiledArtifactIndex::open(
            &bytes,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .expect("artifact index");
        let section = index
            .section(&bytes, PlantCompiledSectionKind::MaterialsCoverage)
            .unwrap()
            .unwrap();
        let (materials, layout) = decode_material_section(section.as_ref()).expect("materials");
        assert!(!materials.is_empty(), "the family binds a material");
        let layout = layout.expect("a family with a coverage texture is atlased");
        let container = index
            .section(&bytes, PlantCompiledSectionKind::TextureContainer)
            .unwrap()
            .unwrap();
        assert_eq!(
            &container[..12],
            &[
                0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A
            ],
            "the texture section is a KTX2 container"
        );
        let atlas = super::super::decode::decode_family_atlas(Some(layout), container.as_ref())
            .expect("the layout and the container pair")
            .expect("a family with a coverage texture ships texels");
        assert!(
            atlas.format.is_srgb(),
            "the packed colour keeps its authored encoding"
        );
        // The layout section carries material rows and rectangles, never texels: a second copy of
        // the chain could disagree with the container and nothing would catch it.
        assert!(
            section.len() < atlas.levels[0].rgba.len(),
            "the materials section is {} bytes against a {} byte level 0",
            section.len(),
            atlas.levels[0].rgba.len()
        );

        // A layout with no placement for slot 0 would leave that slot's UVs unremapped while the
        // artifact still claimed an atlas — the half-applied state this guards against.
        let placement = atlas
            .layout
            .placement(0)
            .expect("the one material slot has a placement");
        assert!(placement.width > 0 && placement.height > 0);
        assert!(placement.x + placement.width <= atlas.layout.width);
        assert!(placement.y + placement.height <= atlas.layout.height);
        // Level 0 plus a full chain down to 1x1; a single level would mean the coverage-preserving
        // mips never ran and distant foliage would thin out.
        assert!(atlas.levels.len() > 1, "the atlas carries a mip chain");
        assert_eq!(atlas.levels[0].width, atlas.layout.width);
        assert_eq!(atlas.levels.last().map(|level| level.width), Some(1));
        // The container's level 0 is the composited atlas, not some other image: the cutout fixture
        // is opaque inside the slot and the gutter carries no coverage.
        let texel = |x: u32, y: u32| {
            let at = (y as usize * atlas.levels[0].width as usize + x as usize) * 4;
            atlas.levels[0].rgba[at + 3]
        };
        assert_eq!(texel(placement.x, placement.y), 255);
        assert_eq!(texel(placement.x - 1, placement.y), 0);

        let rows = decode_mesh_section(
            index
                .section(&bytes, PlantCompiledSectionKind::Geometry)
                .unwrap()
                .unwrap()
                .as_ref(),
        )
        .expect("geometry");
        let mut sampled = 0_usize;
        for row in &rows {
            for vertex in &row.vertices {
                sampled += 1;
                let uv = [q16_to_f32(vertex.uv_bits[0]), q16_to_f32(vertex.uv_bits[1])];
                let u = uv[0] * atlas.layout.width as f32;
                let v = uv[1] * atlas.layout.height as f32;
                // Atlas-space UVs land inside the slot's own rectangle. Slot-local UVs would span
                // the whole atlas instead, which is what this catches.
                assert!(
                    u >= placement.x as f32 - 1.0
                        && u <= (placement.x + placement.width) as f32 + 1.0
                        && v >= placement.y as f32 - 1.0
                        && v <= (placement.y + placement.height) as f32 + 1.0,
                    "uv {uv:?} is outside slot 0's rectangle {placement:?}"
                );
            }
        }
        assert!(sampled > 0, "the family cooked vertices to check");
    }

    #[test]
    fn a_cooked_family_derives_micromaps_that_only_ever_remove_work() {
        // The derivation's whole licence is that it can only REMOVE classifier work: a
        // micro-triangle settles opaque or transparent only where a min/max pyramid over its
        // dilated UV footprint proves every point classifies that way, and anything else stays
        // unknown, where the classifier still runs. So the two assertions that matter are that
        // micromaps are produced at all — an empty set passes every "is it correct" check
        // vacuously — and that what they settled is internally consistent.
        let (_scratch, _assets, bytes, _hash) = cook_family_with_coverage("family-micromaps");
        let index = PlantCompiledArtifactIndex::open(
            &bytes,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .expect("artifact index");
        let section = index
            .section(&bytes, PlantCompiledSectionKind::RayTracing)
            .unwrap()
            .unwrap();
        let ray_tracing =
            saffron_geometry::decode_ray_tracing(section.as_ref()).expect("ray tracing section");
        assert!(
            !ray_tracing.opacity_micromaps.is_empty(),
            "a masked family with a coverage texture must derive at least one micromap"
        );
        for micromap in &ray_tracing.opacity_micromaps {
            let (opaque, transparent, unknown) = micromap.classes;
            assert!(
                opaque + transparent + unknown > 0,
                "a stored micromap classified no micro-triangles"
            );
            // Every usage row must account for triangles that exist, and the block a
            // non-negative index names must be present — a dangling index is the failure the
            // build turns into device loss rather than a validation message.
            let counted: u32 = micromap.usage.iter().map(|&(count, ..)| count).sum();
            assert!(counted as usize <= micromap.indices.len());
            for &block in &micromap.indices {
                if block >= 0 {
                    assert!(
                        (block as usize) < micromap.blocks.len(),
                        "index {block} names no block"
                    );
                }
            }
        }
    }
}
