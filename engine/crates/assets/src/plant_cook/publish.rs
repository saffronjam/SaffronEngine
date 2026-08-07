//! Assembly of the `.splantc` section set and the strict read-back that gates publication.

use saffron_core::Uuid;
use saffron_geometry::{
    VirtualHierarchyMaterial, calibrate_voxel_appearance_error,
    canonical_hierarchy_reference_fixtures, cook_portable_virtual_hierarchy,
};
use saffron_vegetation::{
    ContentHash, PlantCompileDiagnosticCode, PlantCompileOutput, PlantCompiledArtifactIndex,
    PlantCompiledSection, PlantCompiledSectionKind, PlantFamilyAsset, PlantSourceRole,
    plant_hierarchy_input,
};

use crate::{Error, Result};

use super::atlas::{atlas_normalized_family, derive_family_micromaps};
use super::decode::{decode_material_section, validate_family_atlas};
use super::distance_field::distance_field_section;
use super::materials::{ResolvedCoverageImages, ResolvedMaterialDocuments};
use super::sections::{
    collision_section, material_section, mesh_section, navigation_section, part_table_section,
    phenotype_section, provenance_section, skeleton_section, source_normalization_section,
    texture_container_section, validation_section,
};

/// Raster resolution the voxel appearance-error calibration renders each transition at. It costs
/// one render per voxel node per direction fixture, and the component a coarse resolution
/// under-reports — silhouette — is the one thin features fail on, so raising it only ever widens
/// the declared error further.
const VOXEL_CALIBRATION_RESOLUTION: u32 = 32;

/// The plant's local bounds-sphere top (`localBounds.y + localBounds.w`), the height the wind
/// prepass scales its modes by. Derived from the hierarchy about to be published rather than from
/// the authored dimensions, which describe the source before normalization.
fn hierarchy_top_height(hierarchy: &saffron_geometry::PortableVirtualHierarchy) -> f32 {
    let mut minimum = [i64::MAX; 3];
    let mut maximum = [i64::MIN; 3];
    for node in &hierarchy.nodes {
        for axis in 0..3 {
            minimum[axis] = minimum[axis].min(i64::from(node.bounds.min_bits[axis]));
            maximum[axis] = maximum[axis].max(i64::from(node.bounds.max_bits[axis]));
        }
    }
    if minimum[0] > maximum[0] {
        return 0.0;
    }
    let radius: [f32; 3] =
        std::array::from_fn(|axis| (maximum[axis] - minimum[axis]) as f32 / 65_536.0 * 0.5);
    let center_y = (minimum[1] + maximum[1]) as f32 / 65_536.0 * 0.5;
    center_y + radius.iter().map(|value| value * value).sum::<f32>().sqrt()
}

/// The wind modes an aggregate voxel drops, at the amplitude the runtime can never exceed. The
/// prepass clamps the wind term — `min(speed * 0.02, 0.5)` for the branch mode,
/// `min(speed * 0.012, 0.2)` for flutter — before the authored response scales it, so the largest
/// either can reach is a property of the family alone. Takes the PACKED words because those are the
/// bytes the GPU reads, including the all-zero case that means no authored response.
fn modal_aggregation_bound(
    mechanics: [u32; 4],
    top_height: f32,
) -> saffron_geometry::ModalAggregationBound {
    let authored = mechanics != [0; 4];
    let scalar = |word: u32| word as i32 as f32 / 65_536.0;
    let stiffness = if authored {
        scalar(mechanics[0]).max(0.05)
    } else {
        1.0
    };
    let drag = if authored { scalar(mechanics[1]) } else { 1.0 };
    let flutter = if authored { scalar(mechanics[2]) } else { 1.0 };
    let settle = if authored {
        1.0 - 0.5 * ((mechanics[3] & 0xffff) as f32 / 65_535.0)
    } else {
        1.0
    };
    // A zero authored limit is unlimited, exactly as the prepass reads it: a plant that may not
    // bend at all is a prop rather than a bend limit.
    let limit = (mechanics[3] >> 16) as f32 / 65_535.0;
    let bend_limit = if authored && limit > 0.0 {
        limit * 2.0
    } else {
        f32::INFINITY
    };
    let height = top_height.max(0.5);
    saffron_geometry::ModalAggregationBound {
        branch: (0.5 * (height * 0.25).min(1.0) * drag * settle / stiffness).min(bend_limit),
        flutter: (0.2 * flutter * settle).min(bend_limit),
    }
}

pub(super) fn build_plant_sections(
    asset: &PlantFamilyAsset,
    compile: &PlantCompileOutput,
    material_documents: &ResolvedMaterialDocuments,
    hierarchy_materials: &[VirtualHierarchyMaterial],
    coverage_images: &ResolvedCoverageImages,
) -> Result<Vec<PlantCompiledSection>> {
    let mut family = compile
        .family
        .as_ref()
        .ok_or_else(|| Error::Io("plant compiler produced no publishable family".to_owned()))?
        .clone();
    // Pack the family's coverage into one atlas and rewrite its UVs to address it, before anything
    // downstream reads them. Every section then describes ONE family — the atlased one — rather
    // than the geometry describing atlas space while the normalization record describes the
    // compiler's slot-local output.
    let atlas = atlas_normalized_family(&mut family, coverage_images);
    let family = &family;
    let mut accepted_compile = compile.clone();
    accepted_compile.family = Some(family.clone());
    accepted_compile.source_updates.clear();
    accepted_compile
        .diagnostics
        .retain(|diagnostic| diagnostic.code != PlantCompileDiagnosticCode::SourceChanged);
    let hierarchy_input = plant_hierarchy_input(asset, family, hierarchy_materials)?;
    let mut hierarchy = cook_portable_virtual_hierarchy(&hierarchy_input)?;
    // The cooker's analytic appearance error guesses low for thin separated features — a brick
    // fills the gaps a comb of blades leaves — and the cut selector trusts that number to decide
    // when a voxel brick may stand in for triangles, so a low one swaps early and pops. The
    // measurement also covers what aggregating takes away: a brick has no parts, so it applies
    // neither the per-use swing nor the leaf shimmer, and distant vegetation moves less than near
    // vegetation by a bound the declared error has to cover.
    let modal = modal_aggregation_bound(
        crate::gpu_scene_mirror::packed_mechanics(Some(asset.mechanics)),
        hierarchy_top_height(&hierarchy),
    );
    calibrate_voxel_appearance_error(
        &mut hierarchy,
        &canonical_hierarchy_reference_fixtures(),
        VOXEL_CALIBRATION_RESOLUTION,
        modal,
    )?;
    if let Some(atlas) = atlas.as_ref() {
        hierarchy.opacity_micromaps = derive_family_micromaps(
            &hierarchy,
            family,
            hierarchy_materials,
            material_documents,
            coverage_images,
            atlas,
        )?;
    }
    Ok(vec![
        PlantCompiledSection::new(
            PlantCompiledSectionKind::SourceNormalization,
            source_normalization_section(asset, compile, family),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::PartTable,
            part_table_section(asset),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Geometry,
            mesh_section(family, PlantSourceRole::Geometry, true),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::MaterialsCoverage,
            material_section(family, material_documents, atlas.as_ref()),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::SkeletonWeights,
            skeleton_section(asset, family),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Phenotypes,
            phenotype_section(asset),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Collision,
            collision_section(asset, family),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Navigation,
            navigation_section(asset, family),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Provenance,
            provenance_section(asset),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::TriangleHierarchy,
            hierarchy.triangle_hierarchy_bytes()?,
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::VoxelHierarchy,
            hierarchy.voxel_hierarchy_bytes()?,
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Deformation,
            hierarchy.deformation_bytes()?,
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::PageDirectory,
            hierarchy.page_directory_bytes()?,
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::RayTracing,
            hierarchy.ray_tracing_bytes()?,
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Validation,
            validation_section(&accepted_compile),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::DistanceField,
            distance_field_section(&hierarchy),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::TextureContainer,
            texture_container_section(atlas.as_ref())?,
        ),
    ])
}

pub(super) fn validate_complete_plant_artifact(
    bytes: &[u8],
    family: Uuid,
    cook_key: ContentHash,
    platform: ContentHash,
) -> Result<()> {
    let index = PlantCompiledArtifactIndex::open(
        bytes,
        saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
    )?;
    if index.family != family || index.cook_key != cook_key || index.platform_profile != platform {
        return Err(Error::Io(
            "compiled plant artifact header does not match its cook request".to_owned(),
        ));
    }
    // Parsing the response here means an unparseable part table fails the cook rather than
    // reaching the renderer, where the failure would read as a plant that will not sway.
    index.mechanical_response(bytes)?;
    // The atlas is two sections — where each slot landed, and the texels — and a family that
    // published one without the other samples texels the cook never placed. Both halves look
    // well-formed alone, so the disagreement has to be caught where they are still together.
    let materials = index
        .section(bytes, PlantCompiledSectionKind::MaterialsCoverage)?
        .ok_or_else(|| Error::Io("compiled plant artifact is missing its materials".to_owned()))?;
    let container = index
        .section(bytes, PlantCompiledSectionKind::TextureContainer)?
        .ok_or_else(|| {
            Error::Io("compiled plant artifact is missing its texture container".to_owned())
        })?;
    let (_, layout) = decode_material_section(materials.as_ref())?;
    validate_family_atlas(layout.as_ref(), container.as_ref())?;
    crate::vegetation_store::validate_plant_artifact(bytes)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        cook_family_with_coverage, fixture_server, fixture_server_with_material, imported_family,
        options, save_family, two_prototype_family,
    };
    use super::super::{PlantRecookOutcome, recook_plant_family};
    use super::*;
    use crate::MaterialAsset;
    use saffron_geometry::decode_portable_virtual_hierarchy_sections;
    use saffron_spatial::{DecisionScalar, UnitInterval};
    use saffron_vegetation::{
        MaterialSurface, PlantCompiledArtifactIndex, ThinSheetFoliageParameters,
        VoxelMaterialMoments,
    };
    use std::collections::BTreeSet;

    #[test]
    fn portable_hierarchy_round_trip_and_cut_are_hole_free() {
        let (_scratch, mut assets, material, mesh) = fixture_server("portable-hierarchy");
        let family = save_family(&mut assets, imported_family(material, mesh));
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };
        let bytes = std::fs::read(&published.publication.path).expect("artifact");
        let index = PlantCompiledArtifactIndex::open(
            &bytes,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .expect("artifact index");
        let section = |kind| {
            index
                .section(&bytes, kind)
                .expect("valid section")
                .expect("required section")
        };
        let triangle = section(PlantCompiledSectionKind::TriangleHierarchy);
        let voxel = section(PlantCompiledSectionKind::VoxelHierarchy);
        let deformation = section(PlantCompiledSectionKind::Deformation);
        let pages = section(PlantCompiledSectionKind::PageDirectory);
        let ray_tracing = section(PlantCompiledSectionKind::RayTracing);
        let hierarchy = decode_portable_virtual_hierarchy_sections(
            triangle.as_ref(),
            voxel.as_ref(),
            deformation.as_ref(),
            pages.as_ref(),
            ray_tracing.as_ref(),
        )
        .expect("portable hierarchy");
        assert!(!hierarchy.triangle_clusters.is_empty());
        assert!(!hierarchy.voxel_bricks.is_empty());
        assert!(hierarchy.pages.iter().all(|page| {
            page.dependency
                .is_none_or(|dependency| dependency < page.id)
        }));

        let root_pages = hierarchy
            .roots
            .iter()
            .map(|root| hierarchy.nodes[*root as usize].page)
            .collect::<BTreeSet<_>>();
        let coarse = saffron_geometry::select_portable_hierarchy_cut(&hierarchy, &root_pages, 0)
            .expect("coarse cut");
        assert_eq!(coarse, hierarchy.roots);

        let all_pages = hierarchy.pages.iter().map(|page| page.id).collect();
        let fine = saffron_geometry::select_portable_hierarchy_cut(&hierarchy, &all_pages, 0)
            .expect("fine cut");
        assert!(!fine.is_empty());
        assert!(fine.iter().all(|node| {
            hierarchy.nodes[*node as usize].children.is_empty()
                || hierarchy.nodes[*node as usize].appearance_error.total == 0
        }));
        assert_eq!(
            hierarchy.triangle_hierarchy_bytes().expect("re-encode"),
            triangle.as_ref()
        );
        assert_eq!(
            hierarchy.voxel_hierarchy_bytes().expect("re-encode"),
            voxel.as_ref()
        );

        let mut corrupt = pages.to_vec();
        corrupt.push(0);
        assert!(saffron_geometry::decode_page_directory(&corrupt).is_err());
    }

    /// The full runtime seam: a published two-prototype family loads from the artifact
    /// store into an assembly-carrying `GpuMesh`, registered under the family id in the
    /// shared mesh + page-payload caches with its material slot table. Skips when no
    /// Vulkan device is present.
    #[test]
    fn published_family_loads_as_an_assembly_mesh_under_the_family_id() {
        use saffron_rendering::{
            BindlessFreeList, Descriptors, Device, SurfaceSource, Uploader, validation_issue_count,
        };
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();
        let free_list: BindlessFreeList = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");

        let (_scratch, mut assets, material, first_mesh) = fixture_server("render-load");
        let two_prototype = two_prototype_family(&mut assets, material, first_mesh);
        let family = save_family(&mut assets, two_prototype);
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };

        let gpu = crate::gpu::RendererUploader::new(&uploader, &descriptors, false);
        let render = assets
            .load_plant_family(&gpu, family.id, published.publication.content_hash)
            .expect("family render");
        let assembly = render.mesh.assembly.as_ref().expect("assembly table");
        assert_eq!(
            assembly.prototypes.len(),
            2,
            "one prototype per source mesh"
        );
        assert!(assembly.uses.len() >= 2, "every prototype is placed");
        assert_eq!(assembly.prototypes[0].vertex_base, 0);
        assert_eq!(
            assembly.prototypes[1].vertex_base * 2,
            render.mesh.vertex_count,
            "two identical source meshes split the flattened stream evenly"
        );
        assert!(
            render.mesh.blas.is_none(),
            "an assembly carries no merged BLAS"
        );
        assert_eq!(render.materials.as_ref(), &[material]);

        // The family registers under its own id, so the mirror's mesh path resolves it.
        let registered = assets
            .load_mesh_asset(&gpu, family.id)
            .expect("family mesh resolves by id");
        assert!(std::sync::Arc::ptr_eq(&registered, &render.mesh));
        assert!(
            matches!(
                assets.page_payload_source(family.id),
                Some(crate::page_stream::PagePayloadSource::Cooked(_))
            ),
            "family pages stream from the retained cooked hierarchy"
        );

        device.wait_idle().expect("idle before teardown");
        assets.clear_asset_caches();
        drop(render);
        drop(registered);
        drop(assets);
        drop(uploader);
        drop(descriptors);
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn triangle_to_voxel_error_covers_thin_sheet_appearance() {
        let parameters = ThinSheetFoliageParameters {
            voxel_moments: VoxelMaterialMoments {
                occupancy: UnitInterval::from_bits(40_000),
                albedo_mean: [DecisionScalar::from_bits(20_000); 3],
                roughness_mean: UnitInterval::from_bits(30_000),
                transmission_mean: [DecisionScalar::from_bits(15_000); 3],
                thickness_mean: DecisionScalar::from_bits(400),
                normal_second_moments: [DecisionScalar::from_bits(12_000); 6],
            },
            ..ThinSheetFoliageParameters::default()
        };
        let material_asset = MaterialAsset {
            surface: MaterialSurface::ThinSheetFoliage(parameters),
            blend: "masked".to_owned(),
            double_sided: true,
            ..MaterialAsset::default()
        };
        let (_scratch, mut assets, material, mesh) =
            fixture_server_with_material("thin-sheet-error", &material_asset);
        let family = save_family(&mut assets, imported_family(material, mesh));
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };
        let bytes = std::fs::read(&published.publication.path).expect("artifact");
        let index = PlantCompiledArtifactIndex::open(
            &bytes,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .expect("artifact index");
        let triangle = index
            .section(&bytes, PlantCompiledSectionKind::TriangleHierarchy)
            .unwrap()
            .unwrap();
        let voxel = index
            .section(&bytes, PlantCompiledSectionKind::VoxelHierarchy)
            .unwrap()
            .unwrap();
        let deformation = index
            .section(&bytes, PlantCompiledSectionKind::Deformation)
            .unwrap()
            .unwrap();
        let pages = index
            .section(&bytes, PlantCompiledSectionKind::PageDirectory)
            .unwrap()
            .unwrap();
        let ray_tracing = index
            .section(&bytes, PlantCompiledSectionKind::RayTracing)
            .unwrap()
            .unwrap();
        let hierarchy = decode_portable_virtual_hierarchy_sections(
            triangle.as_ref(),
            voxel.as_ref(),
            deformation.as_ref(),
            pages.as_ref(),
            ray_tracing.as_ref(),
        )
        .expect("portable hierarchy");
        let root = &hierarchy.nodes[hierarchy.roots[0] as usize];
        assert!(root.appearance_error.silhouette > 0);
        assert!(root.appearance_error.coverage > 0);
        assert!(root.appearance_error.transmission > 0);
        assert!(root.appearance_error.material > 0);
        assert!(root.appearance_error.normal_distribution > 0);
        assert_eq!(
            hierarchy.ray_tracing[hierarchy.roots[0] as usize].material_class,
            saffron_geometry::VirtualMaterialClass::ThinSheet
        );
    }

    #[test]
    fn a_cooked_plant_declares_an_error_every_transition_fits_within() {
        // The claim the cut selector reads every frame: when a voxel brick stands in for its
        // triangle descendants, the declared appearance error covers how different it actually
        // looks. The cooker's analytic estimate does not guarantee that — it derives from bounds
        // and material moments and cannot see that a brick fills the gaps between thin separated
        // features, so it guesses low exactly where vegetation lives. The cook measures and widens.
        //
        // MUTATION-CHECKED: removing the `calibrate_voxel_appearance_error` call from
        // `build_plant_sections` fails this test with a named node, so it is testing the
        // calibration rather than the analytic estimate happening to be conservative.
        let parameters = ThinSheetFoliageParameters {
            voxel_moments: VoxelMaterialMoments {
                occupancy: UnitInterval::from_bits(40_000),
                albedo_mean: [DecisionScalar::from_bits(20_000); 3],
                roughness_mean: UnitInterval::from_bits(30_000),
                transmission_mean: [DecisionScalar::from_bits(15_000); 3],
                thickness_mean: DecisionScalar::from_bits(400),
                normal_second_moments: [DecisionScalar::from_bits(12_000); 6],
            },
            ..ThinSheetFoliageParameters::default()
        };
        let material_asset = MaterialAsset {
            surface: MaterialSurface::ThinSheetFoliage(parameters),
            blend: "masked".to_owned(),
            double_sided: true,
            ..MaterialAsset::default()
        };
        let (_scratch, mut assets, material, mesh) =
            fixture_server_with_material("transition-error", &material_asset);
        let family = save_family(&mut assets, imported_family(material, mesh));
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };
        let bytes = std::fs::read(&published.publication.path).expect("artifact");
        let index = PlantCompiledArtifactIndex::open(
            &bytes,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .expect("artifact index");
        let section = |kind| index.section(&bytes, kind).unwrap().unwrap();
        let triangle = section(PlantCompiledSectionKind::TriangleHierarchy);
        let voxel = section(PlantCompiledSectionKind::VoxelHierarchy);
        let deformation = section(PlantCompiledSectionKind::Deformation);
        let pages = section(PlantCompiledSectionKind::PageDirectory);
        let ray_tracing = section(PlantCompiledSectionKind::RayTracing);
        let hierarchy = decode_portable_virtual_hierarchy_sections(
            triangle.as_ref(),
            voxel.as_ref(),
            deformation.as_ref(),
            pages.as_ref(),
            ray_tracing.as_ref(),
        )
        .expect("portable hierarchy");

        // Re-measured with the SAME modal bound the cook calibrated against, which is the
        // second half of the claim: distant vegetation keeps the whole-plant sway and loses the
        // per-part modes, and the declared error has to cover that loss as well as the shape
        // difference. Measuring with a zero bound here would assert the easier statement.
        let modal = modal_aggregation_bound(
            crate::gpu_scene_mirror::packed_mechanics(Some(family.mechanics)),
            hierarchy_top_height(&hierarchy),
        );
        assert!(
            !modal.is_zero(),
            "the fixture family must author modes for the bound to mean anything"
        );
        let comparisons = saffron_geometry::compare_triangle_voxel_transitions(
            &hierarchy,
            &canonical_hierarchy_reference_fixtures(),
            VOXEL_CALIBRATION_RESOLUTION,
            modal,
        )
        .expect("the published hierarchy measures");
        // Without transitions there is nothing to be within anything, and the loop below would
        // pass over an empty set — the exact shape of a test that proves nothing.
        assert!(
            !comparisons.is_empty(),
            "the fixture must cook transitions to measure"
        );
        for comparison in &comparisons {
            assert!(
                comparison.is_within_declared_error(),
                "voxel node {} exceeds its declared error: measured {:?} against declared {:?}",
                comparison.voxel_node,
                comparison.measured,
                comparison.declared
            );
        }
    }

    #[test]
    fn the_published_hierarchy_reads_back_with_both_representations_and_their_errors() {
        // What the hierarchy view shows. It reads the PUBLISHED cut rather than re-cooking, because
        // the cut a view selects is chosen against the errors in these bytes — a freshly cooked
        // hierarchy would answer a question about a different plant than the one on screen.
        let (_scratch, assets, _bytes, hash) = cook_family_with_coverage("family-hierarchy");
        let hierarchy = crate::plant_family_hierarchy(&assets, hash).expect("hierarchy reads");
        assert!(!hierarchy.nodes.is_empty());

        let voxels = hierarchy
            .nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.representation,
                    saffron_geometry::HierarchyRepresentation::Voxel { .. }
                )
            })
            .count();
        // Thin foliage cooks an aggregate form; a hierarchy of triangles alone would mean the view
        // has no representation transition to show and the cut control nothing to move between.
        assert!(voxels > 0, "the family cooked an aggregate node");

        // Exactly one root, and every other node's parent is a real node — the shape the view
        // indents by, and a cycle or a dangling parent would hang a walker rather than mis-draw.
        let roots = hierarchy
            .nodes
            .iter()
            .filter(|node| node.parent.is_none())
            .count();
        assert_eq!(roots, 1);
        for node in &hierarchy.nodes {
            if let Some(parent) = node.parent {
                assert!((parent as usize) < hierarchy.nodes.len());
            }
            // The total is the saturating sum the selector compares, so it can never read below a
            // component — a view showing a total under its own silhouette error would be lying
            // about which node gets picked.
            assert!(node.appearance_error.total >= node.appearance_error.silhouette);
        }
    }
}
