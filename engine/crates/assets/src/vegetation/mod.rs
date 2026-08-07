//! Asset-server I/O for canonical vegetation assets and sparse map packages.

mod asset_io;
mod bounds;
mod catalog_io;
mod graph;
mod graph_inputs;
mod input_snapshot;
mod map_objects;
mod map_package;
#[cfg(test)]
pub(crate) mod test_support;

pub use asset_io::{
    VegetationImport, import_vegetation_asset, load_biome_asset, load_plant_family_asset,
    save_biome_asset, save_plant_family_asset, update_biome_asset, update_plant_family_asset,
};
pub use graph::{
    CatalogBiomeGraphResolver, ResolvedBiomeGraph, compile_catalog_biome_graph,
    compile_catalog_biome_instance_graph, vegetation_graph_dependency_hashes,
};
pub use graph_inputs::assemble_biome_graph_evaluation_job;
pub use map_package::{
    VegetationMapTransaction, commit_vegetation_map_transaction, load_vegetation_map_chunks,
    load_vegetation_map_root, load_vegetation_map_snapshot, load_vegetation_map_tile_snapshot,
    remove_vegetation_map_package, save_vegetation_map_asset, update_vegetation_map_asset,
};

pub(crate) use asset_io::load_plant_family_asset_from;
pub(crate) use graph::{
    compile_catalog_biome_instance_graph_from, vegetation_graph_dependency_hashes_from,
};
pub(crate) use graph_inputs::assemble_biome_graph_evaluation_job_from;
pub(crate) use map_objects::vegetation_map_content_hash_path;
pub(crate) use map_package::{
    load_vegetation_map_snapshot_from, vegetation_map_dependencies, vegetation_map_package_bytes,
};
