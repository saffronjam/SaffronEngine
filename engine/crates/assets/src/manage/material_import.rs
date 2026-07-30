use saffron_core::Uuid;

use crate::import::MaterialMap;
use crate::scan::{detect_height_mode, detect_material_role};
use crate::{AssetServer, Error, Result};

/// The result of a folder material import: the saved material's id plus the space-joined
/// detected roles (for the editor's confirmation proposal).
#[derive(Clone, Debug, Default)]
pub struct MaterialImportResult {
    /// The saved `.smat` material id.
    pub material: Uuid,
    /// Space-joined detected map roles.
    pub roles: String,
}

/// Drag-a-folder material import: scans `dir` for textures, detects each map's role by
/// filename, and bakes one self-contained material container (`materials/<id>.smatx`) that
/// embeds every map as a texture chunk (see [`AssetServer::bake_material_container`]). The
/// result is a single Material tile whose maps are hidden sub-assets — extractable on demand
/// — not a flood of loose textures. Normal maps assume OpenGL convention; a packed ARM/ORM
/// also feeds the occlusion slot.
///
/// # Errors
///
/// [`Error::Io`] if `dir` is not a directory; propagates the container-bake failure.
pub fn import_material_folder(
    assets: &mut AssetServer,
    dir: &str,
    name: &str,
) -> Result<MaterialImportResult> {
    let dir_path = std::path::Path::new(dir);
    if !dir_path.is_dir() {
        return Err(Error::Io(format!("not a directory: {dir}")));
    }

    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir_path)
        .map_err(|e| Error::Io(e.to_string()))?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    files.sort();

    let mut maps = Vec::new();
    for path in &files {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "tga") {
            continue;
        }
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let role = detect_material_role(filename);
        if role.is_empty() {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        maps.push(MaterialMap {
            role: role.to_owned(),
            // Only the `height` role reads this; the filename decides the technique.
            height_mode: detect_height_mode(filename),
            bytes,
        });
    }

    let mut material_name = name.to_owned();
    if material_name.is_empty() {
        material_name = dir_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
    }
    if material_name.is_empty() {
        material_name = "Material".to_owned();
    }

    let baked = assets.bake_material_container(&material_name, &maps)?;
    let material_id = baked.material_id;
    let unique = assets.catalog.unique_name(&material_name);
    for mut row in baked.rows {
        // The parent material row wears the catalog-unique display name; sub-texture rows
        // keep their baked `<name> <role>` names.
        if row.id == material_id {
            row.name = unique.clone();
        }
        assets.register_imported_asset(row);
    }
    Ok(MaterialImportResult {
        material: material_id,
        roles: baked.roles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use saffron_scene::{AssetEntry, AssetType};

    use crate::material::MaterialAsset;

    use super::super::extract::extract_sub_asset;
    use super::super::references::asset_bytes;
    use super::super::test_support::{png_2x2, scratch};

    #[test]
    fn import_material_folder_rejects_a_non_directory() {
        let dir = scratch("matfolder-bad");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        let err = import_material_folder(&mut assets, "/no/such/dir", "Mat")
            .expect_err("not a directory");
        assert!(matches!(err, Error::Io(_)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A material import bakes ONE self-contained `.smatx` container: the parent Material row
    /// self-references (`container == id`, so the frontend still shows it) and every map is a
    /// hidden texture sub-row (`container == material_id`). The `.smat` + textures load back
    /// from the container chunks (pure disk), the ambientCG `NormalGL` lands in the normal
    /// slot and `Displacement` in height, and extract pulls one map out to a standalone file.
    #[test]
    fn import_material_folder_bakes_a_self_contained_container() {
        let dir = scratch("matfolder");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        // An ambientCG-style map set — the exact `*_NormalGL` / `*_Displacement` names the
        // store extracts, exercising the real role routing end to end.
        let folder = dir.join("Rock063");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Rock063_2K-PNG_Color.png"), png_2x2()).unwrap();
        std::fs::write(folder.join("Rock063_2K-PNG_NormalGL.png"), png_2x2()).unwrap();
        std::fs::write(folder.join("Rock063_2K-PNG_Roughness.png"), png_2x2()).unwrap();
        std::fs::write(
            folder.join("Rock063_2K-PNG_AmbientOcclusion.png"),
            png_2x2(),
        )
        .unwrap();
        std::fs::write(folder.join("Rock063_2K-PNG_Displacement.png"), png_2x2()).unwrap();

        let result = import_material_folder(&mut assets, &folder.to_string_lossy(), "Rock 063")
            .expect("import");
        assert!(result.roles.contains("albedo"));
        assert!(result.roles.contains("normal"));
        assert!(result.roles.contains("roughness"));
        assert!(result.roles.contains("height"));

        let entry = assets.catalog.find(result.material).expect("material row");
        assert_eq!(entry.asset_type, AssetType::Material);
        assert_eq!(
            entry.container, result.material,
            "the material self-references so the frontend `container == id` rule shows it"
        );
        let subs: Vec<AssetEntry> = assets
            .catalog
            .entries
            .iter()
            .filter(|e| e.container == result.material && e.id != result.material)
            .cloned()
            .collect();
        assert_eq!(subs.len(), 5, "five hidden texture sub-rows (one per map)");
        assert!(
            subs.iter().all(|e| e.asset_type == AssetType::Texture),
            "every sub-row is a texture"
        );

        let material = crate::material::load_catalog_material_asset(&mut assets, result.material)
            .expect("load");
        assert_ne!(material.albedo_texture.value(), 0);
        assert_ne!(material.normal_texture.value(), 0);
        assert_ne!(material.orm_texture.value(), 0);
        assert_ne!(material.height_texture.value(), 0);
        // An imported height map is never auto-routed to Displacement (that costs tessellation + a
        // per-frame BLAS) — it imports as Parallax; the user promotes it in the editor's `heightMode`
        // dropdown when a true displaced silhouette is wanted.
        assert_eq!(material.height_mode, saffron_core::HeightMode::Parallax);
        assert!(
            subs.iter().any(|e| e.id == material.normal_texture),
            "the normal slot points at an embedded sub-texture"
        );

        for sub in &subs {
            assert!(
                asset_bytes(&mut assets, sub) > 0,
                "embedded texture chunk has bytes"
            );
        }

        let _ = assets.scan_assets();
        let reloaded = assets
            .catalog
            .find(result.material)
            .expect("material survives a rescan");
        assert_eq!(
            reloaded.container, result.material,
            "still a self-container after reload"
        );
        assert_eq!(
            assets
                .catalog
                .entries
                .iter()
                .filter(|e| e.container == result.material && e.id != result.material)
                .count(),
            5,
            "the five embedded map rows are re-derived from the container"
        );

        extract_sub_asset(&mut assets, result.material, material.normal_texture, "")
            .expect("extract normal");
        let extracted = assets.catalog.find(material.normal_texture).expect("row");
        assert_eq!(
            extracted.container,
            Uuid(0),
            "extraction makes the map a standalone file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Editing an imported (self-container `.smatx`) material rewrites only its material chunk:
    /// the edited fields persist on reload, the row stays a self-container, and every embedded
    /// texture chunk survives — the mutation path is container-aware, not a blind `fs::write`
    /// that would clobber the container framing.
    #[test]
    fn update_material_asset_rewrites_a_container_material_in_place() {
        let dir = scratch("matupdate-container");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        let folder = dir.join("Rock063");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Rock063_2K-PNG_Color.png"), png_2x2()).unwrap();
        std::fs::write(folder.join("Rock063_2K-PNG_NormalGL.png"), png_2x2()).unwrap();
        std::fs::write(folder.join("Rock063_2K-PNG_Roughness.png"), png_2x2()).unwrap();
        std::fs::write(folder.join("Rock063_2K-PNG_Displacement.png"), png_2x2()).unwrap();
        let material_id =
            import_material_folder(&mut assets, &folder.to_string_lossy(), "Rock 063")
                .expect("import")
                .material;

        let subs: Vec<Uuid> = assets
            .catalog
            .entries
            .iter()
            .filter(|e| e.container == material_id && e.id != material_id)
            .map(|e| e.id)
            .collect();
        assert_eq!(
            subs.len(),
            4,
            "four embedded map sub-rows (one per source map)"
        );

        let mut material =
            crate::material::load_catalog_material_asset(&mut assets, material_id).expect("load");
        let original_normal = material.normal_texture;
        assert!(
            (material.height_scale - 0.05).abs() < 1e-6,
            "imports at the default amplitude"
        );
        material.height_scale = 0.5;
        material.base_color = saffron_geometry::glam::Vec4::new(0.1, 0.2, 0.3, 1.0);
        crate::material::update_material_asset(&mut assets, material_id, &material)
            .expect("update the container material");

        let reloaded =
            crate::material::load_catalog_material_asset(&mut assets, material_id).expect("reload");
        assert!(
            (reloaded.height_scale - 0.5).abs() < 1e-6,
            "height_scale persisted through the container rewrite"
        );
        assert!(
            (reloaded.base_color.x - 0.1).abs() < 1e-6,
            "base_color persisted"
        );
        assert_eq!(
            reloaded.normal_texture, original_normal,
            "the normal slot is unchanged"
        );
        assert_eq!(
            assets.catalog.find(material_id).unwrap().container,
            material_id,
            "still a self-container after the edit"
        );

        for sub_id in &subs {
            let sub = assets.catalog.find(*sub_id).expect("sub row").clone();
            assert!(
                asset_bytes(&mut assets, &sub) > 0,
                "embedded texture survives the rewrite"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A standalone `.smat` (`container == 0`) round-trips through the plain `fs::write` branch.
    #[test]
    fn update_material_asset_still_rewrites_a_standalone_smat() {
        let dir = scratch("matupdate-standalone");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        let material = MaterialAsset {
            roughness: 0.2,
            ..MaterialAsset::default()
        };
        let id = crate::material::save_material_asset(&mut assets, &material, "Plain", "")
            .expect("save");
        assert_eq!(
            assets.catalog.find(id).unwrap().container.value(),
            0,
            "a fresh `.smat` is standalone"
        );

        let edited = MaterialAsset {
            roughness: 0.9,
            ..material
        };
        crate::material::update_material_asset(&mut assets, id, &edited)
            .expect("update standalone");
        let reloaded =
            crate::material::load_catalog_material_asset(&mut assets, id).expect("reload");
        assert!(
            (reloaded.roughness - 0.9).abs() < 1e-6,
            "the standalone edit persisted"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
