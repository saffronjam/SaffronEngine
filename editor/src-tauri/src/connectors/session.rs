//! One live search session: pages a single store's connector, holding its cursor, exhaustion,
//! and a small look-ahead buffer across `next_batch` calls. There is no synthesized global
//! relevance order — results come back in the store's own order, and the UI's scroll position
//! drives how many batches are pulled.

use std::collections::VecDeque;
use std::sync::Arc;

use super::{SearchQuery, StoreConnector, StoreCursor, StoreResult};

/// The connector state carried across `next_batch` calls.
struct Source {
    connector: Arc<dyn StoreConnector>,
    cursor: Option<StoreCursor>,
    exhausted: bool,
    buffer: VecDeque<StoreResult>,
}

impl Source {
    fn drained(&self) -> bool {
        self.exhausted && self.buffer.is_empty()
    }
}

/// One live search session, keyed in the bridge by an id.
pub struct SearchSession {
    query: SearchQuery,
    source: Source,
}

impl SearchSession {
    pub fn new(query: SearchQuery, connector: Arc<dyn StoreConnector>) -> Self {
        let source = Source {
            connector,
            cursor: None,
            exhausted: false,
            buffer: VecDeque::new(),
        };
        Self { query, source }
    }

    /// Whether the source is exhausted and its buffer drained.
    pub fn all_exhausted(&self) -> bool {
        self.source.drained()
    }

    /// Pulls the next page into the buffer when it is empty and the source is not exhausted.
    /// Returns whether anything was fetched.
    async fn refill(&mut self) -> bool {
        if self.source.exhausted || !self.source.buffer.is_empty() {
            return false;
        }
        match self
            .source
            .connector
            .search(&self.query, self.source.cursor.clone())
            .await
        {
            Ok(page) => {
                self.source.buffer.extend(page.results);
                self.source.cursor = page.next_cursor;
                self.source.exhausted = page.exhausted;
                true
            }
            Err(err) => {
                // A failed fetch must not leave the Store spinning; mark the source done and
                // surface the reason in the log.
                tracing::warn!(
                    "store connector '{}' search failed: {err}",
                    self.source.connector.id()
                );
                self.source.exhausted = true;
                false
            }
        }
    }

    /// The next batch of up to `count` results, refilling from the store as needed.
    pub async fn next_batch(&mut self, count: usize) -> Vec<StoreResult> {
        let mut out = Vec::with_capacity(count);
        while out.len() < count {
            if self.source.buffer.is_empty() && !self.refill().await {
                break;
            }
            match self.source.buffer.pop_front() {
                Some(item) => out.push(item),
                None => break,
            }
        }
        out
    }
}
