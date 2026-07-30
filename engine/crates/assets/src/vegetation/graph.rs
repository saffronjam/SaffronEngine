//! Biome-graph compilation against the project catalog, and the canonical dependency
//! identities every cook key hashes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use saffron_core::Uuid;
use saffron_spatial::SurfaceField;
use saffron_vegetation::{
    BiomeAsset, BiomeGraphResolver, CompiledBiomeGraph, CompiledGraphUnit, GraphCompileOptions,
    GraphDependencySource, PlantPrototype, VegetationMapChunkKind, VegetationMapChunkPayload,
    VegetationMapTileKey, canonical_surface_provider_set_hash, compile_biome_graph,
    vegetation_content_hash,
};

use crate::cook_reader::CookAssetAccess;
use crate::{AssetServer, Error, Result};

use super::asset_io::{load_biome_asset, load_biome_asset_from, load_plant_family_asset_from};
use super::map_package::load_vegetation_map_snapshot_from;

/// A catalog-resolved graph and the complete family prototype table it can emit.
#[derive(Clone, Debug)]
pub struct ResolvedBiomeGraph {
    /// Single compiled graph IR used by every evaluator surface.
    pub graph: CompiledBiomeGraph,
    /// Bound local biome instance, absent only for standalone schema/compile inspection.
    pub biome_instance: Option<u128>,
    /// Sorted authoritative family prototypes required by the graph and its modules.
    pub plant_prototypes: Vec<PlantPrototype>,
}

/// Catalog and canonical non-asset dependency resolver for biome compilation.
pub struct CatalogBiomeGraphResolver<'a> {
    assets: &'a AssetServer,
    external_dependencies: &'a BTreeMap<GraphDependencySource, [u8; 32]>,
}

struct CookBiomeGraphResolver<'a> {
    assets: &'a dyn CookAssetAccess,
    external_dependencies: &'a BTreeMap<GraphDependencySource, [u8; 32]>,
}

impl BiomeGraphResolver for CookBiomeGraphResolver<'_> {
    fn resolve_biome(
        &self,
        id: Uuid,
    ) -> std::result::Result<BiomeAsset, saffron_vegetation::Error> {
        load_biome_asset_from(self.assets, id)
            .map_err(|error| dependency_error(GraphDependencySource::Asset(id), error))
    }

    fn resolve_dependency_hash(
        &self,
        source: GraphDependencySource,
    ) -> std::result::Result<[u8; 32], saffron_vegetation::Error> {
        if let GraphDependencySource::Asset(id) = source {
            let entry = self.assets.catalog().find(id).ok_or_else(|| {
                saffron_vegetation::Error::GraphDocument {
                    path: dependency_path(source),
                    reason: "catalog asset is missing".to_owned(),
                }
            })?;
            let bytes = self
                .assets
                .read_file(&self.assets.root().join(&entry.path))
                .map_err(|error| dependency_error(source, error))?;
            return Ok(vegetation_content_hash(&bytes));
        }
        self.external_dependencies
            .get(&source)
            .copied()
            .ok_or_else(|| saffron_vegetation::Error::GraphDocument {
                path: dependency_path(source),
                reason: "canonical external dependency identity is missing".to_owned(),
            })
    }

    fn available_dependencies(&self) -> Vec<GraphDependencySource> {
        self.external_dependencies.keys().copied().collect()
    }
}

impl<'a> CatalogBiomeGraphResolver<'a> {
    /// Binds catalog assets and exact field/provider/map dependency identities.
    #[must_use]
    pub fn new(
        assets: &'a AssetServer,
        external_dependencies: &'a BTreeMap<GraphDependencySource, [u8; 32]>,
    ) -> Self {
        Self {
            assets,
            external_dependencies,
        }
    }
}

impl BiomeGraphResolver for CatalogBiomeGraphResolver<'_> {
    fn resolve_biome(
        &self,
        id: Uuid,
    ) -> std::result::Result<BiomeAsset, saffron_vegetation::Error> {
        load_biome_asset(self.assets, id)
            .map_err(|error| dependency_error(GraphDependencySource::Asset(id), error))
    }

    fn resolve_dependency_hash(
        &self,
        source: GraphDependencySource,
    ) -> std::result::Result<[u8; 32], saffron_vegetation::Error> {
        if let GraphDependencySource::Asset(id) = source {
            let entry = self.assets.catalog.find(id).ok_or_else(|| {
                saffron_vegetation::Error::GraphDocument {
                    path: dependency_path(source),
                    reason: "catalog asset is missing".to_owned(),
                }
            })?;
            let bytes = std::fs::read(self.assets.root.join(&entry.path))
                .map_err(|error| dependency_error(source, Error::Io(error.to_string())))?;
            return Ok(vegetation_content_hash(&bytes));
        }
        self.external_dependencies
            .get(&source)
            .copied()
            .ok_or_else(|| saffron_vegetation::Error::GraphDocument {
                path: dependency_path(source),
                reason: "canonical external dependency identity is missing".to_owned(),
            })
    }

    fn available_dependencies(&self) -> Vec<GraphDependencySource> {
        self.external_dependencies.keys().copied().collect()
    }
}

/// Compiles a catalog biome and resolves every family prototype reachable through its modules.
pub fn compile_catalog_biome_graph(
    assets: &AssetServer,
    biome: Uuid,
    root_bindings: &[(u128, serde_json::Value)],
    external_dependencies: &BTreeMap<GraphDependencySource, [u8; 32]>,
    options: GraphCompileOptions,
) -> Result<ResolvedBiomeGraph> {
    compile_catalog_biome_graph_from(assets, biome, root_bindings, external_dependencies, options)
}

pub(crate) fn compile_catalog_biome_graph_from(
    assets: &dyn CookAssetAccess,
    biome: Uuid,
    root_bindings: &[(u128, serde_json::Value)],
    external_dependencies: &BTreeMap<GraphDependencySource, [u8; 32]>,
    options: GraphCompileOptions,
) -> Result<ResolvedBiomeGraph> {
    let root = load_biome_asset_from(assets, biome)?;
    let resolver = CookBiomeGraphResolver {
        assets,
        external_dependencies,
    };
    let graph = compile_biome_graph(&root, root_bindings, &resolver, options)?;
    let mut families = BTreeSet::new();
    collect_graph_families(&graph.root, &mut families);
    let plant_prototypes = families
        .into_iter()
        .map(|family| {
            let asset = load_plant_family_asset_from(assets, Uuid(family))?;
            Ok(PlantPrototype::from_family(&asset)?)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ResolvedBiomeGraph {
        graph,
        biome_instance: None,
        plant_prototypes,
    })
}

/// Compiles one map-local biome instance with its exact typed bindings.
pub fn compile_catalog_biome_instance_graph(
    assets: &AssetServer,
    map: Uuid,
    biome_instance: u128,
    external_dependencies: &BTreeMap<GraphDependencySource, [u8; 32]>,
    options: GraphCompileOptions,
) -> Result<ResolvedBiomeGraph> {
    compile_catalog_biome_instance_graph_from(
        assets,
        map,
        biome_instance,
        external_dependencies,
        options,
    )
}

pub(crate) fn compile_catalog_biome_instance_graph_from(
    assets: &dyn CookAssetAccess,
    map: Uuid,
    biome_instance: u128,
    external_dependencies: &BTreeMap<GraphDependencySource, [u8; 32]>,
    options: GraphCompileOptions,
) -> Result<ResolvedBiomeGraph> {
    let map_asset = load_vegetation_map_snapshot_from(assets, map)?;
    let instance = map_asset
        .biome_instances
        .iter()
        .find(|instance| instance.id == biome_instance)
        .ok_or_else(|| {
            Error::Io("vegetation biome instance is not present in the map".to_owned())
        })?;
    let mut resolved = compile_catalog_biome_graph_from(
        assets,
        instance.biome,
        &instance.bindings,
        external_dependencies,
        options,
    )?;
    resolved.biome_instance = Some(biome_instance);
    Ok(resolved)
}

/// Resolves canonical authored-map and surface-provider hashes for graph compilation.
pub fn vegetation_graph_dependency_hashes(
    assets: &AssetServer,
    map: Uuid,
    surface_providers: &[Arc<dyn SurfaceField>],
) -> Result<BTreeMap<GraphDependencySource, [u8; 32]>> {
    vegetation_graph_dependency_hashes_from(assets, map, surface_providers)
}

pub(crate) fn vegetation_graph_dependency_hashes_from(
    assets: &dyn CookAssetAccess,
    map: Uuid,
    surface_providers: &[Arc<dyn SurfaceField>],
) -> Result<BTreeMap<GraphDependencySource, [u8; 32]>> {
    let map_asset = load_vegetation_map_snapshot_from(assets, map)?;
    let mut result = BTreeMap::new();
    let layer_ids = map_asset
        .layers
        .iter()
        .map(|layer| layer.id)
        .collect::<BTreeSet<_>>();
    for layer in &map_asset.layers {
        let reference = map_asset
            .inventory
            .iter()
            .find(|reference| {
                reference.key.layer == layer.id
                    && reference.key.kind == VegetationMapChunkKind::LayerMetadata
                    && reference.key.tile == VegetationMapTileKey::Global
            })
            .ok_or_else(|| Error::Io("vegetation-map layer object is missing".to_owned()))?;
        result.insert(
            GraphDependencySource::MapLayer(layer.id),
            reference.content_hash,
        );
    }
    for chunk in &map_asset.chunks {
        if let VegetationMapChunkPayload::Field(payload) = &chunk.payload {
            for field in payload.fields.iter().chain(&payload.blockers) {
                if !layer_ids.contains(&field.layer) {
                    return Err(Error::Io(
                        "authored field references a layer absent from its vegetation map"
                            .to_owned(),
                    ));
                }
                let channel = field.channel;
                let source = GraphDependencySource::Field(channel);
                let mut preimage = b"saffron-anima/map-field-contract/v1\0".to_vec();
                preimage.extend_from_slice(&source.canonical_bytes());
                result
                    .entry(source)
                    .or_insert_with(|| vegetation_content_hash(&preimage));
            }
        }
    }
    let mut ordered_providers = surface_providers.iter().collect::<Vec<_>>();
    ordered_providers.sort_by_key(|provider| provider.descriptor().id);
    for provider in ordered_providers {
        let descriptor = provider.descriptor();
        let hash = canonical_surface_provider_set_hash(&[Arc::clone(provider)], 1)?;
        if result
            .insert(
                GraphDependencySource::SurfaceProvider(descriptor.id.0),
                hash,
            )
            .is_some()
        {
            return Err(Error::Io(
                "surface provider identity is duplicated".to_owned(),
            ));
        }
        for channel in provider.field_channels() {
            let preimage = result
                .entry(GraphDependencySource::Field(channel))
                .or_insert_with(|| vegetation_content_hash(b"saffron-anima/surface-field/v1\0"));
            let mut bytes = preimage.to_vec();
            bytes.extend_from_slice(&descriptor.id.0.to_be_bytes());
            bytes.extend_from_slice(&hash);
            *preimage = vegetation_content_hash(&bytes);
        }
    }
    Ok(result)
}

fn collect_graph_families(unit: &CompiledGraphUnit, families: &mut BTreeSet<u64>) {
    families.extend(unit.palette.iter().map(|entry| entry.plant.value()));
    for node in &unit.nodes {
        if let Some(module) = node.module.as_deref() {
            collect_graph_families(module, families);
        }
    }
}

fn dependency_path(source: GraphDependencySource) -> String {
    match source {
        GraphDependencySource::Asset(id) => format!("dependencies.asset.{}", id.value()),
        GraphDependencySource::Field(channel) => format!("dependencies.field.{channel:?}"),
        GraphDependencySource::SurfaceProvider(provider) => {
            format!("dependencies.surfaceProvider.{provider}")
        }
        GraphDependencySource::MapLayer(layer) => {
            format!("dependencies.mapLayer.{layer:032x}")
        }
    }
}

fn dependency_error(source: GraphDependencySource, error: Error) -> saffron_vegetation::Error {
    saffron_vegetation::Error::GraphDocument {
        path: dependency_path(source),
        reason: error.to_string(),
    }
}
