//! Resolution of an on-disk model or USD stage into one plant source contribution.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use saffron_core::Uuid;
use saffron_geometry::glam::Mat4;
use saffron_geometry::{
    ImportedMaterial, ImportedModel, ImportedNode, sub_id_for, translate_model,
};
use saffron_vegetation::{
    MaterialSurface, PlantCompileDiagnostic, PlantCompileDiagnosticCode,
    PlantCompileDiagnosticSeverity, PlantFamilyAsset, PlantFamilySource, PlantSemanticDestination,
    PlantSourceJointSnapshot, PlantSourceLocator, PlantSourceMaterialSnapshot,
    PlantSourceMeshSnapshot, PlantSourceReference, PlantSourceRole, PlantSourceSelector,
    PlantSourceSnapshot, vegetation_content_hash,
};

use crate::cook_reader::CookAssetAccess;
use crate::model::imported_node_world_transforms;
use crate::spawn::imported_skin_from_json;
use crate::{Error, Result};

use super::materials::{
    ResolvedCoverageImages, imported_material_coverage, imported_material_coverage_image,
    imported_material_document,
};
use super::sources::{ResolvedPlantSource, source_snapshot_hash};

pub(super) fn resolve_file_source(
    assets: &dyn CookAssetAccess,
    asset: &PlantFamilyAsset,
    uri: &str,
) -> Result<ResolvedPlantSource> {
    let path = source_file_path(assets.root(), uri)?;
    let bytes = assets.read_file(&path)?;
    // USD carries a plant's structure in `UsdSkel` prims rather than in a model graph, and the
    // model importers do not read it at all. Routing it here — rather than teaching `translate_model`
    // a fourth format — keeps the skeleton reader the single truth about that file.
    let usd = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let extension = extension.to_ascii_lowercase();
            extension == "usd" || extension == "usda"
        });
    if usd {
        return resolve_usd_skeleton_source(uri, &bytes);
    }
    let graph = translate_model(&path)?;
    resolve_imported_model(asset, uri, &path, graph)
}

/// Resolves a USD stage into a skeleton-only source contribution: its joints become the family's
/// spine, and the meshes a plant renders come from its geometry sources. A stage with no skeleton is
/// an error rather than an empty joint list, which would surface later as a plant that will not
/// bend.
fn resolve_usd_skeleton_source(uri: &str, bytes: &[u8]) -> Result<ResolvedPlantSource> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        Error::Io("USD plant source is not UTF-8 (binary .usdc is unsupported)".to_owned())
    })?;
    let (skeletons, unsupported) = saffron_vegetation::read_usd_skeletons(text)
        .map_err(|err| Error::Io(format!("USD plant source: {err}")))?;
    let Some(skeleton) = skeletons.into_iter().next() else {
        return Err(Error::Io(format!(
            "USD plant source '{uri}' declares no UsdSkel skeleton"
        )));
    };
    // A joint's selector is its authored path token, which is the identity USD itself uses — so a
    // re-import against an edited stage matches joints by name rather than by position, and adding
    // a joint does not silently re-parent every one after it.
    let selector_for = |path: &str| PlantSourceSelector::Element {
        id: u128::from(sub_id_for(uri, "joint", path, 0).value()),
        path: path.to_owned(),
    };
    let joints = skeleton
        .joints
        .iter()
        .map(|joint| PlantSourceJointSnapshot {
            selector: selector_for(&joint.path),
            parent: joint
                .parent
                .and_then(|index| skeleton.joints.get(index))
                .map(|parent| selector_for(&parent.path)),
            rest_transform: usd_row_major_transform(joint.rest),
        })
        .collect();
    let mut snapshot = PlantSourceSnapshot {
        source: 0,
        content_hash: [0; 32],
        meshes: Vec::new(),
        materials: Vec::new(),
        joints,
        semantic_elements: Vec::new(),
    };
    snapshot.content_hash = source_snapshot_hash(&snapshot);
    // Attributes the reader could not express are reported rather than dropped: a stage carrying
    // a custom plant schema should say so, not import as if it had none.
    if !unsupported.is_empty() {
        tracing::info!(
            "USD plant source '{uri}': {} unsupported attribute(s): {}",
            unsupported.len(),
            unsupported.join(", ")
        );
    }
    Ok(ResolvedPlantSource {
        origin: saffron_geometry::ImportedOrigin::default(),
        snapshot,
        material_documents: BTreeMap::new(),
        coverage_images: ResolvedCoverageImages::new(),
    })
}

/// Converts a USD `matrix4d` into a `Mat4`. USD is row-major with row vectors (translation in the
/// last ROW); `Mat4` is column-major with column vectors (translation in the last COLUMN), so the
/// conversion is a transpose — which reading row-major storage as column-major already performs.
/// Permuting the values here would transpose twice.
fn usd_row_major_transform(rows: [f64; 16]) -> Mat4 {
    Mat4::from_cols_array(&rows.map(|value| value as f32))
}

fn resolve_imported_model(
    asset: &PlantFamilyAsset,
    uri: &str,
    path: &Path,
    graph: ImportedModel,
) -> Result<ResolvedPlantSource> {
    let origin = graph.origin.clone();
    let model_key = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Io("plant source file has no UTF-8 stem".to_owned()))?;
    let world = imported_node_world_transforms(&graph.nodes)?;
    let paths = node_paths(&graph.nodes)?;
    let material_selectors = graph
        .materials
        .iter()
        .enumerate()
        .map(|(index, material)| PlantSourceSelector::Element {
            id: u128::from(
                sub_id_for(
                    model_key,
                    "material",
                    &material_name(material, index),
                    index as u32,
                )
                .value(),
            ),
            path: format!("materials/{}", material_name(material, index)),
        })
        .collect::<Vec<_>>();
    let material_ids = material_selectors
        .iter()
        .map(|selector| mapped_file_material(asset, uri, selector))
        .collect::<Result<Vec<_>>>()?;
    let mut materials = Vec::new();
    let mut material_documents = BTreeMap::new();
    let mut coverage_images = BTreeMap::new();
    for ((source_material, selector), material) in graph
        .materials
        .iter()
        .zip(&material_selectors)
        .zip(&material_ids)
    {
        let document = imported_material_document(source_material);
        let (classification, coverage_source) = imported_material_coverage(source_material);
        materials.push(PlantSourceMaterialSnapshot {
            selector: selector.clone(),
            material: *material,
            content_hash: vegetation_content_hash(&document),
            surface: MaterialSurface::Standard,
            alpha_classification: classification,
            coverage_source,
        });
        if let Some(image) = imported_material_coverage_image(source_material)? {
            coverage_images.insert(material.value(), image);
        }
        material_documents.insert(material.value(), document);
    }
    let mut meshes = Vec::new();
    let skin_node = graph
        .skin
        .as_ref()
        .map(|skin| skin.desc.mesh_node)
        .unwrap_or(-1);
    for (index, node) in graph.nodes.iter().enumerate() {
        let Some(mesh) = node.mesh.clone() else {
            continue;
        };
        let mesh_id = sub_id_for(model_key, "mesh", &node.name, index as u32);
        let skin = if index as i32 == skin_node {
            graph
                .skin
                .as_ref()
                .map(|skin| skin.stream.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        meshes.push(PlantSourceMeshSnapshot {
            selector: PlantSourceSelector::Element {
                id: u128::from(mesh_id.value()),
                path: paths[index].clone(),
            },
            transform: world[index],
            mesh,
            skin,
            material_slots: material_ids.clone(),
        });
    }
    let joints = match &graph.skin {
        Some(skin) => imported_joints(model_key, &graph.nodes, &world, &skin.desc.joints)?,
        None => Vec::new(),
    };
    let mut snapshot = PlantSourceSnapshot {
        source: 0,
        content_hash: [0; 32],
        meshes,
        materials,
        joints,
        semantic_elements: Vec::new(),
    };
    snapshot.content_hash = source_snapshot_hash(&snapshot);
    Ok(ResolvedPlantSource {
        origin,
        snapshot,
        material_documents,
        coverage_images,
    })
}

pub(super) fn catalog_joints(
    model_key: &str,
    nodes: &[ImportedNode],
    world: &[Mat4],
    skin: &saffron_json::Value,
) -> Result<Vec<saffron_vegetation::PlantSourceJointSnapshot>> {
    let skin = imported_skin_from_json(skin);
    imported_joints(model_key, nodes, world, &skin.joints)
}

fn imported_joints(
    model_key: &str,
    nodes: &[ImportedNode],
    world: &[Mat4],
    joints: &[i32],
) -> Result<Vec<saffron_vegetation::PlantSourceJointSnapshot>> {
    let joint_nodes = joints
        .iter()
        .map(|joint| {
            usize::try_from(*joint)
                .ok()
                .filter(|index| *index < nodes.len())
                .ok_or_else(|| Error::Io("skin joint index is out of range".to_owned()))
        })
        .collect::<Result<Vec<_>>>()?;
    let selectors = joint_nodes
        .iter()
        .map(|index| joint_selector(model_key, &nodes[*index], *index))
        .collect::<Vec<_>>();
    let selector_by_node = joint_nodes
        .iter()
        .copied()
        .zip(selectors.iter().cloned())
        .collect::<BTreeMap<_, _>>();
    joint_nodes
        .iter()
        .zip(selectors)
        .map(|(node, selector)| {
            let parent = usize::try_from(nodes[*node].parent)
                .ok()
                .and_then(|parent| selector_by_node.get(&parent).cloned());
            Ok(saffron_vegetation::PlantSourceJointSnapshot {
                selector,
                parent,
                rest_transform: *world
                    .get(*node)
                    .ok_or_else(|| Error::Io("skin joint transform is missing".to_owned()))?,
            })
        })
        .collect()
}

fn joint_selector(model_key: &str, node: &ImportedNode, index: usize) -> PlantSourceSelector {
    PlantSourceSelector::Element {
        id: u128::from(sub_id_for(model_key, "joint", &node.name, index as u32).value()),
        path: format!("joints/{}", node_component(node, index)),
    }
}

pub(super) fn node_paths(nodes: &[ImportedNode]) -> Result<Vec<String>> {
    let mut paths = Vec::with_capacity(nodes.len());
    for index in 0..nodes.len() {
        let mut components = vec![node_component(&nodes[index], index)];
        let mut parent = nodes[index].parent;
        let mut active = BTreeSet::new();
        active.insert(index);
        while parent >= 0 {
            let parent_index = usize::try_from(parent)
                .map_err(|_| Error::Io("model node parent is invalid".to_owned()))?;
            let node = nodes
                .get(parent_index)
                .ok_or_else(|| Error::Io("model node parent is out of range".to_owned()))?;
            if !active.insert(parent_index) {
                return Err(Error::Io(
                    "model node hierarchy contains a cycle".to_owned(),
                ));
            }
            components.push(node_component(node, parent_index));
            parent = node.parent;
        }
        components.reverse();
        paths.push(components.join("/"));
    }
    Ok(paths)
}

fn node_component(node: &ImportedNode, index: usize) -> String {
    if node.name.trim().is_empty() {
        format!("node_{index}")
    } else {
        node.name.clone()
    }
}

fn source_file_path(root: &Path, uri: &str) -> Result<PathBuf> {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !matches!(extension.as_str(), "gltf" | "glb" | "obj" | "usd" | "usda") {
        return Err(Error::Io(
            "plant file source must be a .gltf, .glb, .obj, .usd, or .usda".to_owned(),
        ));
    }
    if !path.is_file() {
        return Err(Error::Io(format!(
            "plant source '{}' is not a file",
            path.display()
        )));
    }
    Ok(path)
}

fn mapped_file_material(
    asset: &PlantFamilyAsset,
    uri: &str,
    selector: &PlantSourceSelector,
) -> Result<Uuid> {
    let PlantFamilySource::Imported(recipe) = &asset.source else {
        return Err(Error::Io("plant family is not imported".to_owned()));
    };
    let mut slots = BTreeSet::new();
    for source in recipe.sources.iter().filter(|source| {
        source.role == PlantSourceRole::Material
            && source.locator == PlantSourceLocator::File(uri.to_owned())
    }) {
        for target in recipe.semantic_targets.iter().filter(|target| {
            target.source == source.id && selectors_match(&target.selector, selector)
        }) {
            if let PlantSemanticDestination::MaterialSlot(slot) = target.destination {
                slots.insert(slot);
            }
        }
    }
    if slots.len() != 1 {
        return Err(Error::Io(format!(
            "file material selector {selector:?} requires exactly one material-slot target"
        )));
    }
    let slot = slots
        .first()
        .copied()
        .ok_or_else(|| Error::Io("file material target is missing".to_owned()))?
        as usize;
    asset
        .material_slots
        .get(slot)
        .copied()
        .ok_or_else(|| Error::Io("file material target is out of range".to_owned()))
}

fn selectors_match(first: &PlantSourceSelector, second: &PlantSourceSelector) -> bool {
    match (first, second) {
        (PlantSourceSelector::Whole, PlantSourceSelector::Whole) => true,
        (
            PlantSourceSelector::Element { id: first, .. },
            PlantSourceSelector::Element { id: second, .. },
        ) => first == second,
        (
            PlantSourceSelector::Submesh {
                element: first_element,
                index: first_index,
            },
            PlantSourceSelector::Submesh {
                element: second_element,
                index: second_index,
            },
        ) => first_element == second_element && first_index == second_index,
        _ => false,
    }
}

fn material_name(material: &ImportedMaterial, index: usize) -> String {
    if material.name.trim().is_empty() {
        format!("material_{index}")
    } else {
        material.name.clone()
    }
}

/// Reports what a source file states about its own origin when the authored provenance does not
/// already carry it. The statement is surfaced verbatim rather than folded into the authored
/// provenance, which would put a legal claim nobody authored into the artifact.
pub(super) fn attribution_notice(
    source: &PlantSourceReference,
    origin: &saffron_geometry::ImportedOrigin,
) -> Option<PlantCompileDiagnostic> {
    if origin.generator.is_empty() && origin.copyright.is_empty() {
        return None;
    }
    let stated = !origin.copyright.is_empty();
    let recorded = !source.provenance.attribution.trim().is_empty();
    if recorded || !stated {
        return None;
    }
    Some(PlantCompileDiagnostic {
        severity: PlantCompileDiagnosticSeverity::Warning,
        code: PlantCompileDiagnosticCode::MissingSource,
        source: Some(source.id),
        selector: Some(source.selector.clone()),
        path: "source.imported.sources.provenance.attribution".to_owned(),
        message: format!(
            "source file states copyright '{}' from generator '{}' and the plant source records no attribution",
            origin.copyright, origin.generator
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{Scratch, base_family, options, provenance};
    use super::super::{PlantRecookOutcome, recook_plant_family, validate_plant_family_sources};
    use super::*;
    use saffron_core::Uuid;
    use saffron_vegetation::{
        ImportedPlantFamilyRecipe, PlantCompileDiagnosticSeverity, PlantCompileLimits,
        PlantImportSettings, PlantManualSemanticTarget, PlantPart, PlantPartSemantic, PlantPivot,
        PlantSemanticDestination, PlantSourceLocator, PlantSourceReference, PlantSourceRole,
        PlantSourceSelector,
    };

    /// A minimal USD stage carrying one skeleton, matching the reader's own fixture shape.
    const USD_SKEL_STAGE: &str = r#"#usda 1.0
(
    upAxis = "Y"
)

def SkelRoot "BirchRig"
{
    def Skeleton "Birch"
    {
        uniform token[] joints = ["Root", "Root/Trunk", "Root/TrunkGuard", "Root/Trunk/Branch"]
        uniform matrix4d[] restTransforms = [
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1) ),
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 2, 0, 1) ),
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (1, 2, 0, 1) ),
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 4, 0, 1) )
        ]
    }
}
"#;

    #[test]
    fn a_usd_stage_resolves_as_a_skeleton_source_input() {
        // A USD stage is a SOURCE INPUT, not merely something inspectable: a recipe names the
        // file and gets a spine out of it, which is what lets a plant bend the way its author
        // rigged it.
        let resolved =
            resolve_usd_skeleton_source("rig.usda", USD_SKEL_STAGE.as_bytes()).expect("resolves");
        let joints = &resolved.snapshot.joints;
        assert_eq!(joints.len(), 4);
        // A USD source contributes STRUCTURE only — the meshes a plant renders come from its
        // geometry sources, and inventing an empty mesh here would give the compiler a source that
        // claims to draw nothing rather than one that claims not to draw.
        assert!(resolved.snapshot.meshes.is_empty());
        assert!(resolved.snapshot.materials.is_empty());

        // Parentage survives the translation, including the trap the reader guards: `Root/Trunk`
        // is a string prefix of `Root/TrunkGuard`, and hanging the guard off the trunk would bend
        // geometry the wrong way while passing every count and length check.
        assert_eq!(joints[0].parent, None);
        assert_eq!(joints[1].parent.as_ref(), Some(&joints[0].selector));
        assert_eq!(joints[2].parent.as_ref(), Some(&joints[0].selector));
        assert_eq!(joints[3].parent.as_ref(), Some(&joints[1].selector));

        // USD writes `matrix4d` row-major and `Mat4` is column-major, so a translation that
        // survives the transpose is the check that the two conventions were actually reconciled
        // rather than copied across.
        assert_eq!(joints[1].rest_transform.col(3).truncate().y, 2.0);
        assert_eq!(joints[3].rest_transform.col(3).truncate().y, 4.0);
        assert_eq!(joints[2].rest_transform.col(3).truncate().x, 1.0);

        // Identity by selector, not by position: re-importing an edited stage must match joints by
        // the path USD itself uses, or inserting one joint silently re-parents every later one.
        let PlantSourceSelector::Element { path, .. } = &joints[1].selector else {
            panic!("a joint selector is an addressable element");
        };
        assert_eq!(path, "Root/Trunk");
    }

    #[test]
    fn a_usd_stage_with_no_skeleton_is_refused_rather_than_imported_empty() {
        // A recipe naming a file with no skeleton in it is a mistake. Returning an empty joint list
        // would surface much later as a plant that refuses to bend, with nothing pointing at the
        // source that caused it.
        let bare = "#usda 1.0\n(\n    upAxis = \"Y\"\n)\n";
        assert!(resolve_usd_skeleton_source("bare.usda", bare.as_bytes()).is_err());
    }

    #[test]
    fn obj_file_source_resolves_geometry_and_material_through_one_snapshot() {
        let scratch = Scratch::new("obj-source");
        let mut assets = scratch.assets();
        let source_dir = assets.root.join("sources");
        std::fs::create_dir_all(&source_dir).expect("source directory");
        std::fs::write(source_dir.join("oak.mtl"), "newmtl Bark\nKd 0.6 0.4 0.2\n")
            .expect("material source");
        std::fs::write(
            source_dir.join("oak.obj"),
            concat!(
                "mtllib oak.mtl\n",
                "o Oak\n",
                "v -0.5 0 0\n",
                "v 0.5 0 0\n",
                "v 0 1 0\n",
                "vt 0 0\n",
                "vt 1 0\n",
                "vt 0.5 1\n",
                "vn 0 0 1\n",
                "usemtl Bark\n",
                "f 1/1/1 2/2/1 3/3/1\n",
            ),
        )
        .expect("geometry source");
        let material = Uuid(8_001);
        let material_selector = PlantSourceSelector::Element {
            id: u128::from(sub_id_for("oak", "material", "Bark", 0).value()),
            path: "materials/Bark".to_owned(),
        };
        let source_uri = "sources/oak.obj".to_owned();
        let family = base_family(
            material,
            PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
                sources: vec![
                    PlantSourceReference {
                        id: 10,
                        locator: PlantSourceLocator::File(source_uri.clone()),
                        role: PlantSourceRole::Geometry,
                        selector: PlantSourceSelector::Whole,
                        content_hash: [1; 32],
                        settings: PlantImportSettings {
                            pivot: PlantPivot::SourceOrigin,
                            ..PlantImportSettings::default()
                        },
                        provenance: provenance(),
                    },
                    PlantSourceReference {
                        id: 11,
                        locator: PlantSourceLocator::File(source_uri),
                        role: PlantSourceRole::Material,
                        selector: PlantSourceSelector::Whole,
                        content_hash: [2; 32],
                        settings: PlantImportSettings::default(),
                        provenance: provenance(),
                    },
                ],
                semantic_targets: vec![
                    PlantManualSemanticTarget {
                        id: 30,
                        source: 10,
                        selector: PlantSourceSelector::Whole,
                        destination: PlantSemanticDestination::Part(40),
                    },
                    PlantManualSemanticTarget {
                        id: 31,
                        source: 11,
                        selector: material_selector,
                        destination: PlantSemanticDestination::MaterialSlot(0),
                    },
                ],
            }),
        );
        let validation =
            validate_plant_family_sources(&mut assets, &family, PlantCompileLimits::default())
                .expect("validate OBJ source");
        assert!(
            validation.compile.publishable(),
            "{:?}",
            validation.compile.diagnostics
        );
        assert_eq!(validation.compile.statistics.meshes, 1);
        assert_eq!(validation.compile.statistics.materials, 1);
    }

    /// Splitting one source's faces across two materials must not move its geometry.
    ///
    /// A material names a surface, not a place. Two OBJs carrying byte-identical vertices must cook
    /// to byte-identical positions whether their faces sit in one `usemtl` run or two — otherwise a
    /// family that gains a second material silently relocates, which reads downstream as plants
    /// missing from the view rather than as anything to do with materials.
    #[test]
    fn a_second_material_run_does_not_move_the_cooked_geometry() {
        let single = cook_two_box_obj("obj-one-material", false);
        let split = cook_two_box_obj("obj-two-materials", true);
        assert_eq!(
            single.len(),
            split.len(),
            "the same geometry cooked to a different vertex count"
        );
        for (index, (one, two)) in single.iter().zip(&split).enumerate() {
            assert_eq!(
                one, two,
                "vertex {index} moved when the faces gained a second material"
            );
        }
    }

    /// Cooks one two-box OBJ — the boxes in one `usemtl` run or in two — and returns the published
    /// family's vertex positions as bit patterns, so a comparison is exact rather than approximate.
    fn cook_two_box_obj(tag: &str, split: bool) -> Vec<[u32; 3]> {
        let scratch = Scratch::new(tag);
        let mut assets = scratch.assets();
        let source_dir = assets.root.join("sources");
        std::fs::create_dir_all(&source_dir).expect("source directory");
        std::fs::write(
            source_dir.join("oak.mtl"),
            "newmtl Bark\nKd 0.6 0.4 0.2\nnewmtl Leaf\nKd 0.2 0.5 0.2\n",
        )
        .expect("material source");
        // Identical geometry in both cases; only the second run's `usemtl` differs.
        let second_material = if split { "usemtl Leaf\n" } else { "" };
        std::fs::write(
            source_dir.join("oak.obj"),
            format!(
                concat!(
                    "mtllib oak.mtl\n",
                    "o Trunk\nv -0.5 0 0\nv 0.5 0 0\nv 0 1 0\n",
                    "vt 0 0\nvt 1 0\nvt 0.5 1\nvn 0 0 1\n",
                    "usemtl Bark\nf 1/1/1 2/2/1 3/3/1\n",
                    "o Canopy\nv -0.5 1 0\nv 0.5 1 0\nv 0 2 0\n",
                    "{}f 4/1/1 5/2/1 6/3/1\n",
                ),
                second_material
            ),
        )
        .expect("geometry source");

        let material = Uuid(8_010);
        let source_uri = "sources/oak.obj".to_owned();
        let mut targets = vec![
            PlantManualSemanticTarget {
                id: 30,
                source: 10,
                selector: PlantSourceSelector::Whole,
                destination: PlantSemanticDestination::Part(40),
            },
            PlantManualSemanticTarget {
                id: 31,
                source: 11,
                selector: PlantSourceSelector::Element {
                    id: u128::from(sub_id_for("oak", "material", "Bark", 0).value()),
                    path: "materials/Bark".to_owned(),
                },
                destination: PlantSemanticDestination::MaterialSlot(0),
            },
        ];
        let mut family = base_family(
            material,
            PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
                sources: vec![
                    PlantSourceReference {
                        id: 10,
                        locator: PlantSourceLocator::File(source_uri.clone()),
                        role: PlantSourceRole::Geometry,
                        selector: PlantSourceSelector::Whole,
                        content_hash: [1; 32],
                        settings: PlantImportSettings::default(),
                        provenance: provenance(),
                    },
                    PlantSourceReference {
                        id: 11,
                        locator: PlantSourceLocator::File(source_uri),
                        role: PlantSourceRole::Material,
                        selector: PlantSourceSelector::Whole,
                        content_hash: [1; 32],
                        settings: PlantImportSettings::default(),
                        provenance: provenance(),
                    },
                ],
                semantic_targets: Vec::new(),
            }),
        );
        if split {
            targets.push(PlantManualSemanticTarget {
                id: 33,
                source: 11,
                selector: PlantSourceSelector::Element {
                    id: u128::from(sub_id_for("oak", "material", "Leaf", 1).value()),
                    path: "materials/Leaf".to_owned(),
                },
                destination: PlantSemanticDestination::MaterialSlot(1),
            });
            family.material_slots.push(Uuid(8_011));
        }
        if let PlantFamilySource::Imported(recipe) = &mut family.source {
            recipe.semantic_targets = targets;
        }

        let id = crate::save_plant_family_asset(&mut assets, family, "Oak", "plants")
            .expect("register family");
        let family = crate::load_plant_family_asset(&assets, id).expect("reload family");
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        match outcome {
            PlantRecookOutcome::Published(published) => {
                let bytes = std::fs::read(&published.publication.path).expect("artifact");
                let decoded = crate::plant_render::decode_plant_render_sections(&bytes)
                    .expect("render-decode");
                decoded
                    .mesh
                    .vertices
                    .iter()
                    .map(|vertex| vertex.position.to_array().map(f32::to_bits))
                    .collect()
            }
            PlantRecookOutcome::Rejected(validation) => panic!(
                "rejected: {:#?} {:#?}",
                validation.compile.conflicts, validation.compile.diagnostics
            ),
        }
    }

    /// A multi-part OBJ family binds each part through a submesh selector, not an element one.
    ///
    /// The OBJ importer collapses every `o` block into ONE node carrying ONE mesh, so a file with
    /// two objects still yields a single element and there is no second element identity to name.
    /// What survives the merge is the material run: faces are grouped into submeshes in first-seen
    /// `usemtl` order, which is what `PlantSourceSelector::Submesh` addresses.
    #[test]
    fn obj_submesh_selectors_bind_each_material_run_to_its_own_part() {
        let scratch = Scratch::new("obj-submesh");
        let mut assets = scratch.assets();
        let source_dir = assets.root.join("sources");
        std::fs::create_dir_all(&source_dir).expect("source directory");
        std::fs::write(
            source_dir.join("oak.mtl"),
            "newmtl Bark\nKd 0.6 0.4 0.2\nnewmtl Leaf\nKd 0.2 0.5 0.2\n",
        )
        .expect("material source");
        // Two objects with distinct `usemtl` runs. OBJ vertex indices are file-global and
        // one-based, so the canopy references the second triple.
        std::fs::write(
            source_dir.join("oak.obj"),
            concat!(
                "mtllib oak.mtl\n",
                "vt 0 0\n",
                "vt 1 0\n",
                "vt 0.5 1\n",
                "vn 0 0 1\n",
                "o Trunk\n",
                "v -0.5 0 0\n",
                "v 0.5 0 0\n",
                "v 0 1 0\n",
                "usemtl Bark\n",
                "f 1/1/1 2/2/1 3/3/1\n",
                "o Canopy\n",
                "v -0.5 1 0\n",
                "v 0.5 1 0\n",
                "v 0 2 0\n",
                "usemtl Leaf\n",
                "f 4/1/1 5/2/1 6/3/1\n",
            ),
        )
        .expect("geometry source");

        // The node the importer produced is named for the file stem and is the only one, so its
        // identity is fully determined — no guessing at what a second object would have been called.
        let element = u128::from(sub_id_for("oak", "mesh", "oak", 0).value());
        let trunk = PlantSourceSelector::Submesh { element, index: 0 };
        let canopy = PlantSourceSelector::Submesh { element, index: 1 };
        let material = Uuid(8_002);
        let source_uri = "sources/oak.obj".to_owned();
        let mut family = base_family(
            material,
            PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
                sources: vec![
                    PlantSourceReference {
                        id: 10,
                        locator: PlantSourceLocator::File(source_uri.clone()),
                        role: PlantSourceRole::Geometry,
                        selector: PlantSourceSelector::Whole,
                        content_hash: [1; 32],
                        settings: PlantImportSettings::default(),
                        provenance: provenance(),
                    },
                    PlantSourceReference {
                        id: 11,
                        locator: PlantSourceLocator::File(source_uri),
                        role: PlantSourceRole::Material,
                        selector: PlantSourceSelector::Whole,
                        // Geometry and material read one file, so they must agree on its bytes
                        // and on how to read them: one address carrying two contents is an error.
                        content_hash: [1; 32],
                        settings: PlantImportSettings::default(),
                        provenance: provenance(),
                    },
                ],
                semantic_targets: vec![
                    PlantManualSemanticTarget {
                        id: 30,
                        source: 10,
                        selector: trunk,
                        destination: PlantSemanticDestination::Part(40),
                    },
                    PlantManualSemanticTarget {
                        id: 31,
                        source: 10,
                        selector: canopy,
                        destination: PlantSemanticDestination::Part(41),
                    },
                    PlantManualSemanticTarget {
                        id: 32,
                        source: 11,
                        selector: PlantSourceSelector::Element {
                            id: u128::from(sub_id_for("oak", "material", "Bark", 0).value()),
                            path: "materials/Bark".to_owned(),
                        },
                        destination: PlantSemanticDestination::MaterialSlot(0),
                    },
                    // Every imported material needs exactly one slot target: a second material run
                    // with nowhere to land fails the whole source, not just its own binding.
                    PlantManualSemanticTarget {
                        id: 33,
                        source: 11,
                        selector: PlantSourceSelector::Element {
                            id: u128::from(sub_id_for("oak", "material", "Leaf", 1).value()),
                            path: "materials/Leaf".to_owned(),
                        },
                        destination: PlantSemanticDestination::MaterialSlot(1),
                    },
                ],
            }),
        );
        family.material_slots.push(Uuid(8_003));
        family.parts.push(PlantPart {
            id: 41,
            parent: Some(40),
            semantic: PlantPartSemantic::Leaf,
            material_slot: 1,
            sources: vec![10],
        });

        let validation =
            validate_plant_family_sources(&mut assets, &family, PlantCompileLimits::default())
                .expect("validate OBJ source");
        assert!(
            validation.compile.publishable(),
            "{:?}",
            validation.compile.diagnostics
        );
        // Both material runs survived as submeshes of the one merged element — the premise the
        // canopy selector rests on, asserted rather than assumed.
        assert_eq!(validation.compile.statistics.materials, 2);
    }

    fn source() -> PlantSourceReference {
        PlantSourceReference {
            id: 10,
            locator: PlantSourceLocator::File("file:///plants/oak.gltf".to_owned()),
            role: PlantSourceRole::Geometry,
            selector: PlantSourceSelector::Whole,
            content_hash: [0; 32],
            settings: PlantImportSettings::default(),
            provenance: saffron_vegetation::SourceProvenance::default(),
        }
    }

    /// A file that states a copyright the plant source does not record raises it, so an export from a
    /// tool whose licence requires attribution cannot be cooked in silence.
    #[test]
    fn a_stated_copyright_without_recorded_attribution_is_reported() {
        let origin = saffron_geometry::ImportedOrigin {
            generator: "SpeedTree Modeler 9.5.2".to_owned(),
            copyright: "(c) 2026 Example Studio".to_owned(),
        };
        let notice = attribution_notice(&source(), &origin).expect("a notice");
        assert_eq!(notice.severity, PlantCompileDiagnosticSeverity::Warning);
        assert!(notice.message.contains("SpeedTree Modeler 9.5.2"));
        assert!(notice.message.contains("Example Studio"));

        // Once the source records an attribution there is nothing to raise.
        let mut recorded = source();
        recorded.provenance.attribution = "Oak by Example Studio".to_owned();
        assert!(attribution_notice(&recorded, &origin).is_none());

        // A generator alone is not a licence claim, and a file that states nothing raises nothing.
        let generator_only = saffron_geometry::ImportedOrigin {
            generator: "Blender 4.2".to_owned(),
            copyright: String::new(),
        };
        assert!(attribution_notice(&source(), &generator_only).is_none());
        assert!(
            attribution_notice(&source(), &saffron_geometry::ImportedOrigin::default()).is_none()
        );
    }
}
