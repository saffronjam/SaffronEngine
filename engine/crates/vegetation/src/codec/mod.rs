//! Canonical versioned codecs for vegetation-authored assets and sparse map chunks.

mod biome;
mod enums;
mod map;
mod plant;
mod stream;

pub use biome::{biome_asset_schema_hash, read_biome_asset, write_biome_asset};
pub use map::{
    read_vegetation_map_asset, read_vegetation_map_chunk, vegetation_map_chunk_schema_hash,
    vegetation_map_schema_hash, write_vegetation_map_asset, write_vegetation_map_chunk,
};
pub use plant::{plant_asset_schema_hash, read_plant_asset, write_plant_asset};

#[cfg(test)]
fn fixed(value: i32) -> saffron_spatial::DecisionScalar {
    saffron_spatial::DecisionScalar::from_integer(value).unwrap()
}
