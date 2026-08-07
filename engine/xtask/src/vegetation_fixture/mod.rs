//! Canonical authored vegetation package used by the real-host E2E acceptance test.

mod biome;
mod plant;
mod source;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use atomic_write_file::AtomicWriteFile;
use saffron_core::Uuid;
use saffron_spatial::DecisionScalar;
use saffron_vegetation::{
    VEGETATION_MAP_VERSION, VegetationMapAsset, VegetationMapChunkLayout,
    VegetationMapChunkReference, vegetation_content_hash, vegetation_map_chunk_schema_hash,
    write_biome_asset, write_plant_asset, write_vegetation_map_asset, write_vegetation_map_chunk,
};
use serde::Serialize;

use source::{PlantContent, SourceFile};

const PLANT: Uuid = Uuid(7_300_001);
const BIOME: Uuid = Uuid(7_300_002);
const MAP: Uuid = Uuid(7_300_003);
const DEFAULT_MATERIAL: Uuid = Uuid(1);
/// The second slot of a two-part family. The asset validator requires unique slot ids, and this
/// one is absent from the catalog, so it resolves through the imported source's material.
const CANOPY_MATERIAL: Uuid = Uuid(7_300_012);
const AUTHORED_LAYER: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
const BIOME_INSTANCE: u128 = 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0001;

/// One authored-package recipe: the canonical fixture plus the stress-matrix rows.
struct Recipe {
    file: &'static str,
    stress: Option<&'static str>,
    /// The source-file stem, unique per recipe: every fixture installs into one project, and two
    /// families sharing a source path would import each other's bytes.
    stem: &'static str,
    plant_id: Uuid,
    biome_id: Uuid,
    map_id: Uuid,
    layer: u128,
    instance: u128,
    trunk_height: i32,
    coverage_count: u32,
    /// Candidates the understory scatter produces. It feeds the micro output alone, so raising it
    /// makes the cosmetic field denser without touching a macro plant's identity.
    micro_coverage_count: u32,
    micro_dims: [u32; 3],
    cells: &'static [(i64, i64, i64)],
    seasonal: bool,
    content: PlantContent,
    expected_plant: &'static str,
    expected_accepted: &'static str,
}

const CANONICAL: Recipe = Recipe {
    file: "vegetation-phase3.json",
    stress: None,
    stem: "e2e-birch",
    plant_id: PLANT,
    biome_id: BIOME,
    map_id: MAP,
    layer: AUTHORED_LAYER,
    instance: BIOME_INSTANCE,
    trunk_height: 8,
    coverage_count: 2,
    micro_coverage_count: 2,
    micro_dims: [8, 1, 8],
    cells: &[(0, 0, 0)],
    seasonal: false,
    content: PlantContent::Trunk { canopy: false },
    expected_plant: "13a5f40885613ffa491fd721727fbb08",
    expected_accepted: "2",
};

/// The leaf-content rows share cells, density, micro field, and trunk, so the only thing that
/// separates their cooked families and their draws is the foliage the source authors.
const LEAF_CELLS: &[(i64, i64, i64)] = &[(0, 0, 0)];
const LEAF_TRUNK_HEIGHT: i32 = 4;
const LEAF_COVERAGE: u32 = 4;

const STRESS: &[Recipe] = &[
    Recipe {
        file: "vegetation-stress-meadow.json",
        stress: Some("meadow"),
        stem: "e2e-meadow",
        plant_id: Uuid(7_310_001),
        biome_id: Uuid(7_310_002),
        map_id: Uuid(7_310_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8891,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0011,
        trunk_height: 2,
        coverage_count: 4,
        micro_coverage_count: 4,
        micro_dims: [16, 1, 16],
        cells: &[(0, 0, 0)],
        seasonal: false,
        content: PlantContent::Trunk { canopy: false },
        expected_plant: "",
        expected_accepted: "1",
    },
    Recipe {
        file: "vegetation-stress-woodland.json",
        stress: Some("woodland"),
        stem: "e2e-woodland",
        plant_id: Uuid(7_320_001),
        biome_id: Uuid(7_320_002),
        map_id: Uuid(7_320_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8892,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0012,
        trunk_height: 8,
        coverage_count: 6,
        micro_coverage_count: 6,
        micro_dims: [8, 1, 8],
        cells: &[(0, 0, 0), (-1, 0, 0), (0, 0, -1), (1, 0, 0)],
        seasonal: true,
        content: PlantContent::Trunk { canopy: false },
        expected_plant: "",
        expected_accepted: "1",
    },
    Recipe {
        file: "vegetation-canopy.json",
        stress: Some("canopy"),
        stem: "e2e-canopy",
        plant_id: Uuid(7_350_001),
        biome_id: Uuid(7_350_002),
        map_id: Uuid(7_350_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8895,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0015,
        trunk_height: 8,
        coverage_count: 6,
        micro_coverage_count: 6,
        micro_dims: [8, 1, 8],
        cells: &[(0, 0, 0), (-1, 0, 0), (0, 0, -1), (1, 0, 0)],
        seasonal: true,
        content: PlantContent::Trunk { canopy: true },
        expected_plant: "",
        expected_accepted: "1",
    },
    Recipe {
        file: "vegetation-stress-scale.json",
        stress: Some("scale"),
        stem: "e2e-scale",
        plant_id: Uuid(7_330_001),
        biome_id: Uuid(7_330_002),
        map_id: Uuid(7_330_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8893,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0013,
        trunk_height: 40,
        coverage_count: 2,
        micro_coverage_count: 2,
        micro_dims: [8, 1, 8],
        cells: &[(0, 0, 0)],
        seasonal: false,
        content: PlantContent::Trunk { canopy: false },
        expected_plant: "",
        expected_accepted: "1",
    },
    Recipe {
        file: "vegetation-stress-traversal.json",
        stress: Some("traversal"),
        stem: "e2e-traversal",
        plant_id: Uuid(7_340_001),
        biome_id: Uuid(7_340_002),
        map_id: Uuid(7_340_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8894,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0014,
        trunk_height: 8,
        coverage_count: 3,
        micro_coverage_count: 3,
        micro_dims: [8, 1, 8],
        cells: &[(-1, 0, 0), (0, 0, 0), (1, 0, 0)],
        seasonal: false,
        content: PlantContent::Trunk { canopy: false },
        expected_plant: "",
        expected_accepted: "1",
    },
    Recipe {
        file: "vegetation-stress-broadleaf.json",
        stress: Some("broadleaf"),
        stem: "e2e-broadleaf",
        plant_id: Uuid(7_360_001),
        biome_id: Uuid(7_360_002),
        map_id: Uuid(7_360_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8896,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0016,
        trunk_height: LEAF_TRUNK_HEIGHT,
        coverage_count: LEAF_COVERAGE,
        micro_coverage_count: LEAF_COVERAGE,
        micro_dims: [8, 1, 8],
        cells: LEAF_CELLS,
        seasonal: false,
        content: PlantContent::BroadLeaf,
        expected_plant: "",
        expected_accepted: "1",
    },
    Recipe {
        file: "vegetation-stress-serration.json",
        stress: Some("serration"),
        stem: "e2e-serration",
        plant_id: Uuid(7_370_001),
        biome_id: Uuid(7_370_002),
        map_id: Uuid(7_370_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8897,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0017,
        trunk_height: LEAF_TRUNK_HEIGHT,
        coverage_count: LEAF_COVERAGE,
        micro_coverage_count: LEAF_COVERAGE,
        micro_dims: [8, 1, 8],
        cells: LEAF_CELLS,
        seasonal: false,
        content: PlantContent::MaskedSerration,
        expected_plant: "",
        expected_accepted: "1",
    },
    Recipe {
        file: "vegetation-stress-needles.json",
        stress: Some("needles"),
        stem: "e2e-needles",
        plant_id: Uuid(7_380_001),
        biome_id: Uuid(7_380_002),
        map_id: Uuid(7_380_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8898,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0018,
        trunk_height: LEAF_TRUNK_HEIGHT,
        coverage_count: LEAF_COVERAGE,
        micro_coverage_count: LEAF_COVERAGE,
        micro_dims: [8, 1, 8],
        cells: LEAF_CELLS,
        seasonal: false,
        content: PlantContent::ConiferNeedle,
        expected_plant: "",
        expected_accepted: "1",
    },
    // The understory row: a cosmetic field a viewport ray can actually hit, on dimensions that
    // cannot hide a transposed texel decode.
    //
    // A 32x32 understory scatter over the 64 m cell lands at 1 m, 3 m, … 63 m on both axes. Against
    // a 64x1x32 grid that is one candidate in every odd 1 m column of X and in every 2 m row of Z,
    // so half the texels carry blades and the occupied half spans the cell's full X range. Reading
    // the grid X-fastest instead would fold every one of those blades into x >= 32 m.
    Recipe {
        file: "vegetation-micro-pick.json",
        stress: Some("micro-pick"),
        stem: "e2e-micro-pick",
        plant_id: Uuid(7_390_001),
        biome_id: Uuid(7_390_002),
        map_id: Uuid(7_390_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8899,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0019,
        trunk_height: 8,
        coverage_count: 2,
        micro_coverage_count: 1_024,
        micro_dims: [64, 1, 32],
        cells: &[(0, 0, 0)],
        seasonal: false,
        content: PlantContent::Trunk { canopy: false },
        expected_plant: "",
        expected_accepted: "1",
    },
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    format_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    stress: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cells: Vec<[i64; 3]>,
    plant_hex: String,
    biome_hex: String,
    map_hex: String,
    map_objects: Vec<MapObject>,
    sources: Vec<FixtureSource>,
    plant: String,
    biome: String,
    map: String,
    authored_layer: String,
    biome_instance: String,
    expected_plant: String,
    expected_accepted: String,
}

/// One project-relative source file the E2E installs before importing the family.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FixtureSource {
    path: String,
    hex: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MapObject {
    content_hash: String,
    hex: String,
}

/// Writes the canonical fixture plus every stress-matrix fixture into `dir`.
pub fn write_all(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut written = Vec::new();
    for recipe in std::iter::once(&CANONICAL).chain(STRESS) {
        let path = dir.join(recipe.file);
        write_recipe(recipe, &path)?;
        written.push(path);
    }
    Ok(written)
}

fn write_recipe(recipe: &Recipe, path: &Path) -> Result<()> {
    let sources: Vec<SourceFile> = recipe.content.files(recipe.stem, recipe.trunk_height)?;
    let primary = &sources
        .first()
        .expect("every content authors a geometry source")
        .bytes;
    let plant = write_plant_asset(&plant::plant(recipe, primary))?;
    let biome = write_biome_asset(&biome::biome(recipe))?;
    let chunks = biome::map_chunks(recipe);
    let mut encoded_chunks = chunks
        .into_iter()
        .map(|chunk| {
            let bytes = write_vegetation_map_chunk(&chunk)?;
            let reference = VegetationMapChunkReference {
                key: chunk.key,
                content_hash: vegetation_content_hash(&bytes),
                byte_length: u64::try_from(bytes.len())?,
                revision: chunk.revision,
            };
            Ok((reference, bytes))
        })
        .collect::<Result<Vec<_>>>()?;
    encoded_chunks.sort_by_key(|(reference, _)| reference.order_key());
    let root = VegetationMapAsset {
        version: VEGETATION_MAP_VERSION,
        id: recipe.map_id,
        name: "E2E vegetation world".to_owned(),
        bounds: biome::union_bounds(recipe.cells),
        chunk_layout: VegetationMapChunkLayout {
            level: 0,
            schema_hash: vegetation_map_chunk_schema_hash(),
        },
        generation: 1,
        inventory: encoded_chunks
            .iter()
            .map(|(reference, _)| *reference)
            .collect(),
    };
    let map = write_vegetation_map_asset(&root)?;
    let fixture = Fixture {
        format_version: 4,
        stress: recipe.stress.map(str::to_owned),
        cells: if recipe.stress.is_some() {
            recipe.cells.iter().map(|&(x, y, z)| [x, y, z]).collect()
        } else {
            Vec::new()
        },
        plant_hex: hex(&plant),
        biome_hex: hex(&biome),
        map_hex: hex(&map),
        sources: sources
            .iter()
            .map(|file| FixtureSource {
                path: file.path.clone(),
                hex: hex(&file.bytes),
            })
            .collect(),
        map_objects: encoded_chunks
            .into_iter()
            .map(|(reference, bytes)| MapObject {
                content_hash: hex(&reference.content_hash),
                hex: hex(&bytes),
            })
            .collect(),
        plant: recipe.plant_id.value().to_string(),
        biome: recipe.biome_id.value().to_string(),
        map: recipe.map_id.value().to_string(),
        authored_layer: format!("{:032x}", recipe.layer),
        biome_instance: format!("{:032x}", recipe.instance),
        expected_plant: recipe.expected_plant.to_owned(),
        expected_accepted: recipe.expected_accepted.to_owned(),
    };
    let mut bytes = serde_json::to_vec_pretty(&fixture)?;
    bytes.push(b'\n');
    let mut file = AtomicWriteFile::options()
        .open(path)
        .with_context(|| format!("open vegetation E2E fixture {}", path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("write vegetation E2E fixture {}", path.display()))?;
    file.commit()
        .with_context(|| format!("publish vegetation E2E fixture {}", path.display()))
}

fn fixed(value: i32) -> DecisionScalar {
    DecisionScalar::from_integer(value).expect("representable fixture scalar")
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}
