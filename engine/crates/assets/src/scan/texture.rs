//! Registering standalone textures: decode, upload, write the file, catalog the row.

use saffron_core::Uuid;
use saffron_geometry::{decode_image_from_memory, decode_image_from_memory_hdr};
use saffron_scene::{AssetEntry, AssetType, Colorspace, TextureRole};

use crate::AssetServer;
use crate::error::{Error, Result};
use crate::gpu::GpuUploader;

use super::roles::infer_texture_role;

/// The fields of a standalone Texture catalog row, as [`AssetServer::put_texture_row`] takes them.
struct TextureRowSpec<'a> {
    name: &'a str,
    path: String,
    hdr: bool,
    linear: bool,
    content_hash: u64,
    role: TextureRole,
}

impl AssetServer {
    /// Decodes + uploads `encoded` (RGBA8) as a `srgb`/unorm texture, writes the bytes to
    /// `textures/<uuid>.<ext>`, adds a [`AssetType::Texture`] catalog row, and seeds the
    /// GPU texture cache. Returns the minted id.
    ///
    /// # Errors
    ///
    /// [`Error::Geometry`] if the bytes do not decode, [`Error::Render`] if the upload
    /// fails, [`Error::Io`] if the file cannot be written.
    pub fn register_texture_bytes(
        &mut self,
        gpu: &dyn GpuUploader,
        encoded: &[u8],
        ext: &str,
        name: &str,
        srgb: bool,
        role: TextureRole,
    ) -> Result<Uuid> {
        let decoded = decode_image_from_memory(encoded)?;
        let texture = gpu.upload_texture(&decoded.rgba, decoded.width, decoded.height, srgb)?;
        let id = Uuid::new();
        let extension = if ext.is_empty() { "png" } else { ext };
        self.ensure_asset_directories();
        let relative_path = format!("textures/{}.{extension}", id.value());
        std::fs::write(format!("{}/{relative_path}", self.root.display()), encoded)
            .map_err(|e| Error::Io(format!("cannot write texture '{relative_path}': {e}")))?;
        self.put_texture_row(
            id,
            TextureRowSpec {
                name,
                path: relative_path,
                hdr: false,
                linear: !srgb,
                content_hash: crate::import::hash_bytes_fnv(encoded),
                role,
            },
        );
        self.texture_by_uuid.insert(id.value(), Some(texture));
        Ok(id)
    }

    /// Decodes + uploads `encoded` HDR bytes as a linear float texture, writes them to
    /// `textures/<uuid>.hdr`, adds a Texture row with `hdr = true`, and seeds the cache.
    /// Returns the minted id.
    ///
    /// # Errors
    ///
    /// [`Error::Geometry`] if the bytes do not decode, [`Error::Render`] if the upload
    /// fails, [`Error::Io`] if the file cannot be written.
    pub fn register_hdr_texture_bytes(
        &mut self,
        gpu: &dyn GpuUploader,
        encoded: &[u8],
        name: &str,
    ) -> Result<Uuid> {
        let decoded = decode_image_from_memory_hdr(encoded)?;
        let texture = gpu.upload_texture_float(&decoded.rgba, decoded.width, decoded.height)?;
        let id = Uuid::new();
        self.ensure_asset_directories();
        let relative_path = format!("textures/{}.hdr", id.value());
        std::fs::write(format!("{}/{relative_path}", self.root.display()), encoded)
            .map_err(|e| Error::Io(format!("cannot write texture '{relative_path}': {e}")))?;
        self.put_texture_row(
            id,
            TextureRowSpec {
                name,
                path: relative_path,
                hdr: true,
                linear: false,
                content_hash: crate::import::hash_bytes_fnv(encoded),
                role: TextureRole::Hdri,
            },
        );
        self.texture_by_uuid.insert(id.value(), Some(texture));
        Ok(id)
    }

    /// Imports an external image file into the asset dir + catalog (name = filename stem),
    /// dispatching `.hdr` to the float register path.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be read, plus any
    /// [`Self::register_texture_bytes`] / [`Self::register_hdr_texture_bytes`] error.
    pub fn import_texture(
        &mut self,
        gpu: &dyn GpuUploader,
        path: &str,
        colorspace: Option<Colorspace>,
        role: TextureRole,
    ) -> Result<Uuid> {
        let encoded =
            std::fs::read(path).map_err(|e| Error::Io(format!("cannot open '{path}': {e}")))?;
        let file = std::path::Path::new(path);
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        // The caller resolves `colorspace` (an explicit override, or the role-derived policy).
        // `None` means neither was given (a plain manual import): dispatch `.hdr` to the float
        // path, else sRGB — the prior heuristic. `role` still rides through for preview routing.
        match colorspace {
            Some(Colorspace::Linear) => {
                self.register_texture_bytes(gpu, &encoded, ext, stem, false, role)
            }
            Some(Colorspace::Srgb) => {
                self.register_texture_bytes(gpu, &encoded, ext, stem, true, role)
            }
            Some(Colorspace::Hdr) => self.register_hdr_texture_bytes(gpu, &encoded, stem),
            _ if ext.eq_ignore_ascii_case("hdr") => {
                self.register_hdr_texture_bytes(gpu, &encoded, stem)
            }
            _ => self.register_texture_bytes(gpu, &encoded, ext, stem, true, role),
        }
    }

    /// Inserts a standalone Texture row with a name uniqued against the live catalog. A
    /// [`TextureRole::Unknown`] role is inferred from the (pre-unique) filename + HDR-ness;
    /// an explicit role (a connector, or the HDR path) is kept verbatim.
    fn put_texture_row(&mut self, id: Uuid, spec: TextureRowSpec<'_>) {
        let role = if spec.role == TextureRole::Unknown {
            infer_texture_role(spec.name, spec.hdr)
        } else {
            spec.role
        };
        let unique = self.catalog.unique_name(spec.name);
        self.register_imported_asset(AssetEntry {
            id,
            name: unique,
            asset_type: AssetType::Texture,
            path: spec.path,
            hdr: spec.hdr,
            linear: spec.linear,
            content_hash: spec.content_hash,
            role,
            ..AssetEntry::default()
        });
        if let Err(err) = self.write_asset_sidecar(id) {
            tracing::warn!(
                "import: could not write texture .smeta for {}: {err}",
                id.value()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use saffron_rendering::validation_issue_count;

    use super::*;

    use crate::RendererUploader;

    use super::super::test_support::{gpu_or_skip, png_2x2, scratch};

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
}
