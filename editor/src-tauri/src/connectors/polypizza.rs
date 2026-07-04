//! The Poly Pizza connector — `AuthKind::ApiKey`. Low-poly models (all of Quaternius and
//! more) delivered as GLB. The free API key is read from the OS keyring at request time;
//! if it is absent the connector returns a typed "not configured" error rather than
//! silently empty results.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::{
    AuthKind, ConnectorError, Credentials, ResourceCache, SearchPage, SearchQuery, StoreConnector,
    StoreCursor, StoreImportDescriptor, StoreKind, StoreLicense, StoreRef, StoreResult,
};

const API_BASE: &str = "https://api.poly.pizza/v1.1";
const PAGE: usize = 24;

pub struct PolyPizza {
    cache: Arc<ResourceCache>,
}

impl PolyPizza {
    pub fn new(cache: Arc<ResourceCache>) -> Self {
        Self { cache }
    }

    fn key(&self) -> Result<String, ConnectorError> {
        Credentials::global()
            .get_secret(self.id())
            .filter(|k| !k.is_empty())
            .ok_or_else(|| ConnectorError::NotConfigured(self.id().to_owned()))
    }

    fn to_result(&self, item: &Value) -> Option<StoreResult> {
        let id = item.get("ID").and_then(Value::as_str)?;
        // Filter-to-importable at the mapping step: only items that resolve to a GLB.
        let download = item.get("Download").and_then(Value::as_str)?;
        if !download.to_lowercase().ends_with(".glb") {
            return None;
        }
        let name = item
            .get("Title")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_owned();
        let author = item
            .get("Creator")
            .and_then(|c| c.get("Username").or_else(|| c.get("DisplayName")))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let lic = item
            .get("Attribution")
            .or_else(|| item.get("Licence"))
            .or_else(|| item.get("License"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        // Default to CC-BY (requires attribution) unless the item is explicitly CC0 — the
        // safe direction, since over-crediting never violates a license.
        let license = if lic.contains("cc0") || lic.contains("public domain") {
            StoreLicense {
                id: "cc0".to_owned(),
                requires_attribution: false,
                url: "https://creativecommons.org/publicdomain/zero/1.0/".to_owned(),
            }
        } else {
            StoreLicense {
                id: "cc-by".to_owned(),
                requires_attribution: true,
                url: "https://creativecommons.org/licenses/by/4.0/".to_owned(),
            }
        };
        Some(StoreResult {
            id: id.to_owned(),
            store: StoreRef {
                id: self.id().to_owned(),
                display_name: self.display_name().to_owned(),
            },
            kind: StoreKind::Model,
            name,
            author,
            thumbnail_url: item
                .get("Thumbnail")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            source_url: format!("https://poly.pizza/m/{id}"),
            license,
            import_descriptor: StoreImportDescriptor {
                format: "glb".to_owned(),
                ref_: download.to_owned(),
                resolution: None,
            },
            has_parts: false,
            supports_resolution: false,
            published_at: None,
            updated_at: None,
            tri_count: item.get("Tris").and_then(Value::as_u64),
            file_size: None,
            resolution: None,
            tags: None,
        })
    }
}

#[async_trait]
impl StoreConnector for PolyPizza {
    fn id(&self) -> &'static str {
        "poly-pizza"
    }

    fn display_name(&self) -> &'static str {
        "Poly Pizza"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn description(&self) -> &'static str {
        "Thousands of low-poly models (incl. all of Quaternius). Needs a free API key."
    }

    fn website(&self) -> &'static str {
        "https://poly.pizza"
    }

    async fn search(
        &self,
        query: &SearchQuery,
        cursor: Option<StoreCursor>,
    ) -> Result<SearchPage, ConnectorError> {
        if matches!(query.kind, Some(k) if k != StoreKind::Model) {
            return Ok(SearchPage {
                results: Vec::new(),
                next_cursor: None,
                exhausted: true,
            });
        }
        // Poly Pizza's search needs a term; with none it contributes nothing this round.
        let term = query.text.trim();
        if term.is_empty() {
            return Ok(SearchPage {
                results: Vec::new(),
                next_cursor: None,
                exhausted: true,
            });
        }
        let key = self.key()?;
        let page: usize = cursor.as_ref().and_then(|c| c.0.parse().ok()).unwrap_or(1);
        let url = format!("{API_BASE}/search/{}", encode_segment(term));
        let resp = self
            .cache
            .client()
            .get(&url)
            .query(&[("page", page.to_string()), ("limit", PAGE.to_string())])
            .header("x-auth-token", key)
            .send()
            .await
            .map_err(|e| ConnectorError::Http(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ConnectorError::NotConfigured(self.id().to_owned()));
        }
        if !resp.status().is_success() {
            return Err(ConnectorError::Http(format!("{} for {url}", resp.status())));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ConnectorError::Decode(e.to_string()))?;
        let items = body
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let got = items.len();
        let results: Vec<StoreResult> = items.iter().filter_map(|it| self.to_result(it)).collect();
        let exhausted = got < PAGE;
        Ok(SearchPage {
            results,
            next_cursor: (!exhausted).then(|| StoreCursor((page + 1).to_string())),
            exhausted,
        })
    }

    async fn download(
        &self,
        descriptor: &StoreImportDescriptor,
        progress: &super::ProgressFn,
    ) -> Result<PathBuf, ConnectorError> {
        // A single GLB, keyed by its (stable) download url — cached, so a re-import is instant.
        self.cache
            .file(&descriptor.ref_, "glb", Some(progress))
            .await
            .map_err(Into::into)
    }
}

/// Percent-encodes a URL path segment (RFC 3986 unreserved set passes through).
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
