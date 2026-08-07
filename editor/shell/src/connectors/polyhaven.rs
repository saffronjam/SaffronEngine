//! The Poly Haven connector — keyless (`AuthKind::None`), CC0 assets, models only.
//!
//! Every request carries the framework's unique `User-Agent`, which Poly Haven requires.
//! The model deliverable is a glTF with external buffers/images, so `download()` fetches
//! the manifest plus every file it `include`s into one folder the host importer resolves.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use super::{
    AssetPart, AuthKind, ConnectorError, GalleryImage, ResourceCache, SearchPage, SearchQuery,
    StoreConnector, StoreCursor, StoreImportDescriptor, StoreKind, StoreLicense, StoreRef,
    StoreResult,
};

const API_BASE: &str = "https://api.polyhaven.com";
const PAGE: usize = 24;

/// Top-level `/files` keys that are whole-model deliverables (the main Import button), not
/// individually-importable maps.
const MODEL_FILE_KEYS: &[&str] = &["blend", "gltf", "fbx", "usd", "usdc", "usda", "mtlx"];

/// Map-type suffixes → (colorspace role, friendly name), longest-first so `diffuse` wins over
/// `diff`. A `/files` map key is either an exact suffix (single-material models: `Diffuse`,
/// `nor_gl`) or `<material>_<suffix>` (multi-material models: `body_diff`, `lens_body_nor_gl`).
const MAP_SUFFIXES: &[(&str, &str, &str)] = &[
    ("nor_gl", "normal", "Normal (GL)"),
    ("nor_dx", "normal", "Normal (DX)"),
    ("diffuse", "color", "Diffuse"),
    ("diff", "color", "Diffuse"),
    ("albedo", "color", "Albedo"),
    ("color", "color", "Color"),
    ("roughness", "roughness", "Roughness"),
    ("rough", "roughness", "Roughness"),
    ("metallic", "metallic", "Metallic"),
    ("metalness", "metallic", "Metallic"),
    ("metal", "metallic", "Metallic"),
    ("arm", "arm", "ARM"),
    ("ao", "ao", "Ambient occlusion"),
    ("displacement", "displacement", "Displacement"),
    ("disp", "displacement", "Displacement"),
    ("specular", "specular", "Specular"),
    ("spec", "specular", "Specular"),
    ("emissive", "emission", "Emission"),
    ("emission", "emission", "Emission"),
];

/// Classifies a `/files` map key into (colorspace role, display label), splitting any
/// material prefix from the map-type suffix. Unknown keys fall back to a linear data map.
fn classify_map(key: &str) -> (String, String) {
    let lower = key.to_lowercase();
    for (suffix, role, friendly) in MAP_SUFFIXES {
        if lower == *suffix {
            return ((*role).to_owned(), (*friendly).to_owned());
        }
        if let Some(prefix) = lower.strip_suffix(&format!("_{suffix}")) {
            return (
                (*role).to_owned(),
                format!("{} · {friendly}", title_case(prefix)),
            );
        }
    }
    ("data".to_owned(), title_case(&lower))
}

/// `lens_body` → `Lens body`: underscores to spaces, first letter upper-cased.
fn title_case(s: &str) -> String {
    let spaced = s.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => spaced,
    }
}

/// Chooses a resolution key from an available `resolution → …` map: the requested one
/// (`2K` → `2k`) if present, else 2k/1k/4k, else the smallest available.
fn pick_resolution_key(map: &Map<String, Value>, requested: Option<&str>) -> Option<String> {
    if let Some(req) = requested {
        let want = req.to_lowercase();
        if map.contains_key(&want) {
            return Some(want);
        }
    }
    for pref in ["2k", "1k", "4k"] {
        if map.contains_key(pref) {
            return Some(pref.to_owned());
        }
    }
    map.keys()
        .min_by_key(|k| {
            k.trim_end_matches(|c: char| !c.is_ascii_digit())
                .parse::<u32>()
                .unwrap_or(u32::MAX)
        })
        .cloned()
}

/// Picks a small (1k) jpg/png variant from a map's `resolution → format → {url}` tree;
/// returns `None` for non-image entries (model files), which excludes them from the maps.
fn pick_map_image(map: &Map<String, Value>) -> Option<(String, String, String, Option<u64>)> {
    let res = if map.contains_key("1k") {
        "1k".to_owned()
    } else {
        map.keys()
            .min_by_key(|k| {
                k.trim_end_matches(|c: char| !c.is_ascii_digit())
                    .parse::<u32>()
                    .unwrap_or(u32::MAX)
            })?
            .clone()
    };
    let formats = map.get(&res)?.as_object()?;
    let (fmt, entry) = ["jpg", "png"]
        .iter()
        .find_map(|f| formats.get(*f).map(|e| ((*f).to_owned(), e)))?;
    let url = entry.get("url").and_then(Value::as_str)?.to_owned();
    Some((res, fmt, url, entry.get("size").and_then(Value::as_u64)))
}

/// A cached row from the Poly Haven model listing.
struct ModelRow {
    slug: String,
    name: String,
    author: String,
    haystack: String,
}

pub struct PolyHaven {
    cache: Arc<ResourceCache>,
    /// The full model listing, fetched once per process and filtered client-side.
    listing: Mutex<Option<Arc<Vec<ModelRow>>>>,
}

impl PolyHaven {
    pub fn new(cache: Arc<ResourceCache>) -> Self {
        Self {
            cache,
            listing: Mutex::new(None),
        }
    }

    async fn get_json(&self, url: &str) -> Result<Value, ConnectorError> {
        let resp = self
            .cache
            .client()
            .get(url)
            .send()
            .await
            .map_err(|e| ConnectorError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ConnectorError::Http(format!("{} for {url}", resp.status())));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| ConnectorError::Decode(e.to_string()))
    }

    async fn ensure_listing(&self) -> Result<Arc<Vec<ModelRow>>, ConnectorError> {
        let mut guard = self.listing.lock().await;
        if let Some(rows) = guard.as_ref() {
            return Ok(Arc::clone(rows));
        }
        let listing = self
            .get_json(&format!("{API_BASE}/assets?type=models"))
            .await?;
        let map = listing
            .as_object()
            .ok_or_else(|| ConnectorError::Decode("assets listing is not an object".to_owned()))?;
        let mut rows: Vec<ModelRow> = map
            .iter()
            .map(|(slug, meta)| {
                let name = meta
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(slug)
                    .to_owned();
                let author = meta
                    .get("authors")
                    .and_then(Value::as_object)
                    .and_then(|a| a.keys().next())
                    .cloned()
                    .unwrap_or_default();
                let mut haystack = format!("{slug} {name}").to_lowercase();
                for key in ["tags", "categories"] {
                    if let Some(items) = meta.get(key).and_then(Value::as_array) {
                        for item in items {
                            if let Some(s) = item.as_str() {
                                haystack.push(' ');
                                haystack.push_str(&s.to_lowercase());
                            }
                        }
                    }
                }
                ModelRow {
                    slug: slug.clone(),
                    name,
                    author,
                    haystack,
                }
            })
            .collect();
        rows.sort_by(|a, b| a.slug.cmp(&b.slug));
        let rows = Arc::new(rows);
        *guard = Some(Arc::clone(&rows));
        Ok(rows)
    }

    fn to_result(&self, row: &ModelRow) -> StoreResult {
        StoreResult {
            id: row.slug.clone(),
            store: StoreRef {
                id: self.id().to_owned(),
                display_name: self.display_name().to_owned(),
            },
            kind: StoreKind::Model,
            name: row.name.clone(),
            author: row.author.clone(),
            thumbnail_url: format!(
                "https://cdn.polyhaven.com/asset_img/thumbs/{}.png?width=256&height=256",
                row.slug
            ),
            source_url: format!("https://polyhaven.com/a/{}", row.slug),
            license: StoreLicense {
                id: "cc0".to_owned(),
                requires_attribution: false,
                url: "https://creativecommons.org/publicdomain/zero/1.0/".to_owned(),
            },
            import_descriptor: StoreImportDescriptor {
                format: "gltf".to_owned(),
                ref_: row.slug.clone(),
                resolution: None,
            },
            // The /files endpoint lists every map individually, so a model exposes parts.
            has_parts: true,
            supports_resolution: true,
            published_at: None,
            updated_at: None,
            tri_count: None,
            file_size: None,
            resolution: None,
            tags: None,
        }
    }

    /// A cheap HEAD probe — Poly Haven renders aren't listed by the API, so a predictable
    /// render URL is only included in the gallery once confirmed to exist.
    async fn url_exists(&self, url: &str) -> bool {
        self.cache
            .client()
            .head(url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

#[async_trait]
impl StoreConnector for PolyHaven {
    fn id(&self) -> &'static str {
        "polyhaven"
    }

    fn display_name(&self) -> &'static str {
        "Poly Haven"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::None
    }

    fn description(&self) -> &'static str {
        "Free CC0 HDRIs, textures, and 3D models — no account needed."
    }

    fn website(&self) -> &'static str {
        "https://polyhaven.com"
    }

    async fn search(
        &self,
        query: &SearchQuery,
        cursor: Option<StoreCursor>,
    ) -> Result<SearchPage, ConnectorError> {
        // Poly Haven contributes models only, so a `type:` filter for anything else matches nothing.
        if matches!(query.kind, Some(k) if k != StoreKind::Model) {
            return Ok(SearchPage {
                results: Vec::new(),
                next_cursor: None,
                exhausted: true,
            });
        }
        let rows = self.ensure_listing().await?;
        let text = query.text.trim().to_lowercase();
        let matched: Vec<&ModelRow> = rows
            .iter()
            .filter(|r| text.is_empty() || r.haystack.contains(&text))
            .collect();

        let offset = cursor
            .as_ref()
            .and_then(|c| c.0.parse::<usize>().ok())
            .unwrap_or(0);
        let end = (offset + PAGE).min(matched.len());
        let results: Vec<StoreResult> = matched
            .get(offset..end)
            .unwrap_or(&[])
            .iter()
            .map(|row| self.to_result(row))
            .collect();
        let exhausted = end >= matched.len();
        Ok(SearchPage {
            results,
            next_cursor: (!exhausted).then(|| StoreCursor(end.to_string())),
            exhausted,
        })
    }

    async fn download(
        &self,
        descriptor: &StoreImportDescriptor,
        progress: &super::ProgressFn,
    ) -> Result<PathBuf, ConnectorError> {
        let slug = &descriptor.ref_;
        let files = self.get_json(&format!("{API_BASE}/files/{slug}")).await?;
        let gltf = files
            .get("gltf")
            .and_then(Value::as_object)
            .ok_or_else(|| ConnectorError::UnsupportedFormat("no glTF variant".to_owned()))?;
        // Resolutions are keyed "1k"/"2k"/…/"16k"; honor the chosen one, else default 2k.
        let res_key = pick_resolution_key(gltf, descriptor.resolution.as_deref())
            .ok_or_else(|| ConnectorError::UnsupportedFormat("no glTF resolution".to_owned()))?;
        // `gltf[res].gltf` is the file object itself: { url, md5, size, include }.
        let entry = gltf
            .get(&res_key)
            .and_then(|v| v.get("gltf"))
            .and_then(Value::as_object)
            .ok_or_else(|| ConnectorError::UnsupportedFormat("no glTF entry".to_owned()))?;
        let main_url = entry
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| ConnectorError::Download("glTF url missing".to_owned()))?;
        let filename = main_url
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("model.gltf");

        // The whole glTF file set (manifest + external buffers/images) is a materialized
        // directory in the cache, keyed by slug + resolution. A repeat import of the same asset +
        // resolution reuses it — only the small `/files` manifest lookup above still runs.
        let derived = self.cache.derived(&format!("polyhaven-{slug}-{res_key}"));
        if !derived.exists() {
            let dir = derived.begin()?;
            // Fetch the manifest plus every referenced file; sizes come from the manifest, so
            // progress spans the whole set (each file's fraction folded into the aggregate).
            let mut targets: Vec<(String, PathBuf, u64)> = vec![(
                main_url.to_owned(),
                dir.join(filename),
                entry.get("size").and_then(Value::as_u64).unwrap_or(0),
            )];
            if let Some(include) = entry.get("include").and_then(Value::as_object) {
                for (rel, info) in include {
                    if let Some(url) = info.get("url").and_then(Value::as_str) {
                        targets.push((
                            url.to_owned(),
                            dir.join(rel),
                            info.get("size").and_then(Value::as_u64).unwrap_or(0),
                        ));
                    }
                }
            }
            let total = targets.iter().map(|(_, _, s)| *s).sum::<u64>().max(1) as f64;
            let mut done: u64 = 0;
            for (url, dest, size) in &targets {
                let base = done as f64;
                let sz = *size as f64;
                let file_prog = move |frac: f64| progress((base + frac * sz) / total);
                let (bytes, _) = self.cache.fetch(url, Some(&file_prog)).await?;
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| ConnectorError::Download(e.to_string()))?;
                }
                std::fs::write(dest, &bytes)
                    .map_err(|e| ConnectorError::Download(e.to_string()))?;
                done += size;
            }
            derived.commit()?;
        }
        progress(1.0);
        Ok(derived.path().join(filename))
    }

    async fn parts(&self, result: &StoreResult) -> Result<Vec<AssetPart>, ConnectorError> {
        let slug = &result.id;
        let files = self.get_json(&format!("{API_BASE}/files/{slug}")).await?;
        let obj = files
            .as_object()
            .ok_or_else(|| ConnectorError::Decode("files is not an object".to_owned()))?;
        // Every non-model top-level entry is a map; a multi-material model prefixes each by
        // material (`body_diff`, `strap_arm`), so we parse generically rather than by a fixed set.
        let mut parts = Vec::new();
        for (key, value) in obj {
            if MODEL_FILE_KEYS.contains(&key.as_str()) {
                continue;
            }
            let Some(map) = value.as_object() else {
                continue;
            };
            let Some((res, fmt, url, size)) = pick_map_image(map) else {
                continue;
            };
            let (role, label) = classify_map(key);
            parts.push(AssetPart {
                id: format!("{slug}:{key}"),
                label,
                import_kind: StoreKind::Texture,
                role: Some(role),
                resolution: Some(res),
                format: Some(fmt),
                size,
                ref_: url,
                bundle: None,
            });
        }
        // Deterministic order (the `/files` object's key order is not guaranteed).
        parts.sort_by(|a, b| a.label.cmp(&b.label));
        Ok(parts)
    }

    async fn download_part(
        &self,
        part: &AssetPart,
        resolution: Option<&str>,
    ) -> Result<PathBuf, ConnectorError> {
        let ext = part.format.as_deref().unwrap_or("jpg");
        // The part URL is built at the part's own resolution (e.g. 1k); rewrite it to the
        // chosen one by swapping that token, falling back to the original if it 404s.
        let url = match (resolution, part.resolution.as_deref()) {
            (Some(res), Some(orig)) if !res.eq_ignore_ascii_case(orig) => {
                part.ref_.replace(orig, &res.to_lowercase())
            }
            _ => part.ref_.clone(),
        };
        match self.cache.file(&url, ext, None).await {
            Ok(p) => Ok(p),
            Err(_) if url != part.ref_ => self
                .cache
                .file(&part.ref_, ext, None)
                .await
                .map_err(Into::into),
            Err(e) => Err(e.into()),
        }
    }

    async fn gallery(&self, result: &StoreResult) -> Result<Vec<GalleryImage>, ConnectorError> {
        // Preview + the rendered angles — no texture maps: those are the *parts* (`parts()`), fetched
        // lazily by the import dropdown, not slideshow images. Keeping them out is what makes the
        // gallery light (no `/files` manifest fetch here) and short (a handful of images, not 10-30).
        // Card shows the 256px thumb (same as before hover → no reload blink); the large detail view
        // upgrades to a 1024px render of the same framing (the thumb endpoint resizes on demand) so
        // the hero isn't soft up close.
        let mut images = vec![GalleryImage {
            url: result.thumbnail_url.clone(),
            label: Some("Preview".to_owned()),
            full_url: Some(format!(
                "https://cdn.polyhaven.com/asset_img/thumbs/{}.png?width=1024&height=1024",
                result.id
            )),
        }];
        // Poly Haven's gallery renders live at predictable `asset_img/renders/<slug>/` paths (the API
        // doesn't list them): textured orthographic angles plus a clay pass, resized via `?height=`.
        // Probe all four concurrently (a model may lack a top view, etc.) so the gallery returns after
        // one round-trip, not four serial ones.
        let base = format!("https://cdn.polyhaven.com/asset_img/renders/{}", result.id);
        const RENDERS: &[(&str, &str)] = &[
            ("orth_front.png", "Front"),
            ("orth_side.png", "Side"),
            ("orth_top.png", "Top"),
            ("clay.png", "Clay"),
        ];
        let (u0, u1, u2, u3) = (
            format!("{base}/{}", RENDERS[0].0),
            format!("{base}/{}", RENDERS[1].0),
            format!("{base}/{}", RENDERS[2].0),
            format!("{base}/{}", RENDERS[3].0),
        );
        let (front, side, top, clay) = tokio::join!(
            self.url_exists(&u0),
            self.url_exists(&u1),
            self.url_exists(&u2),
            self.url_exists(&u3),
        );
        for (exists, (file, label)) in [front, side, top, clay].into_iter().zip(RENDERS) {
            if exists {
                images.push(GalleryImage {
                    url: format!("{base}/{file}?height=512&quality=95"),
                    label: Some((*label).to_owned()),
                    full_url: Some(format!("{base}/{file}?height=1024&quality=95")),
                });
            }
        }
        Ok(images)
    }
}
