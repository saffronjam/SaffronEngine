//! The connector registry: the fixed set of connectors the editor knows about. Which of them a
//! project uses is per-project state (`get-stores`/`set-stores`, saved in `project.json`); a
//! search names the one store to run against, resolved here by id.

use std::sync::Arc;

use serde::Serialize;

use super::{
    AuthKind, ResourceCache, StoreConnector, ambientcg::AmbientCg, polyhaven::PolyHaven,
    polypizza::PolyPizza, sketchfab::Sketchfab,
};

/// A connector's identity + state, surfaced to the webview.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorInfo {
    pub id: String,
    pub display_name: String,
    pub auth_kind: AuthKind,
    pub description: String,
    pub website: String,
    pub enabled: bool,
}

pub struct ConnectorRegistry {
    connectors: Vec<Arc<dyn StoreConnector>>,
}

impl ConnectorRegistry {
    pub fn new(cache: Arc<ResourceCache>) -> Self {
        let connectors: Vec<Arc<dyn StoreConnector>> = vec![
            Arc::new(PolyHaven::new(Arc::clone(&cache))),
            Arc::new(AmbientCg::new(Arc::clone(&cache))),
            Arc::new(PolyPizza::new(Arc::clone(&cache))),
            Arc::new(Sketchfab::new(cache)),
        ];
        Self { connectors }
    }

    pub fn by_id(&self, id: &str) -> Option<Arc<dyn StoreConnector>> {
        self.connectors.iter().find(|c| c.id() == id).cloned()
    }

    pub fn infos(&self) -> Vec<ConnectorInfo> {
        self.connectors
            .iter()
            .map(|c| ConnectorInfo {
                id: c.id().to_owned(),
                display_name: c.display_name().to_owned(),
                auth_kind: c.auth_kind(),
                description: c.description().to_owned(),
                website: c.website().to_owned(),
                enabled: true,
            })
            .collect()
    }
}
