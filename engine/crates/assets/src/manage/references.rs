use saffron_core::Uuid;
use saffron_geometry::ChunkKind;
use saffron_scene::{
    AssetEntry, AssetType, IdComponent, MaterialSet, Mesh as MeshComponent, ModelInstance, Scene,
    SkinnedMesh, VegetationField,
};

use crate::AssetServer;

use super::container::resolve_container_material;

/// One node in the asset dependency graph: an asset and its on-disk byte cost.
#[derive(Clone, Copy, Debug)]
pub struct RefNode {
    /// The asset id.
    pub id: Uuid,
    /// The asset kind.
    pub asset_type: AssetType,
    /// The owning container (`0` for a standalone asset).
    pub container: Uuid,
    /// The on-disk byte cost.
    pub bytes: u64,
}

/// How an [`RefEdge`] arises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefEdgeKind {
    /// A container references its embedded sub-asset.
    ContainerChild,
    /// A material references a texture slot.
    MaterialTexture,
    /// A vegetation map, biome, or plant family references another catalog asset.
    VegetationDependency,
    /// A scene entity references an asset.
    EntityAsset,
}

/// One directed edge: `from` references `to`. `from` may be an entity uuid (`EntityAsset`)
/// rather than a catalog asset.
#[derive(Clone, Copy, Debug)]
pub struct RefEdge {
    /// The referrer id.
    pub from: Uuid,
    /// The referenced id.
    pub to: Uuid,
    /// How the reference arises.
    pub kind: RefEdgeKind,
}

/// The scene → asset → sub-asset reference graph: who-references-this,
/// what-this-references, and a byte footprint. Read-only/diagnostic, rebuilt on demand.
#[derive(Clone, Debug, Default)]
pub struct DependencyGraph {
    /// The catalog assets as nodes.
    pub nodes: Vec<RefNode>,
    /// The directed reference edges.
    pub edges: Vec<RefEdge>,
}

impl DependencyGraph {
    /// The referrers of `id` (every edge's `from` where `to == id`).
    #[must_use]
    pub fn referenced_by(&self, id: Uuid) -> Vec<Uuid> {
        self.edges
            .iter()
            .filter(|e| e.to.value() == id.value())
            .map(|e| e.from)
            .collect()
    }

    /// The references of `id` (every edge's `to` where `from == id`).
    #[must_use]
    pub fn references_of(&self, id: Uuid) -> Vec<Uuid> {
        self.edges
            .iter()
            .filter(|e| e.from.value() == id.value())
            .map(|e| e.to)
            .collect()
    }

    /// The on-disk bytes recorded for `id` (`0` when absent).
    #[must_use]
    pub fn bytes_of(&self, id: Uuid) -> u64 {
        self.nodes
            .iter()
            .find(|n| n.id.value() == id.value())
            .map_or(0, |n| n.bytes)
    }

    /// The on-disk footprint of `id` — its own bytes. A container's `.smodel` size already
    /// counts its embedded sub-assets, so there is no double-counting.
    #[must_use]
    pub fn footprint(&self, id: Uuid) -> u64 {
        self.bytes_of(id)
    }
}

/// The on-disk bytes of a catalog row: a model / standalone file's size, or an embedded
/// sub-asset's chunk length (read from its container's TOC).
#[must_use]
pub fn asset_bytes(assets: &mut AssetServer, entry: &AssetEntry) -> u64 {
    if entry.container.value() == 0 {
        if entry.asset_type == AssetType::VegetationMap {
            return crate::vegetation::vegetation_map_package_bytes(assets, entry);
        }
        return std::fs::metadata(format!("{}/{}", assets.root.display(), entry.path))
            .map(|m| m.len())
            .unwrap_or(0);
    }
    let Some(model) = assets.load_model_asset(entry.container) else {
        return 0;
    };
    let kind = match entry.asset_type {
        AssetType::Mesh => ChunkKind::Mesh,
        AssetType::Material => ChunkKind::Material,
        AssetType::Animation => ChunkKind::Animation,
        AssetType::Plant | AssetType::Biome | AssetType::VegetationMap => return 0,
        _ => ChunkKind::Texture,
    };
    model
        .reader
        .find(kind, entry.id.value())
        .map_or(0, |toc| toc.length)
}

/// Gathers each entity's stable id plus the asset ids it references, by component query.
/// Two passes because [`Scene::for_each`](saffron_scene::Scene::for_each) borrows the world
/// mutably during the callback, so the id read happens after.
fn entity_asset_pairs(scene: &mut Scene) -> Vec<(Uuid, Uuid)> {
    let mut refs: Vec<(saffron_scene::Entity, Uuid)> = Vec::new();
    scene.for_each::<&MeshComponent, _>(|entity, mesh| refs.push((entity, mesh.mesh)));
    scene.for_each::<&SkinnedMesh, _>(|entity, skin| refs.push((entity, skin.mesh)));
    // A material slot references a `.smat`; the material→texture edges come from the graph
    // builder below, so an entity keeps its textures alive transitively.
    scene.for_each::<&MaterialSet, _>(|entity, set| {
        for slot in &set.slots {
            refs.push((entity, slot.material));
        }
    });
    scene.for_each::<&ModelInstance, _>(|entity, instance| refs.push((entity, instance.model_id)));
    scene.for_each::<&VegetationField, _>(|entity, field| refs.push((entity, field.map)));

    let mut out = Vec::new();
    for (entity, asset) in refs {
        if asset.value() == 0 {
            continue;
        }
        let entity_id = scene
            .component::<IdComponent>(entity)
            .map_or(0, |id| id.id.value());
        out.push((Uuid(entity_id), asset));
    }
    out
}

/// Builds the dependency graph: catalog assets as nodes; container→child,
/// material→texture, and scene-entity→asset edges. A snapshot — rebuilt on demand.
pub fn build_dependency_graph(scene: &mut Scene, assets: &mut AssetServer) -> DependencyGraph {
    let mut graph = DependencyGraph::default();
    let entries: Vec<AssetEntry> = assets.catalog.entries.clone();
    for entry in &entries {
        let bytes = asset_bytes(assets, entry);
        graph.nodes.push(RefNode {
            id: entry.id,
            asset_type: entry.asset_type,
            container: entry.container,
            bytes,
        });
    }
    for entry in &entries {
        // A Model container owns its children by `container == parent id`; a material import is
        // a self-container (`container == its own id`), so it owns its embedded maps the same
        // way — minus the self-edge, since the parent row itself matches that predicate.
        let is_container_parent = entry.asset_type == AssetType::Model
            || (entry.asset_type == AssetType::Material && entry.container == entry.id);
        if is_container_parent {
            for child in &entries {
                if child.container.value() == entry.id.value() && child.id != entry.id {
                    graph.edges.push(RefEdge {
                        from: entry.id,
                        to: child.id,
                        kind: RefEdgeKind::ContainerChild,
                    });
                }
            }
        }
        if entry.asset_type == AssetType::Material {
            let material = if entry.container.value() != 0 {
                resolve_container_material(assets, entry.container, entry.id)
            } else {
                crate::material::load_material_asset(assets, entry.id).ok()
            };
            if let Some(material) = material {
                for tex in [
                    material.albedo_texture,
                    material.orm_texture,
                    material.normal_texture,
                    material.emissive_texture,
                    material.height_texture,
                ] {
                    if tex.value() != 0 {
                        graph.edges.push(RefEdge {
                            from: entry.id,
                            to: tex,
                            kind: RefEdgeKind::MaterialTexture,
                        });
                    }
                }
                if let saffron_vegetation::MaterialSurface::ThinSheetFoliage(parameters) =
                    material.surface
                    && let saffron_vegetation::CoverageSource::Texture(texture) =
                        parameters.coverage_source
                {
                    graph.edges.push(RefEdge {
                        from: entry.id,
                        to: texture,
                        kind: RefEdgeKind::MaterialTexture,
                    });
                }
            }
        }
        match entry.asset_type {
            AssetType::Plant => {
                if let Ok(plant) = crate::vegetation::load_plant_family_asset(assets, entry.id) {
                    for dependency in plant.material_slots {
                        if dependency.value() != 0 {
                            graph.edges.push(RefEdge {
                                from: entry.id,
                                to: dependency,
                                kind: RefEdgeKind::VegetationDependency,
                            });
                        }
                    }
                    if let saffron_vegetation::PlantFamilySource::Imported(recipe) = plant.source {
                        for source in recipe.sources {
                            if let saffron_vegetation::PlantSourceLocator::Asset(dependency) =
                                source.locator
                            {
                                graph.edges.push(RefEdge {
                                    from: entry.id,
                                    to: dependency,
                                    kind: RefEdgeKind::VegetationDependency,
                                });
                            }
                        }
                    }
                }
            }
            AssetType::Biome => {
                if let Ok(biome) = crate::vegetation::load_biome_asset(assets, entry.id) {
                    let dependencies = biome
                        .palette
                        .into_iter()
                        .map(|item| item.plant)
                        .chain(
                            biome
                                .competition
                                .into_iter()
                                .flat_map(|rule| [rule.first, rule.second]),
                        )
                        .chain(
                            biome
                                .companions
                                .into_iter()
                                .flat_map(|rule| [rule.parent, rule.child]),
                        )
                        .chain(
                            biome
                                .succession
                                .into_iter()
                                .flat_map(|rule| [rule.from, rule.to]),
                        )
                        .chain(biome.modules.into_iter().map(|item| item.biome));
                    for dependency in dependencies.filter(|dependency| dependency.value() != 0) {
                        graph.edges.push(RefEdge {
                            from: entry.id,
                            to: dependency,
                            kind: RefEdgeKind::VegetationDependency,
                        });
                    }
                }
            }
            AssetType::VegetationMap => {
                if let Ok(dependencies) =
                    crate::vegetation::vegetation_map_dependencies(assets, entry)
                {
                    for dependency in dependencies {
                        graph.edges.push(RefEdge {
                            from: entry.id,
                            to: dependency,
                            kind: RefEdgeKind::VegetationDependency,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    for (entity_id, asset) in entity_asset_pairs(scene) {
        graph.edges.push(RefEdge {
            from: entity_id,
            to: asset,
            kind: RefEdgeKind::EntityAsset,
        });
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_support::{bake_and_register, first_sub, one_material_graph, scratch};

    #[test]
    fn dependency_graph_links_container_children_material_textures_and_entities() {
        let dir = scratch("graph");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        let model_id = bake_and_register(&mut assets, &one_material_graph(), "/tmp/paint.obj");
        let mesh_sub = first_sub(&assets, model_id, AssetType::Mesh);
        let material_sub = first_sub(&assets, model_id, AssetType::Material);
        let texture_sub = first_sub(&assets, model_id, AssetType::Texture);

        let mut scene = Scene::new();
        let entity = scene.create_entity("Tri");
        scene
            .add_component(entity, MeshComponent { mesh: mesh_sub })
            .unwrap();
        let entity_id = scene.component::<IdComponent>(entity).unwrap().id;

        let graph = build_dependency_graph(&mut scene, &mut assets);

        let children = graph.references_of(model_id);
        assert!(children.iter().any(|c| c.value() == mesh_sub.value()));
        assert!(children.iter().any(|c| c.value() == material_sub.value()));

        assert!(
            graph
                .references_of(material_sub)
                .iter()
                .any(|t| t.value() == texture_sub.value()),
            "material → texture edge"
        );
        assert!(
            graph
                .referenced_by(mesh_sub)
                .iter()
                .any(|r| r.value() == entity_id.value()),
            "entity → mesh edge"
        );
        assert!(graph.footprint(model_id) > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
