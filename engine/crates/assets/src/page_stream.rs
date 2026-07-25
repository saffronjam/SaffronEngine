//! Asynchronous hierarchy-page payload loading.
//!
//! [`PageStreamWorker`] owns one named worker thread that turns page-load requests into
//! locked device payloads: an artifact-backed source re-reads its `.smesh`/`.smodel`
//! slice from disk and decodes the embedded hierarchy envelope (cached per mesh while
//! its pages stream), a cooked source builds straight from the retained hierarchy. The
//! mirror drains completed payloads once per frame, patches the child-handle tables, and
//! hands the bytes to the renderer's page-residency authority.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use saffron_geometry::{PortableVirtualHierarchy, load_mesh_hierarchy_from_bytes};
use saffron_rendering::{GpuHandle, PagePayload, build_page_payload};

use crate::model::ByteSource;

/// Where a mesh's page payloads come from.
#[derive(Clone)]
pub enum PagePayloadSource {
    /// Re-read the artifact slice and decode its embedded hierarchy envelope.
    Artifact(ByteSource),
    /// Build from the retained cooked hierarchy (a generated mesh has no artifact).
    Cooked(Arc<PortableVirtualHierarchy>),
}

/// One page-load request the mirror hands the worker.
pub struct PageLoadRequest {
    /// The mirror's mesh key.
    pub mesh: u64,
    /// Cook page id within the mesh's hierarchy.
    pub page_id: u32,
    /// The resident page-table handle the payload publishes under.
    pub handle: GpuHandle,
    /// The payload source.
    pub source: PagePayloadSource,
}

/// One completed load.
pub struct PageLoadResult {
    /// The mirror's mesh key.
    pub mesh: u64,
    /// Cook page id within the mesh's hierarchy.
    pub page_id: u32,
    /// The resident page-table handle.
    pub handle: GpuHandle,
    /// The built payload, or the failure to report.
    pub payload: Result<PagePayload, String>,
}

#[derive(Default)]
struct WorkerState {
    requests: VecDeque<PageLoadRequest>,
    results: Vec<PageLoadResult>,
    shutdown: bool,
}

/// Retained decoded hierarchies while a mesh's pages stream.
const HIERARCHY_CACHE_CAPACITY: usize = 8;

/// The page-payload load worker: one named thread, request and result queues drained
/// per frame.
pub struct PageStreamWorker {
    shared: Arc<(Mutex<WorkerState>, Condvar)>,
    thread: Option<JoinHandle<()>>,
    in_flight: usize,
}

impl Default for PageStreamWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl PageStreamWorker {
    /// Spawns the worker thread.
    #[must_use]
    pub fn new() -> Self {
        let shared = Arc::new((Mutex::new(WorkerState::default()), Condvar::new()));
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("page-stream".to_owned())
            .spawn(move || worker_loop(&thread_shared))
            .expect("spawn page-stream worker");
        Self {
            shared,
            thread: Some(thread),
            in_flight: 0,
        }
    }

    /// Requests queued or loading but not yet drained.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// Enqueues `requests` for the worker.
    pub fn enqueue(&mut self, requests: Vec<PageLoadRequest>) {
        if requests.is_empty() {
            return;
        }
        self.in_flight += requests.len();
        let (state, work) = &*self.shared;
        let mut state = state.lock().expect("page-stream state");
        state.requests.extend(requests);
        drop(state);
        work.notify_one();
    }

    /// Drains every completed load.
    pub fn drain(&mut self) -> Vec<PageLoadResult> {
        let (state, _) = &*self.shared;
        let mut state = state.lock().expect("page-stream state");
        let results = std::mem::take(&mut state.results);
        drop(state);
        self.in_flight = self.in_flight.saturating_sub(results.len());
        results
    }
}

impl Drop for PageStreamWorker {
    fn drop(&mut self) {
        let (state, work) = &*self.shared;
        if let Ok(mut state) = state.lock() {
            state.shutdown = true;
        }
        work.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn worker_loop(shared: &(Mutex<WorkerState>, Condvar)) {
    let (state, work) = shared;
    let mut hierarchies: HashMap<u64, Arc<PortableVirtualHierarchy>> = HashMap::new();
    let mut cache_order: VecDeque<u64> = VecDeque::new();
    loop {
        let request = {
            let mut state = state.lock().expect("page-stream state");
            loop {
                if state.shutdown {
                    return;
                }
                if let Some(request) = state.requests.pop_front() {
                    break request;
                }
                state = work.wait(state).expect("page-stream wait");
            }
        };
        let payload =
            resolve_hierarchy(&request, &mut hierarchies, &mut cache_order).and_then(|hierarchy| {
                build_page_payload(&hierarchy, request.page_id)
                    .map_err(|err| format!("page {} payload: {err}", request.page_id))
            });
        let result = PageLoadResult {
            mesh: request.mesh,
            page_id: request.page_id,
            handle: request.handle,
            payload,
        };
        let mut state = state.lock().expect("page-stream state");
        state.results.push(result);
    }
}

fn resolve_hierarchy(
    request: &PageLoadRequest,
    hierarchies: &mut HashMap<u64, Arc<PortableVirtualHierarchy>>,
    cache_order: &mut VecDeque<u64>,
) -> Result<Arc<PortableVirtualHierarchy>, String> {
    match &request.source {
        PagePayloadSource::Cooked(hierarchy) => Ok(Arc::clone(hierarchy)),
        PagePayloadSource::Artifact(source) => {
            if let Some(hierarchy) = hierarchies.get(&request.mesh) {
                return Ok(Arc::clone(hierarchy));
            }
            let bytes = source
                .read()
                .map_err(|err| format!("page source '{}': {err}", source.path))?;
            let hierarchy = load_mesh_hierarchy_from_bytes(&bytes)
                .map_err(|err| format!("page source '{}': {err}", source.path))?;
            let hierarchy = Arc::new(hierarchy);
            if hierarchies.len() >= HIERARCHY_CACHE_CAPACITY
                && let Some(evicted) = cache_order.pop_front()
            {
                hierarchies.remove(&evicted);
            }
            cache_order.push_back(request.mesh);
            hierarchies.insert(request.mesh, Arc::clone(&hierarchy));
            Ok(hierarchy)
        }
    }
}
