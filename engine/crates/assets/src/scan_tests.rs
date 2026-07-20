//! Scan / load-catalog / cache / texture-register tests.
//!
//! The catalog-reconciliation tests are GPU-free (a `.smodel` written via the bake +
//! container codec). The texture-register tests need a real upload, so they use a
//! headless device when one is present and skip otherwise (the same pattern as the
//! resolve/load tests).

use super::*;
use crate::import::ImportOptions;
use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{ImportedMaterial, ImportedModel, Mesh, Submesh, Vertex};
use saffron_scene::{AssetEntry, AssetType, Colorspace, TextureRole};
use std::path::PathBuf;

/// A unique scratch dir under the system temp, removed and recreated per test.
fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("saffron-assets-scan-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A single-triangle mesh.
fn triangle_mesh() -> Mesh {
    Mesh {
        vertices: vec![
            Vertex {
                position: Vec3::ZERO,
                normal: Vec3::Z,
                uv0: Vec2::ZERO,
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::X,
                normal: Vec3::Z,
                uv0: Vec2::new(1.0, 0.0),
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::Y,
                normal: Vec3::Z,
                uv0: Vec2::new(0.0, 1.0),
                ..Vertex::default()
            },
        ],
        indices: vec![0, 1, 2],
        submeshes: vec![Submesh {
            first_index: 0,
            index_count: 3,
            vertex_offset: 0,
            material_slot: 0,
        }],
    }
}

/// A graph with one material (no textures) so the bake writes a small container.
fn flat_graph() -> ImportedModel {
    ImportedModel {
        nodes: vec![saffron_geometry::ImportedNode {
            name: "mesh".to_owned(),
            mesh: Some(triangle_mesh()),
            ..saffron_geometry::ImportedNode::default()
        }],
        materials: vec![ImportedMaterial {
            name: "flat".to_owned(),
            ..ImportedMaterial::default()
        }],
        animations: Vec::new(),
        skin: None,
        morph: None,
    }
}

/// Bakes a `.smodel` under the asset root (no catalog rows added — the scan rediscovers
/// it) and returns its project-relative path + model id.
fn bake_fixture(assets: &AssetServer, source: &str) -> (Uuid, String) {
    let bake = assets
        .bake_model(&flat_graph(), ImportOptions::default(), source, Uuid(0))
        .expect("bake");
    (bake.model_id, bake.path)
}

#[test]
fn fresh_scan_adds_a_containers_rows() {
    let dir = scratch("freshscan");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    let (model_id, _path) = bake_fixture(&assets, "/tmp/flat.glb");

    // The catalog starts empty; the scan rediscovers the baked container.
    assert!(assets.catalog.entries.is_empty());
    let delta = assets.scan_assets().expect("scan");
    // 1 mesh + 1 material = 2 sub-assets; 3 rows (the Model parent + 2).
    assert_eq!(assets.catalog.entries.len(), 3);
    assert_eq!(delta.added.len(), 3);
    assert!(delta.removed.is_empty());
    let model = assets.catalog.find(model_id).expect("model row");
    assert_eq!(model.asset_type, AssetType::Model);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_after_deleting_the_file_removes_the_rows() {
    let dir = scratch("delete");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    let (model_id, path) = bake_fixture(&assets, "/tmp/flat.glb");

    assets.scan_assets().expect("first scan");
    assert!(assets.catalog.find(model_id).is_some());

    // Delete the file and rescan: every row it contributed is dropped.
    std::fs::remove_file(format!("{}/{path}", root.display())).unwrap();
    let delta = assets.scan_assets().expect("rescan");
    assert!(
        assets.catalog.entries.is_empty(),
        "deleting the file drops its rows"
    );
    assert_eq!(delta.removed.len(), 3);
    assert!(delta.added.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_order_is_deterministic_across_runs() {
    let dir = scratch("order");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    // Several containers so the walk order matters.
    bake_fixture(&assets, "/tmp/a.glb");
    bake_fixture(&assets, "/tmp/b.glb");
    bake_fixture(&assets, "/tmp/c.glb");

    assets.scan_assets().expect("scan");
    let first: Vec<Uuid> = assets.catalog.entries.iter().map(|e| e.id).collect();

    // A fresh server over the same dir scans in the same order.
    let mut again = AssetServer::new(&root);
    again.scan_assets().expect("rescan");
    let second: Vec<Uuid> = again.catalog.entries.iter().map(|e| e.id).collect();
    assert_eq!(first, second, "the sorted walk yields a reproducible order");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_catalog_uses_the_cache_when_the_signature_matches() {
    let dir = scratch("cachehit");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    bake_fixture(&assets, "/tmp/flat.glb");

    // A cold load scans + writes the cache.
    let cold = assets.load_catalog().expect("cold load");
    assert_eq!(cold.added.len(), 3, "the cold path scans");
    assert!(
        root.join(".cache").join("catalog.json").exists(),
        "the cold load writes the cache"
    );
    let cold_ids: Vec<Uuid> = assets.catalog.entries.iter().map(|e| e.id).collect();

    // A second load over an unchanged dir is a cache hit: no scan delta, identical rows.
    let mut warm = AssetServer::new(&root);
    let warm_delta = warm.load_catalog().expect("warm load");
    assert!(warm_delta.added.is_empty(), "a cache hit reports no delta");
    let warm_ids: Vec<Uuid> = warm.catalog.entries.iter().map(|e| e.id).collect();
    assert_eq!(cold_ids, warm_ids, "the cache yields the cold-scan catalog");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_catalog_falls_back_to_a_scan_when_the_dir_changes() {
    let dir = scratch("cachemiss");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    bake_fixture(&assets, "/tmp/flat.glb");
    assets.load_catalog().expect("cold load writes the cache");

    // Add a second container, then load: the signature changed, so it cold-scans (and the
    // new container's rows appear).
    bake_fixture(&assets, "/tmp/second.glb");
    let mut reloaded = AssetServer::new(&root);
    reloaded.load_catalog().expect("reload");
    assert_eq!(
        reloaded.catalog.entries.len(),
        6,
        "both containers are catalogued"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_catalog_falls_back_to_a_clean_scan_on_a_corrupt_cache() {
    let dir = scratch("corruptcache");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    bake_fixture(&assets, "/tmp/flat.glb");

    // A cold load scans and writes the cache; that catalog is the source of truth.
    assets.load_catalog().expect("cold load");
    let baseline: Vec<Uuid> = assets.catalog.entries.iter().map(|e| e.id).collect();
    assert!(!baseline.is_empty());

    // Corrupt the cache file. A fresh server's load must fail to parse it and fall back to a
    // full scan, rebuilding the identical catalog — the cache is never load-bearing.
    std::fs::write(
        root.join(".cache").join("catalog.json"),
        b"{ not valid json ]",
    )
    .unwrap();
    let mut reloaded = AssetServer::new(&root);
    reloaded.load_catalog().expect("reload over corrupt cache");
    let recovered: Vec<Uuid> = reloaded.catalog.entries.iter().map(|e| e.id).collect();
    assert_eq!(
        recovered, baseline,
        "a corrupt cache yields the cold-scan catalog"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_preserves_display_names_across_runs() {
    let dir = scratch("preserve");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    let (model_id, _path) = bake_fixture(&assets, "/tmp/flat.glb");
    assets.scan_assets().expect("first scan");

    // Rename the model row; a rescan keeps the display name (the filesystem refreshes only
    // the path, not the human name).
    assert!(assets.catalog.rename(model_id, "Renamed Model"));
    assets.scan_assets().expect("rescan");
    assert_eq!(assets.catalog.find(model_id).unwrap().name, "Renamed Model");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn foreign_file_gets_a_minted_smeta_and_a_texture_row() {
    let dir = scratch("foreign");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    // A foreign PNG with a non-uuid name dropped into textures/.
    let png = png_2x2();
    let rel = "textures/brick_albedo.png";
    std::fs::write(format!("{}/{rel}", root.display()), &png).unwrap();

    let delta = assets.scan_assets().expect("scan");
    assert_eq!(delta.added.len(), 1, "the foreign file is catalogued");
    // A `.smeta` was minted beside it.
    assert!(std::path::Path::new(&format!("{}/{rel}.smeta", root.display())).exists());
    let row = &delta.added[0];
    assert_eq!(row.asset_type, AssetType::Texture);
    assert_eq!(row.name, "brick_albedo");

    // A rescan reuses the minted `.smeta` id (no second mint, the id is stable).
    let id = row.id;
    let mut again = AssetServer::new(&root);
    again.scan_assets().expect("rescan");
    assert!(
        again.catalog.find(id).is_some(),
        "the minted id is stable across scans"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn native_uuid_texture_recovers_name_and_colorspace_from_its_smeta() {
    let dir = scratch("nativesmeta");
    let root = dir.join("assets");
    std::fs::create_dir_all(root.join("textures")).unwrap();

    // A uuid-named native texture on disk (as an import writes it), with no prior catalog row.
    let id = Uuid::new();
    let rel = format!("textures/{}.png", id.value());
    std::fs::write(format!("{}/{rel}", root.display()), png_2x2()).unwrap();

    // Seed a row + write its durable sidecar (a linear data map), as an import does, then drop
    // that server so the next scan starts cold (empty `previous`, no cache).
    {
        let mut seed = AssetServer::new(&root);
        seed.catalog.put(AssetEntry {
            id,
            name: "Brick Normal".to_owned(),
            asset_type: AssetType::Texture,
            path: rel.clone(),
            linear: true,
            ..AssetEntry::default()
        });
        seed.write_asset_sidecar(id).expect("write sidecar");
    }
    assert!(std::path::Path::new(&format!("{}/{rel}.smeta", root.display())).exists());

    // The cold scan recovers name + colorspace from the sidecar, not the uuid stem / defaults.
    let mut cold = AssetServer::new(&root);
    cold.scan_assets().expect("cold scan");
    let row = cold.catalog.find(id).expect("row");
    assert_eq!(
        row.name, "Brick Normal",
        "the .smeta name beats the uuid stem"
    );
    assert_eq!(
        row.colorspace,
        Colorspace::Linear,
        "linear colorspace survives (no silent revert to sRGB)"
    );
    assert!(row.linear, "the linear flag survives");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_wrong_id_smeta_does_not_bleed_onto_a_path_sharing_row() {
    let dir = scratch("smetaguard");
    let root = dir.join("assets");
    std::fs::create_dir_all(root.join("textures")).unwrap();

    // A native texture whose sibling `.smeta` names a *different* id (as a model sidecar sitting
    // beside an embedded sub-asset would): the id guard must reject it, keeping the uuid-stem name.
    let id = Uuid::new();
    let rel = format!("textures/{}.png", id.value());
    std::fs::write(format!("{}/{rel}", root.display()), png_2x2()).unwrap();
    {
        let other = Uuid::new();
        let mut seed = AssetServer::new(&root);
        seed.catalog.put(AssetEntry {
            id: other,
            name: "Someone Else".to_owned(),
            asset_type: AssetType::Texture,
            path: rel.clone(),
            ..AssetEntry::default()
        });
        seed.write_asset_sidecar(other).expect("write sidecar");
    }

    let mut cold = AssetServer::new(&root);
    cold.scan_assets().expect("cold scan");
    let row = cold.catalog.find(id).expect("row");
    assert_eq!(
        row.name,
        id.value().to_string(),
        "an id-mismatched sidecar is ignored; the row keeps its stem name"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cold_scan_recovers_renamed_model_and_extracted_subasset_names_from_sidecars() {
    // A never-saved rename survives a cold scan because the name lives in a co-located `.smeta`
    // sidecar, not only in project.json — across a `.smodel` model row and an extracted sub-asset.
    let dir = scratch("durablenames");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    let (model_id, _path) = bake_fixture(&assets, "/tmp/flat.glb");
    assets.scan_assets().expect("scan");

    let material_sub = assets
        .catalog
        .entries
        .iter()
        .find(|e| e.asset_type == AssetType::Material && e.container == model_id)
        .map(|e| e.id)
        .expect("embedded material sub-asset");

    // Rename the model row and write its durable sidecar (as a rename command does).
    assert!(assets.catalog.rename(model_id, "Hero"));
    assets.write_asset_sidecar(model_id).expect("model sidecar");

    // Extract the embedded material to a standalone `.smat`, rename it, and write its sidecar.
    let extracted =
        crate::manage::extract_sub_asset(&mut assets, model_id, material_sub, "").expect("extract");
    assert!(assets.catalog.rename(extracted, "Brass"));
    assets
        .write_asset_sidecar(extracted)
        .expect("sub-asset sidecar");

    // A cold scan (fresh server, no cache, empty previous) recovers both names from the sidecars
    // alone — the "imported asset came back as a bare uuid" regression guard.
    let mut cold = AssetServer::new(&root);
    cold.scan_assets().expect("cold scan");
    assert_eq!(cold.catalog.find(model_id).expect("model row").name, "Hero");
    assert_eq!(
        cold.catalog.find(extracted).expect("extracted row").name,
        "Brass"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn detect_material_role_classifies_filenames() {
    assert_eq!(detect_material_role("rock_ARM.png"), "orm");
    assert_eq!(detect_material_role("wood_orm.jpg"), "orm");
    assert_eq!(detect_material_role("brick_BaseColor.png"), "albedo");
    assert_eq!(detect_material_role("metal_diffuse.tga"), "albedo");
    // Normal is matched before the packed `orm`/`arm`/`mra` acronyms, so a literal "normal"
    // name (which contains the substring "orm") classifies as "normal", as do the `_nor`/`nrm`
    // tokens. This is what lets ambientCG's `*_NormalGL` land in the normal slot.
    assert_eq!(detect_material_role("stone_nor.png"), "normal");
    assert_eq!(detect_material_role("floor_nrm.png"), "normal");
    assert_eq!(detect_material_role("literal_normal.png"), "normal");
    assert_eq!(
        detect_material_role("Rock063_2K-PNG_NormalGL.png"),
        "normal"
    );
    assert_eq!(detect_material_role("surface_roughness.png"), "roughness");
    assert_eq!(detect_material_role("plate_metallic.png"), "metallic");
    assert_eq!(detect_material_role("lava_emissive.png"), "emissive");
    assert_eq!(detect_material_role("wall_height.png"), "height");
    assert_eq!(detect_material_role("crate_AO.png"), "ao");
    assert_eq!(detect_material_role("shiny_gloss.png"), "gloss");
    assert_eq!(detect_material_role("glass_opacity.png"), "opacity");
    assert_eq!(detect_material_role("random_texture.png"), "");
}

/// The height-map technique routing: an imported map is **never** auto-routed to Displacement (that
/// costs tessellation + a per-frame BLAS, so it is a deliberate editor choice); an explicit bump map →
/// shading bump, every other height map → parallax.
#[test]
fn detect_height_mode_never_auto_routes_displacement() {
    use saffron_core::HeightMode;
    // A provider Displacement map imports as Parallax — the user promotes it in the editor's dropdown.
    assert_eq!(
        detect_height_mode("Rock063_2K-PNG_Displacement.png"),
        HeightMode::Parallax
    );
    assert_eq!(detect_height_mode("wood_disp.png"), HeightMode::Parallax);
    assert_eq!(detect_height_mode("brick_bump.png"), HeightMode::Bump);
    assert_eq!(detect_height_mode("stone_height.png"), HeightMode::Parallax);
}

/// A 2x2 RGBA8 PNG (the encoded bytes the texture register decodes).
fn png_2x2() -> Vec<u8> {
    let buffer = image::RgbaImage::from_pixel(2, 2, image::Rgba([180, 120, 60, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    buffer
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

// Texture-register tests (need a real upload; skip off-hardware).

use crate::RendererUploader;
use saffron_rendering::{
    BindlessFreeList, Descriptors, Device, SurfaceSource, Uploader, validation_issue_count,
};

struct GpuFixture {
    uploader: Uploader,
    descriptors: Descriptors,
    device: Device,
}

fn gpu_or_skip() -> Option<GpuFixture> {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping (no Vulkan device): {err}");
            return None;
        }
    };
    let free_list: BindlessFreeList = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
    let queue = device.graphics_queue.clone();
    let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
    Some(GpuFixture {
        uploader,
        descriptors,
        device,
    })
}

impl GpuFixture {
    fn teardown(self, mut assets: AssetServer) {
        let GpuFixture {
            device,
            descriptors,
            uploader,
        } = self;
        device.wait_idle().expect("idle before teardown");
        assets.clear_asset_caches();
        drop(assets);
        drop(uploader);
        drop(descriptors);
        drop(device);
    }
}

#[test]
fn register_texture_bytes_writes_the_file_adds_a_row_and_seeds_the_cache() {
    let Some(fx) = gpu_or_skip() else {
        return;
    };
    let before = validation_issue_count();
    let dir = scratch("register");
    let root = dir.join("project").join("assets");
    let mut assets = AssetServer::new(&root);

    let gpu = RendererUploader::new(&fx.uploader, &fx.descriptors, true);
    let png = png_2x2();
    let id = assets
        .register_texture_bytes(&gpu, &png, "png", "brick", true, TextureRole::Unknown)
        .expect("register");

    // The file landed under textures/<uuid>.png with the exact encoded bytes.
    let rel = format!("textures/{}.png", id.value());
    let on_disk = std::fs::read(format!("{}/{rel}", root.display())).expect("written file");
    assert_eq!(on_disk, png, "the encoded bytes are written verbatim");

    // A Texture catalog row exists with the uniqued name.
    let row = assets.catalog.find(id).expect("texture row");
    assert_eq!(row.asset_type, AssetType::Texture);
    assert_eq!(row.name, "brick");
    assert_eq!(row.path, rel);
    assert!(!row.linear, "sRGB upload sets linear = false");

    // The GPU texture cache is seeded with a live Arc (no re-resolve needed).
    assert!(
        matches!(assets.texture_by_uuid.get(&id.value()), Some(Some(_))),
        "the just-uploaded texture is seeded in the cache"
    );

    fx.teardown(assets);
    assert_eq!(
        before,
        validation_issue_count(),
        "upload is validation-clean"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_texture_reads_a_file_and_registers_it() {
    let Some(fx) = gpu_or_skip() else {
        return;
    };
    let dir = scratch("importtex");
    let root = dir.join("project").join("assets");
    let mut assets = AssetServer::new(&root);
    // An external PNG outside the asset dir.
    let external = dir.join("external_albedo.png");
    std::fs::write(&external, png_2x2()).unwrap();

    let gpu = RendererUploader::new(&fx.uploader, &fx.descriptors, true);
    let id = assets
        .import_texture(&gpu, external.to_str().unwrap(), None, TextureRole::Unknown)
        .expect("import");
    let row = assets.catalog.find(id).expect("row");
    assert_eq!(row.name, "external_albedo", "the name is the filename stem");
    assert_eq!(row.asset_type, AssetType::Texture);
    assert_eq!(
        row.role,
        TextureRole::Albedo,
        "the role is inferred from the '_albedo' filename token"
    );
    assert!(matches!(
        assets.texture_by_uuid.get(&id.value()),
        Some(Some(_))
    ));

    fx.teardown(assets);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_texture_writes_a_durable_smeta_recovered_on_a_cold_scan() {
    let Some(fx) = gpu_or_skip() else {
        return;
    };
    let dir = scratch("importsmeta");
    let root = dir.join("project").join("assets");
    let mut assets = AssetServer::new(&root);
    // A normal map imported linear: the sidecar must pin both name and the linear colorspace.
    let external = dir.join("brick_nor.png");
    std::fs::write(&external, png_2x2()).unwrap();

    let gpu = RendererUploader::new(&fx.uploader, &fx.descriptors, true);
    let id = assets
        .import_texture(
            &gpu,
            external.to_str().unwrap(),
            Some(Colorspace::Linear),
            TextureRole::Normal,
        )
        .expect("import");

    let rel = format!("textures/{}.png", id.value());
    assert!(
        std::path::Path::new(&format!("{}/{rel}.smeta", root.display())).exists(),
        "import writes a co-located .smeta"
    );

    // A cold scan (fresh server, no cache) recovers the name + linear colorspace + role from the
    // sidecar.
    let mut cold = AssetServer::new(&root);
    cold.scan_assets().expect("cold scan");
    let row = cold.catalog.find(id).expect("row");
    assert_eq!(row.name, "brick_nor");
    assert_eq!(row.colorspace, Colorspace::Linear);
    assert!(row.linear);
    assert_eq!(
        row.role,
        TextureRole::Normal,
        "the normal-map role survives a cold scan via the .smeta"
    );

    fx.teardown(assets);
    let _ = std::fs::remove_dir_all(&dir);
}
