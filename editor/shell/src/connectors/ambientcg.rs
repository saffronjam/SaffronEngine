//! The ambientCG connector — keyless (`AuthKind::None`), CC0 PBR materials and HDRIs. The
//! `full_json` API delivers a material as a single zip of role-named maps (which the host's
//! `import_material_folder` consumes after extraction) and an HDRI as a single file.
//!
//! Imports default to 2K; a chosen resolution swaps the download's `_<n>K` token.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::{
    AssetPart, AuthKind, ConnectorError, ResourceCache, SearchPage, SearchQuery, StoreConnector,
    StoreCursor, StoreImportDescriptor, StoreKind, StoreLicense, StoreRef, StoreResult,
    extract_zip,
};

const API_BASE: &str = "https://ambientcg.com/api/v2/full_json";
const PAGE: usize = 24;

/// Maps an ambientCG `maps` role → (colorspace role, label, filename token to match in the zip).
fn role_of(map: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match map {
        "color" => Some(("color", "Color", "color")),
        "normal" => Some(("normal", "Normal", "normalgl")),
        "roughness" => Some(("roughness", "Roughness", "roughness")),
        "metalness" => Some(("metallic", "Metalness", "metalness")),
        "ambient-occlusion" => Some(("ao", "Ambient occlusion", "ambientocclusion")),
        "displacement" => Some(("displacement", "Displacement", "displacement")),
        _ => None,
    }
}

pub struct AmbientCg {
    cache: Arc<ResourceCache>,
}

impl AmbientCg {
    pub fn new(cache: Arc<ResourceCache>) -> Self {
        Self { cache }
    }

    /// Fetch `requested`, falling back to `fallback` if that resolution variant is missing.
    /// Returns the url actually fetched plus its bytes.
    async fn fetch_with_fallback(
        &self,
        requested: &str,
        fallback: &str,
        progress: &super::ProgressFn<'_>,
    ) -> Result<(String, Vec<u8>), ConnectorError> {
        match self.cache.fetch(requested, Some(progress)).await {
            Ok((bytes, _)) => Ok((requested.to_owned(), bytes)),
            Err(_) if requested != fallback => {
                let (bytes, _) = self.cache.fetch(fallback, Some(progress)).await?;
                Ok((fallback.to_owned(), bytes))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn cc0() -> StoreLicense {
        StoreLicense {
            id: "cc0".to_owned(),
            requires_attribution: false,
            url: "https://creativecommons.org/publicdomain/zero/1.0/".to_owned(),
        }
    }

    fn to_result(&self, item: &Value, kind: StoreKind) -> Option<StoreResult> {
        let asset_id = item.get("assetId").and_then(Value::as_str)?;
        // Walk downloadFolders → categories → downloads, choosing a default resolution.
        let downloads: Vec<&Value> = item
            .get("downloadFolders")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|folders| folders.values())
            .filter_map(|f| f.get("downloadFiletypeCategories"))
            .filter_map(Value::as_object)
            .flat_map(|cats| cats.values())
            .filter_map(|cat| cat.get("downloads").and_then(Value::as_array))
            .flatten()
            .collect();
        let chosen = pick_download(&downloads)?;
        let link = chosen.get("downloadLink").and_then(Value::as_str)?;
        let attribute = chosen
            .get("attribute")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let is_zip = chosen
            .get("fileType")
            .and_then(Value::as_str)
            .map(|t| t.eq_ignore_ascii_case("zip"))
            .unwrap_or(link.to_lowercase().ends_with(".zip"));

        // An HDRI is a single environment file; when ambientCG ships it inside a zip we still
        // must hand the importer the extracted `.hdr`/`.exr`, not the folder (which the
        // material importer wants). `hdri-zip` carries that distinction into `download`.
        let format = if kind == StoreKind::Hdri {
            if is_zip { "hdri-zip" } else { "hdr" }.to_owned()
        } else {
            "texture-zip".to_owned()
        };

        Some(StoreResult {
            id: asset_id.to_owned(),
            store: StoreRef {
                id: self.id().to_owned(),
                display_name: self.display_name().to_owned(),
            },
            kind,
            name: item
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(asset_id)
                .to_owned(),
            author: "ambientCG".to_owned(),
            thumbnail_url: preview_url(item, asset_id),
            source_url: format!("https://ambientcg.com/view?id={asset_id}"),
            license: Self::cc0(),
            import_descriptor: StoreImportDescriptor {
                format,
                ref_: link.to_owned(),
                resolution: None,
            },
            // Materials/textures expose their PBR maps; HDRIs are a single file.
            has_parts: matches!(kind, StoreKind::Material | StoreKind::Texture),
            supports_resolution: true,
            published_at: None,
            updated_at: None,
            tri_count: None,
            file_size: None,
            resolution: (!attribute.is_empty()).then_some(attribute),
            tags: None,
        })
    }
}

#[async_trait]
impl StoreConnector for AmbientCg {
    fn id(&self) -> &'static str {
        "ambientcg"
    }

    fn display_name(&self) -> &'static str {
        "ambientCG"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::None
    }

    fn description(&self) -> &'static str {
        "2000+ free CC0 PBR materials and HDRIs — no account needed."
    }

    fn website(&self) -> &'static str {
        "https://ambientcg.com"
    }

    async fn search(
        &self,
        query: &SearchQuery,
        cursor: Option<StoreCursor>,
    ) -> Result<SearchPage, ConnectorError> {
        // ambientCG has no models; map the kind to its asset type (default: materials).
        let (api_type, kind) = match query.kind {
            Some(StoreKind::Model) => {
                return Ok(SearchPage {
                    results: Vec::new(),
                    next_cursor: None,
                    exhausted: true,
                });
            }
            Some(StoreKind::Hdri) => ("HDRI", StoreKind::Hdri),
            Some(StoreKind::Texture) => ("Material", StoreKind::Texture),
            _ => ("Material", StoreKind::Material),
        };
        let offset: usize = cursor.as_ref().and_then(|c| c.0.parse().ok()).unwrap_or(0);
        let resp = self
            .cache
            .client()
            .get(API_BASE)
            .query(&[
                ("type", api_type),
                ("include", "downloadData"),
                ("limit", &PAGE.to_string()),
                ("offset", &offset.to_string()),
                ("q", query.text.trim()),
                ("sort", "popular"),
            ])
            .send()
            .await
            .map_err(|e| ConnectorError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ConnectorError::Http(format!("{}", resp.status())));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ConnectorError::Decode(e.to_string()))?;
        let found = body
            .get("foundAssets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let got = found.len();
        let results: Vec<StoreResult> = found
            .iter()
            .filter_map(|it| self.to_result(it, kind))
            .collect();
        let exhausted = got < PAGE;
        Ok(SearchPage {
            results,
            next_cursor: (!exhausted).then(|| StoreCursor((offset + PAGE).to_string())),
            exhausted,
        })
    }

    async fn download(
        &self,
        descriptor: &StoreImportDescriptor,
        progress: &super::ProgressFn,
    ) -> Result<PathBuf, ConnectorError> {
        // Honor the chosen resolution by swapping the URL's resolution token; fall back to
        // the default link if that variant doesn't exist for this asset.
        let requested = match &descriptor.resolution {
            Some(res) => url_with_resolution(&descriptor.ref_, res),
            None => descriptor.ref_.clone(),
        };
        match descriptor.format.as_str() {
            // A material map set: extract into a cached folder the host's material importer scans.
            "texture-zip" => {
                let derived = self
                    .cache
                    .derived(&format!("acg-set-{}", stable_id(&requested)));
                if derived.exists() {
                    progress(1.0);
                } else {
                    let (_, bytes) = self
                        .fetch_with_fallback(&requested, &descriptor.ref_, progress)
                        .await?;
                    let dir = derived.begin()?;
                    extract_zip(&bytes, &dir)?;
                    derived.commit()?;
                }
                Ok(derived.path().to_path_buf())
            }
            // An HDRI zipped: extract (cached) and return the single environment file inside.
            "hdri-zip" => {
                let derived = self
                    .cache
                    .derived(&format!("acg-hdri-{}", stable_id(&requested)));
                if derived.exists() {
                    progress(1.0);
                } else {
                    let (_, bytes) = self
                        .fetch_with_fallback(&requested, &descriptor.ref_, progress)
                        .await?;
                    let dir = derived.begin()?;
                    extract_zip(&bytes, &dir)?;
                    derived.commit()?;
                }
                std::fs::read_dir(derived.path())
                    .map_err(|e| ConnectorError::Download(e.to_string()))?
                    .flatten()
                    .map(|e| e.path())
                    .find(|p| {
                        p.extension().and_then(|x| x.to_str()).is_some_and(|x| {
                            matches!(x.to_ascii_lowercase().as_str(), "hdr" | "exr")
                        })
                    })
                    .ok_or_else(|| {
                        ConnectorError::Download("no .hdr/.exr in HDRI archive".to_owned())
                    })
            }
            // A direct HDRI file — blob-cached (fetch-once), with the resolution fallback.
            _ => match self.cache.file(&requested, "hdr", Some(progress)).await {
                Ok(p) => Ok(p),
                Err(_) if requested != descriptor.ref_ => self
                    .cache
                    .file(&descriptor.ref_, "hdr", Some(progress))
                    .await
                    .map_err(Into::into),
                Err(e) => Err(e.into()),
            },
        }
    }

    async fn parts(&self, result: &StoreResult) -> Result<Vec<AssetPart>, ConnectorError> {
        // The whole-asset import already picked a resolution zip; reuse it as the bundle so
        // every chosen map is served from a single cached download.
        let bundle = result.import_descriptor.ref_.clone();
        if bundle.is_empty() {
            return Ok(Vec::new());
        }
        let resp = self
            .cache
            .client()
            .get(API_BASE)
            .query(&[("id", result.id.as_str()), ("include", "downloadData")])
            .send()
            .await
            .map_err(|e| ConnectorError::Http(e.to_string()))?;
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ConnectorError::Decode(e.to_string()))?;
        let maps = body
            .get("foundAssets")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|a| a.get("maps"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let parts = maps
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|m| role_of(m).map(|r| (m, r)))
            .map(|(m, (role, label, token))| AssetPart {
                id: format!("{}:{m}", result.id),
                label: label.to_owned(),
                import_kind: StoreKind::Texture,
                role: Some(role.to_owned()),
                resolution: result.resolution.clone(),
                format: None,
                size: None,
                ref_: token.to_owned(),
                bundle: Some(bundle.clone()),
            })
            .collect();
        Ok(parts)
    }

    async fn download_part(
        &self,
        part: &AssetPart,
        resolution: Option<&str>,
    ) -> Result<PathBuf, ConnectorError> {
        let bundle0 = part
            .bundle
            .as_deref()
            .ok_or_else(|| ConnectorError::Download("part has no bundle".to_owned()))?;
        // Map the chosen resolution onto the bundle zip; fall back if that variant is absent.
        let requested = match resolution {
            Some(res) => url_with_resolution(bundle0, res),
            None => bundle0.to_owned(),
        };
        let (bundle, zip) = match self.cache.file(&requested, "zip", None).await {
            Ok(z) => (requested, z),
            Err(_) if requested != bundle0 => (
                bundle0.to_owned(),
                self.cache.file(bundle0, "zip", None).await?,
            ),
            Err(e) => return Err(e.into()),
        };
        // Extract the (cached) bundle zip into a cached folder, reused across every map picked
        // from the same bundle.
        let derived = self
            .cache
            .derived(&format!("acg-bundle-{}", stable_id(&bundle)));
        if !derived.exists() {
            let bytes = std::fs::read(&zip).map_err(|e| ConnectorError::Download(e.to_string()))?;
            let dir = derived.begin()?;
            extract_zip(&bytes, &dir)?;
            derived.commit()?;
        }
        let token = part.ref_.to_lowercase();
        std::fs::read_dir(derived.path())
            .map_err(|e| ConnectorError::Download(e.to_string()))?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.to_lowercase().contains(&token))
                    .unwrap_or(false)
            })
            .ok_or_else(|| ConnectorError::Download(format!("no '{}' map in archive", part.ref_)))
    }
}

/// Picks a sensible default download: prefer 2K, then 1K, then 4K, else the first.
fn pick_download<'a>(downloads: &[&'a Value]) -> Option<&'a Value> {
    let attr = |d: &Value| {
        d.get("attribute")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_uppercase()
    };
    downloads
        .iter()
        .find(|d| attr(d).contains("2K"))
        .or_else(|| downloads.iter().find(|d| attr(d).contains("1K")))
        .or_else(|| downloads.iter().find(|d| attr(d).contains("4K")))
        .or_else(|| downloads.first())
        .copied()
}

/// Swaps the `_<n>K` resolution token in an ambientCG download URL for `res` (e.g. `4K`),
/// so a chosen resolution maps to its deterministic per-resolution zip.
fn url_with_resolution(url: &str, res: &str) -> String {
    let bytes = url.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'_' && bytes[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'K' || bytes[j] == b'k') {
                return format!("{}_{}{}", &url[..i], res, &url[j + 1..]);
            }
        }
        i += 1;
    }
    url.to_owned()
}

fn preview_url(item: &Value, asset_id: &str) -> String {
    item.get("previewImage")
        .and_then(Value::as_object)
        .and_then(|m| m.values().find_map(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "https://acg-media.struffelproduction.com/file/ambientCG-Web/media/thumbnail/256-PNG/{asset_id}.png"
            )
        })
}

/// A short, filesystem-safe id derived from a url (for the cache dir / file name).
fn stable_id(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}", h.finish())
}
