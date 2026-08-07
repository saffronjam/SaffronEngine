//! CPU authority for hierarchy page payload residency.
//!
//! [`PageResidency`] tracks every registered resident page-table entry through a
//! per-page state machine (unloaded → requested → loading → ready → resident) and owns
//! the byte accounting of the global page arena. Publication respects the hierarchy:
//! a page's payload publishes only after its parent's payload is resident (roots first),
//! so the GPU never observes a child without its drawable ancestor. Eviction reverses
//! that order — only pages without resident children and without the guaranteed-root
//! flag are candidates, picked by least-recent demand and then by the cheapest reader
//! among pages demanded together — and retires arena ranges through the fence-deferred
//! reuse the arenas already provide. Demand arrives from the CPU prioritizer and from the
//! per-frame GPU missing-page request buffer; priority is an explicit caller-computed
//! weight, never distance alone, and it describes the frame it was recorded in rather
//! than accumulating across a page's whole life.

use std::collections::HashMap;

use crate::global_gpu_data::{GlobalGpuData, GlobalGpuTableKind, GpuArenaRange, GpuHandle};
use crate::gpu_scene_upload::{GpuArenaUploadRequest, GpuScenePendingUploads};
use crate::{Error, Result};

/// The active view's inputs to page-demand prioritization: the eye for projected error,
/// the projection scale (pixels per metre at unit distance), and the view-projection for
/// frustum visibility probability.
#[derive(Clone, Copy, Debug)]
pub struct PageDemandView {
    /// World-space camera position.
    pub eye: saffron_geometry::glam::Vec3,
    /// `cot(fovY/2) * viewportHeight / 2` — projected pixels per metre at 1 m.
    pub proj_scale: f32,
    /// World → clip for conservative frustum containment.
    pub view_proj: saffron_geometry::glam::Mat4,
    /// Minimum corner of the window a GI or reflection ray can reach.
    ///
    /// A march gathers from behind the eye and a reflection shows what the camera cannot, so a
    /// page inside this window is read whether or not it is visible — and a non-resident one is a
    /// hole in the gather rather than a missing pixel.
    pub gi_min: saffron_geometry::glam::Vec3,
    /// Maximum corner of the reachable window.
    pub gi_max: saffron_geometry::glam::Vec3,
    /// Wall interval this frame spans, so eye travel and an instance's `previous`→`current`
    /// world transforms convert to metres per second. Zero disables the predictive lead.
    pub frame_seconds: f32,
}

/// How far ahead page demand is scored.
///
/// A payload takes several frames to reach the arena, so the frontier is ranked at its closest
/// approach over this horizon rather than at the frame's standing distance — otherwise the page a
/// moving camera is about to need only outranks its neighbours once the camera is already on it.
pub const PAGE_DEMAND_PREDICTION_SECONDS: f32 = 0.5;

/// Ceiling for a CPU-predicted page-demand priority.
///
/// The prioritizer scores the refinement frontier from projected error, which has no
/// natural bound; clamping it here is what keeps every predicted score below the bands a
/// GPU miss carries, so a page some view actually tried to read this frame outranks every
/// page some view might read next.
pub const PAGE_DEMAND_PREDICTED_CEILING: u64 = 1_000_000_000_000_000_000;

/// Byte budgets for resident page payloads.
#[derive(Clone, Copy, Debug)]
pub struct PageResidencyBudgets {
    /// Ceiling for resident payload bytes; guaranteed roots publish past it.
    pub max_resident_bytes: u64,
}

impl Default for PageResidencyBudgets {
    fn default() -> Self {
        Self {
            max_resident_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Residency counters for stats surfaces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageResidencyStats {
    /// Registered pages.
    pub registered: u64,
    /// Pages whose payload is resident.
    pub resident: u64,
    /// Resident payload bytes.
    pub resident_bytes: u64,
    /// Byte budget.
    pub budget_bytes: u64,
    /// Pages awaiting a load worker.
    pub requested: u64,
    /// Pages at the load worker.
    pub loading: u64,
    /// Loaded pages awaiting publication.
    pub ready: u64,
    /// Cumulative evictions.
    pub evictions: u64,
    /// Pages that went from requested to resident, which is what a fault costs.
    pub faults: u64,
    /// Microseconds those faults took, summed. Divided by `faults` it is the mean fault latency;
    /// kept as a sum so the counter stays additive and the caller picks the window.
    pub fault_latency_us: u64,
    /// Missing-page requests the GPU raised that no region had room for, summed since
    /// boot. Kept as a running total for the same reason the other counters here are:
    /// overflow is bursty, and the frame a caller happens to sample is not the frame the
    /// buffer filled.
    pub requests_dropped: u64,
    /// Bit per view class whose request region has filled since boot
    /// ([`crate::SceneViewClass::bit`]). The count says how much was lost; this says
    /// whose, which is what decides whether it matters.
    pub request_overflow_classes: u32,
}

/// Applies one demand to `entry`. Demands arriving in the same frame take the most
/// urgent of them; a demand in a later frame REPLACES the priority rather than raising
/// it, so a page reads as what wants it now — a page the camera passed once and a gather
/// still reads is a gather's page, and holding the camera's band would make it outlive
/// every page the image is actually made of.
fn record_demand(entry: &mut PageEntry, frame: u64, priority: u64) {
    if entry.last_demand_frame == frame {
        entry.priority = entry.priority.max(priority);
    } else {
        entry.last_demand_frame = frame;
        entry.priority = priority;
    }
    if matches!(entry.state, PageState::Unloaded) {
        entry.state = PageState::Requested;
        entry.requested_at = Some(std::time::Instant::now());
    }
}

enum PageState {
    Unloaded,
    Requested,
    Loading,
    Ready(Vec<u8>),
    Resident(GpuArenaRange),
}

struct PageEntry {
    generation: u32,
    state: PageState,
    /// When the page was last requested, so publication can price the fault.
    requested_at: Option<std::time::Instant>,
    parent: Option<GpuHandle>,
    guaranteed_root: bool,
    resident_children: u32,
    last_demand_frame: u64,
    priority: u64,
}

/// The page-payload residency state machine and arena byte accounting.
#[derive(Default)]
pub struct PageResidency {
    pages: HashMap<u32, PageEntry>,
    budgets: PageResidencyBudgets,
    resident_bytes: u64,
    evictions: u64,
    faults: u64,
    fault_latency_us: u64,
    requests_dropped: u64,
    request_overflow_classes: u32,
    frame: u64,
}

impl PageResidency {
    /// Creates an empty manager with `budgets`.
    #[must_use]
    pub fn new(budgets: PageResidencyBudgets) -> Self {
        Self {
            budgets,
            ..Self::default()
        }
    }

    /// The active budgets.
    #[must_use]
    pub fn budgets(&self) -> PageResidencyBudgets {
        self.budgets
    }

    /// Replaces the budgets; the next publication pass applies them.
    pub fn set_budgets(&mut self, budgets: PageResidencyBudgets) {
        self.budgets = budgets;
    }

    /// Advances the demand clock; call once per rendered frame.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// Registers `handle` with its parent linkage. A guaranteed root is demanded
    /// immediately at top priority.
    pub fn register_page(
        &mut self,
        handle: GpuHandle,
        parent: Option<GpuHandle>,
        guaranteed_root: bool,
    ) {
        let frame = self.frame;
        self.pages.insert(
            handle.index,
            PageEntry {
                generation: handle.generation,
                state: if guaranteed_root {
                    PageState::Requested
                } else {
                    PageState::Unloaded
                },
                parent,
                guaranteed_root,
                resident_children: 0,
                last_demand_frame: frame,
                priority: if guaranteed_root { u64::MAX } else { 0 },
                requested_at: guaranteed_root.then(std::time::Instant::now),
            },
        );
    }

    /// Unregisters `handle`, retiring its resident bytes. The caller removes pages in
    /// reverse dependency order, so children unregister before their parent.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidUploadData`] when a resident range fails to retire.
    pub fn unregister_page(
        &mut self,
        handle: GpuHandle,
        gpu_data: &mut GlobalGpuData,
    ) -> Result<()> {
        let Some(entry) = self.pages.get(&handle.index) else {
            return Ok(());
        };
        if entry.generation != handle.generation {
            return Ok(());
        }
        let parent = entry.parent;
        let was_resident = matches!(entry.state, PageState::Resident(_));
        if let Some(entry) = self.pages.remove(&handle.index)
            && let PageState::Resident(range) = entry.state
        {
            gpu_data.pages.retire(range)?;
            self.resident_bytes = self.resident_bytes.saturating_sub(u64::from(range.count));
        }
        if was_resident && let Some(parent) = parent {
            self.on_child_unresident(parent);
        }
        Ok(())
    }

    /// Records demand for `handle` with `priority` (higher is more urgent).
    pub fn demand(&mut self, handle: GpuHandle, priority: u64) {
        let frame = self.frame;
        if let Some(entry) = self.pages.get_mut(&handle.index)
            && entry.generation == handle.generation
        {
            record_demand(entry, frame, priority);
        }
    }

    /// Records demand the GPU raised and the request buffer could not hold: `dropped`
    /// requests lost across the classes in `classes`.
    ///
    /// A dropped request is not lost geometry — the page faults again next frame — but it
    /// is latency nobody asked for, and it is invisible from outside unless counted.
    pub fn note_dropped_requests(&mut self, dropped: u32, classes: u32) {
        self.requests_dropped = self.requests_dropped.saturating_add(u64::from(dropped));
        self.request_overflow_classes |= classes;
    }

    /// Records demand for a raw resident page-table slot (a GPU missing-page request).
    pub fn demand_slot(&mut self, slot: u32, priority: u64) {
        let frame = self.frame;
        if let Some(entry) = self.pages.get_mut(&slot) {
            record_demand(entry, frame, priority);
        }
    }

    /// Hands out up to `max` requested pages to the load worker, most urgent first.
    pub fn take_load_requests(&mut self, max: usize) -> Vec<GpuHandle> {
        let mut requested: Vec<(u64, u64, GpuHandle)> = self
            .pages
            .iter()
            .filter(|(_, entry)| matches!(entry.state, PageState::Requested))
            .map(|(index, entry)| {
                (
                    entry.priority,
                    entry.last_demand_frame,
                    GpuHandle {
                        index: *index,
                        generation: entry.generation,
                    },
                )
            })
            .collect();
        requested.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        requested.truncate(max);
        let handles: Vec<GpuHandle> = requested.into_iter().map(|(.., handle)| handle).collect();
        for handle in &handles {
            if let Some(entry) = self.pages.get_mut(&handle.index) {
                entry.state = PageState::Loading;
            }
        }
        handles
    }

    /// Accepts a completed load. A stale handle drops the bytes.
    pub fn complete_load(&mut self, handle: GpuHandle, bytes: Vec<u8>) {
        if let Some(entry) = self.pages.get_mut(&handle.index)
            && entry.generation == handle.generation
            && matches!(entry.state, PageState::Loading)
        {
            entry.state = PageState::Ready(bytes);
        }
    }

    /// Records a failed load; a later demand retries the page.
    pub fn fail_load(&mut self, handle: GpuHandle) {
        if let Some(entry) = self.pages.get_mut(&handle.index)
            && entry.generation == handle.generation
            && matches!(entry.state, PageState::Loading)
        {
            entry.state = PageState::Unloaded;
        }
    }

    /// The refinement frontier: unloaded pages whose parent payload is resident (roots
    /// stay demanded from registration, so they never appear here). The prioritizer
    /// scores these against the view; demand walks the hierarchy one level at a time.
    #[must_use]
    pub fn frontier(&self) -> Vec<GpuHandle> {
        self.pages
            .iter()
            .filter(|(_, entry)| {
                matches!(entry.state, PageState::Unloaded)
                    && entry.parent.is_some_and(|parent| {
                        self.pages.get(&parent.index).is_some_and(|parent_entry| {
                            parent_entry.generation == parent.generation
                                && matches!(parent_entry.state, PageState::Resident(_))
                        })
                    })
            })
            .map(|(index, entry)| GpuHandle {
                index: *index,
                generation: entry.generation,
            })
            .collect()
    }

    /// Publishes every ready page whose parent is resident, parents before children,
    /// evicting least-recently-demanded leaves when the budget requires it. Enqueues the
    /// payload bytes and the updated page record into `pending`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidUploadData`] when arena accounting or a page-table update
    /// fails; the manager and tables stay consistent.
    pub fn publish_ready(
        &mut self,
        gpu_data: &mut GlobalGpuData,
        pending: &mut GpuScenePendingUploads,
    ) -> Result<()> {
        loop {
            let candidate = self.pages.iter().find_map(|(index, entry)| {
                if !matches!(entry.state, PageState::Ready(_)) {
                    return None;
                }
                let parent_resident = match entry.parent {
                    None => true,
                    Some(parent) => self.pages.get(&parent.index).is_some_and(|parent_entry| {
                        parent_entry.generation == parent.generation
                            && matches!(parent_entry.state, PageState::Resident(_))
                    }),
                };
                parent_resident.then_some(GpuHandle {
                    index: *index,
                    generation: entry.generation,
                })
            });
            let Some(handle) = candidate else {
                return Ok(());
            };
            self.publish_one(handle, gpu_data, pending)?;
        }
    }

    /// The current counters.
    #[must_use]
    pub fn stats(&self) -> PageResidencyStats {
        let mut stats = PageResidencyStats {
            registered: self.pages.len() as u64,
            resident_bytes: self.resident_bytes,
            budget_bytes: self.budgets.max_resident_bytes,
            evictions: self.evictions,
            faults: self.faults,
            fault_latency_us: self.fault_latency_us,
            requests_dropped: self.requests_dropped,
            request_overflow_classes: self.request_overflow_classes,
            ..PageResidencyStats::default()
        };
        for entry in self.pages.values() {
            match entry.state {
                PageState::Unloaded => {}
                PageState::Requested => stats.requested += 1,
                PageState::Loading => stats.loading += 1,
                PageState::Ready(_) => stats.ready += 1,
                PageState::Resident(_) => stats.resident += 1,
            }
        }
        stats
    }

    fn publish_one(
        &mut self,
        handle: GpuHandle,
        gpu_data: &mut GlobalGpuData,
        pending: &mut GpuScenePendingUploads,
    ) -> Result<()> {
        let entry = self.pages.get_mut(&handle.index).ok_or_else(|| {
            Error::InvalidUploadData("page publication requires a registered page".to_owned())
        })?;
        let PageState::Ready(bytes) = std::mem::replace(&mut entry.state, PageState::Unloaded)
        else {
            return Err(Error::InvalidUploadData(
                "page publication requires a ready payload".to_owned(),
            ));
        };
        // A fault is the whole round trip: the frame the page was demanded to the moment its
        // payload is resident. What a stutter costs is when the geometry can be drawn, not when
        // the bytes arrived, so the timing closes at publication rather than at the worker's return.
        if let Some(requested) = entry.requested_at.take() {
            self.faults += 1;
            self.fault_latency_us += requested.elapsed().as_micros() as u64;
        }
        let length = u64::try_from(bytes.len())
            .map_err(|_| Error::InvalidUploadData("page payload exceeds u64".to_owned()))?;
        let guaranteed_root = entry.guaranteed_root;
        let parent = entry.parent;

        if !guaranteed_root
            && self.resident_bytes.saturating_add(length) > self.budgets.max_resident_bytes
        {
            let needed = self
                .resident_bytes
                .saturating_add(length)
                .saturating_sub(self.budgets.max_resident_bytes);
            self.evict_lru(needed, gpu_data, pending)?;
            if self.resident_bytes.saturating_add(length) > self.budgets.max_resident_bytes {
                // Budget still exceeded (everything else is pinned): drop back to
                // unloaded so a later demand retries.
                return Ok(());
            }
        }

        let count = u32::try_from(bytes.len())
            .map_err(|_| Error::InvalidUploadData("page payload exceeds u32 bytes".to_owned()))?;
        let (range, _) = gpu_data.pages.allocate(count, 16)?;
        let byte_offset = gpu_data.pages.byte_offset(range)?;
        let mut record = *gpu_data.page_table.get(handle).ok_or_else(|| {
            Error::InvalidUploadData("page publication requires a live page record".to_owned())
        })?;
        record.byte_offset = byte_offset;
        record.byte_length = count;
        record.resident_generation = record.resident_generation.wrapping_add(1);
        gpu_data.page_table.update(handle, record)?;
        pending.upload_arena(GpuArenaUploadRequest::PageBytes { range, data: bytes });
        pending.stage_record(GlobalGpuTableKind::Page, handle);

        let entry = self
            .pages
            .get_mut(&handle.index)
            .expect("entry present for the page being published");
        entry.state = PageState::Resident(range);
        self.resident_bytes = self.resident_bytes.saturating_add(length);
        if let Some(parent) = parent
            && let Some(parent_entry) = self.pages.get_mut(&parent.index)
        {
            parent_entry.resident_children += 1;
        }
        Ok(())
    }

    fn evict_lru(
        &mut self,
        needed: u64,
        gpu_data: &mut GlobalGpuData,
        pending: &mut GpuScenePendingUploads,
    ) -> Result<()> {
        let mut freed = 0_u64;
        while freed < needed {
            let candidate = self
                .pages
                .iter()
                .filter(|(_, entry)| {
                    matches!(entry.state, PageState::Resident(_))
                        && !entry.guaranteed_root
                        && entry.resident_children == 0
                })
                // Least-recently demanded first, then the cheapest reader among pages
                // demanded together: a page nobody has asked for in a hundred frames is
                // dead weight whoever last wanted it, and among pages every view still
                // wants, the gather's go before the camera's.
                .min_by_key(|(_, entry)| (entry.last_demand_frame, entry.priority))
                .map(|(index, entry)| GpuHandle {
                    index: *index,
                    generation: entry.generation,
                });
            let Some(handle) = candidate else {
                return Ok(());
            };
            freed = freed.saturating_add(self.evict_one(handle, gpu_data, pending)?);
        }
        Ok(())
    }

    fn evict_one(
        &mut self,
        handle: GpuHandle,
        gpu_data: &mut GlobalGpuData,
        pending: &mut GpuScenePendingUploads,
    ) -> Result<u64> {
        let entry = self.pages.get_mut(&handle.index).ok_or_else(|| {
            Error::InvalidUploadData("page eviction requires a registered page".to_owned())
        })?;
        let PageState::Resident(range) = std::mem::replace(&mut entry.state, PageState::Unloaded)
        else {
            return Err(Error::InvalidUploadData(
                "page eviction requires a resident payload".to_owned(),
            ));
        };
        let parent = entry.parent;
        gpu_data.pages.retire(range)?;
        let mut record = *gpu_data.page_table.get(handle).ok_or_else(|| {
            Error::InvalidUploadData("page eviction requires a live page record".to_owned())
        })?;
        record.byte_offset = 0;
        record.byte_length = 0;
        record.resident_generation = record.resident_generation.wrapping_add(1);
        gpu_data.page_table.update(handle, record)?;
        pending.stage_record(GlobalGpuTableKind::Page, handle);
        let freed = u64::from(range.count);
        self.resident_bytes = self.resident_bytes.saturating_sub(freed);
        self.evictions += 1;
        if let Some(parent) = parent {
            self.on_child_unresident(parent);
        }
        Ok(freed)
    }

    fn on_child_unresident(&mut self, parent: GpuHandle) {
        if let Some(parent_entry) = self.pages.get_mut(&parent.index)
            && parent_entry.generation == parent.generation
        {
            parent_entry.resident_children = parent_entry.resident_children.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_gpu_scene::GpuSceneUploadLimits;
    use crate::{Device, GpuPageRecord, PersistentGpuScene, SurfaceSource};

    fn device_or_skip() -> Option<Device> {
        match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => Some(device),
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                None
            }
        }
    }

    /// A fault is the round trip from demand to residency, and the counters stay additive so a
    /// caller picks its own window. No device needed: the state machine owns the timing.
    #[test]
    fn a_demand_that_never_publishes_is_not_a_fault() {
        let mut residency = PageResidency::new(PageResidencyBudgets::default());
        let handle = GpuHandle {
            index: 4,
            generation: 1,
        };
        residency.register_page(handle, None, false);
        assert_eq!(residency.stats().faults, 0);
        residency.demand(handle, 7);
        // Demanded, not resident: a fault is only priced once the payload can be drawn.
        let stats = residency.stats();
        assert_eq!(stats.requested, 1);
        assert_eq!(stats.faults, 0);
        assert_eq!(stats.fault_latency_us, 0);

        // A guaranteed root is demanded at registration, so its clock starts there.
        let root = GpuHandle {
            index: 5,
            generation: 1,
        };
        residency.register_page(root, None, true);
        assert_eq!(residency.stats().requested, 2);
        assert_eq!(residency.stats().faults, 0);
    }

    /// The two demand sources share one numeric space, and a miss must outrank a prediction.
    /// Nothing else in the tree compares them, so retuning either side could drift silently.
    #[test]
    fn every_missed_page_outranks_every_predicted_one() {
        for class in crate::SceneViewClass::ALL {
            assert!(
                class.page_demand_priority() > PAGE_DEMAND_PREDICTED_CEILING,
                "{class:?} misses must outrank the prioritizer's ceiling"
            );
        }
        assert!(
            crate::SceneViewClass::Camera.page_demand_priority()
                > crate::SceneViewClass::ShadowPage.page_demand_priority(),
            "the image outranks its shadows"
        );
        assert!(
            crate::SceneViewClass::ShadowPage.page_demand_priority()
                > crate::SceneViewClass::Gi.page_demand_priority(),
            "a shadow outranks a gather"
        );
    }

    /// A page's priority names the class that wants it NOW. Holding a running maximum
    /// instead would let one camera glance mark a page as the camera's for the rest of the
    /// run, and eviction would then protect it over every page the image is actually made
    /// of. No device needed: the demand clock owns this.
    #[test]
    fn a_later_frame_replaces_the_priority_rather_than_raising_it() {
        let mut residency = PageResidency::new(PageResidencyBudgets::default());
        let handle = GpuHandle {
            index: 3,
            generation: 1,
        };
        residency.register_page(handle, None, false);

        residency.begin_frame();
        residency.demand(handle, crate::SceneViewClass::Gi.page_demand_priority());
        residency.demand(handle, crate::SceneViewClass::Camera.page_demand_priority());
        assert_eq!(
            residency.pages[&3].priority,
            crate::SceneViewClass::Camera.page_demand_priority(),
            "demands in one frame take the most urgent of them"
        );

        residency.begin_frame();
        residency.demand(handle, crate::SceneViewClass::Gi.page_demand_priority());
        assert_eq!(
            residency.pages[&3].priority,
            crate::SceneViewClass::Gi.page_demand_priority(),
            "the camera moved on and the gather did not"
        );
    }

    struct Fixture {
        gpu_data: GlobalGpuData,
        pending: GpuScenePendingUploads,
        residency: PageResidency,
        device: Device,
    }

    fn fixture(budget: u64) -> Option<Fixture> {
        let device = device_or_skip()?;
        let gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        // Keep the persistent scene import alive for the shared test vocabulary.
        let _ = PersistentGpuScene::new(GpuSceneUploadLimits::default());
        Some(Fixture {
            gpu_data,
            pending: GpuScenePendingUploads::default(),
            residency: PageResidency::new(PageResidencyBudgets {
                max_resident_bytes: budget,
            }),
            device,
        })
    }

    fn insert_page(fixture: &mut Fixture, parent: Option<GpuHandle>, root: bool) -> GpuHandle {
        let handle = fixture
            .gpu_data
            .page_table
            .insert(GpuPageRecord {
                parent: parent.unwrap_or(GpuHandle::INVALID),
                dependencies: GpuArenaRange::default(),
                byte_offset: 0,
                byte_length: 0,
                resident_generation: 0,
                flags: if root {
                    crate::GPU_PAGE_FLAG_GUARANTEED_ROOT
                } else {
                    0
                },
                reserved: 0,
            })
            .expect("page record");
        fixture.residency.register_page(handle, parent, root);
        handle
    }

    #[test]
    fn publication_orders_parents_before_children_and_updates_records() {
        let Some(mut fixture) = fixture(1024 * 1024) else {
            return;
        };
        let root = insert_page(&mut fixture, None, true);
        let child = insert_page(&mut fixture, Some(root), false);

        // The child is ready first; it must wait for the root.
        fixture.residency.demand(child, 10);
        let requests = fixture.residency.take_load_requests(8);
        assert!(requests.contains(&child) && requests.contains(&root));
        fixture.residency.complete_load(child, vec![2_u8; 64]);
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish");
        assert_eq!(
            fixture.residency.stats().resident,
            0,
            "child waits for root"
        );

        fixture.residency.complete_load(root, vec![1_u8; 32]);
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish");
        let stats = fixture.residency.stats();
        assert_eq!(stats.resident, 2, "root then child publish together");
        assert_eq!(stats.resident_bytes, 96);

        let root_record = fixture.gpu_data.page_table.get(root).expect("root record");
        assert_eq!(root_record.byte_length, 32);
        assert_eq!(root_record.resident_generation, 1);
        let child_record = fixture
            .gpu_data
            .page_table
            .get(child)
            .expect("child record");
        assert_eq!(child_record.byte_length, 64);
        assert_ne!(child_record.byte_offset, root_record.byte_offset);

        fixture.device.wait_idle().expect("idle");
    }

    #[test]
    fn budget_evicts_lru_leaves_but_never_roots_or_resident_parents() {
        let Some(mut fixture) = fixture(160) else {
            return;
        };
        let root = insert_page(&mut fixture, None, true);
        let leaf_a = insert_page(&mut fixture, Some(root), false);
        let leaf_b = insert_page(&mut fixture, Some(root), false);

        let root_request = *fixture
            .residency
            .take_load_requests(1)
            .first()
            .expect("root requested");
        fixture
            .residency
            .complete_load(root_request, vec![0_u8; 96]);
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish root");

        fixture.residency.begin_frame();
        fixture.residency.demand(leaf_a, 1);
        for handle in fixture.residency.take_load_requests(8) {
            fixture.residency.complete_load(handle, vec![0_u8; 48]);
        }
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish leaf a");
        assert_eq!(fixture.residency.stats().resident, 2);

        // A newer leaf over budget evicts the older leaf, never the root.
        fixture.residency.begin_frame();
        fixture.residency.demand(leaf_b, 1);
        for handle in fixture.residency.take_load_requests(8) {
            fixture.residency.complete_load(handle, vec![0_u8; 48]);
        }
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish leaf b");
        let stats = fixture.residency.stats();
        assert_eq!(stats.resident, 2, "root + one leaf within budget");
        assert_eq!(stats.evictions, 1);
        assert_eq!(
            fixture
                .gpu_data
                .page_table
                .get(leaf_a)
                .expect("leaf a record")
                .byte_length,
            0,
            "the older leaf evicted"
        );
        assert_eq!(
            fixture
                .gpu_data
                .page_table
                .get(root)
                .expect("root record")
                .byte_length,
            96,
            "the guaranteed root stays"
        );

        fixture.device.wait_idle().expect("idle");
    }

    /// Among pages every view still wants, the cheapest reader's page is the one that
    /// goes. Recency alone cannot express this: both were demanded on the same frame, so
    /// without the class tiebreak the choice is whichever the hash map happened to reach
    /// first, and half the time that is the page the camera is drawing from.
    #[test]
    fn pages_demanded_together_evict_the_gathers_before_the_cameras() {
        let Some(mut fixture) = fixture(200) else {
            return;
        };
        let root = insert_page(&mut fixture, None, true);
        let camera_leaf = insert_page(&mut fixture, Some(root), false);
        let gather_leaf = insert_page(&mut fixture, Some(root), false);
        let arriving = insert_page(&mut fixture, Some(root), false);

        let root_request = *fixture
            .residency
            .take_load_requests(1)
            .first()
            .expect("root requested");
        fixture
            .residency
            .complete_load(root_request, vec![0_u8; 96]);
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish root");

        fixture.residency.begin_frame();
        fixture.residency.demand(
            camera_leaf,
            crate::SceneViewClass::Camera.page_demand_priority(),
        );
        fixture.residency.demand(
            gather_leaf,
            crate::SceneViewClass::Gi.page_demand_priority(),
        );
        for handle in fixture.residency.take_load_requests(8) {
            fixture.residency.complete_load(handle, vec![0_u8; 48]);
        }
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish both leaves");
        assert_eq!(fixture.residency.stats().resident, 3, "root + two leaves");

        // Both are still wanted this frame, and a third page arrives over budget.
        fixture.residency.begin_frame();
        fixture.residency.demand(
            camera_leaf,
            crate::SceneViewClass::Camera.page_demand_priority(),
        );
        fixture.residency.demand(
            gather_leaf,
            crate::SceneViewClass::Gi.page_demand_priority(),
        );
        fixture.residency.demand(
            arriving,
            crate::SceneViewClass::Camera.page_demand_priority(),
        );
        for handle in fixture.residency.take_load_requests(8) {
            fixture.residency.complete_load(handle, vec![0_u8; 48]);
        }
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish the arriving page");

        assert_eq!(fixture.residency.stats().evictions, 1);
        assert_eq!(
            fixture
                .gpu_data
                .page_table
                .get(gather_leaf)
                .expect("gather leaf record")
                .byte_length,
            0,
            "the gather's page went"
        );
        assert_eq!(
            fixture
                .gpu_data
                .page_table
                .get(camera_leaf)
                .expect("camera leaf record")
                .byte_length,
            48,
            "the camera's page stayed"
        );

        fixture.device.wait_idle().expect("idle");
    }

    /// The class only breaks ties. Ordering by class FIRST would make one camera glance
    /// outrank a gather that has been reading its page every frame since — and because a
    /// page's priority never falls on its own, that protection would never expire.
    #[test]
    fn a_page_nobody_asks_for_goes_before_one_a_gather_still_reads() {
        let Some(mut fixture) = fixture(200) else {
            return;
        };
        let root = insert_page(&mut fixture, None, true);
        let glanced_at = insert_page(&mut fixture, Some(root), false);
        let gather_leaf = insert_page(&mut fixture, Some(root), false);
        let arriving = insert_page(&mut fixture, Some(root), false);

        let root_request = *fixture
            .residency
            .take_load_requests(1)
            .first()
            .expect("root requested");
        fixture
            .residency
            .complete_load(root_request, vec![0_u8; 96]);
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish root");

        // The camera passes it once and never looks again.
        fixture.residency.begin_frame();
        fixture.residency.demand(
            glanced_at,
            crate::SceneViewClass::Camera.page_demand_priority(),
        );
        for handle in fixture.residency.take_load_requests(8) {
            fixture.residency.complete_load(handle, vec![0_u8; 48]);
        }
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish the glanced-at page");

        fixture.residency.begin_frame();
        fixture.residency.demand(
            gather_leaf,
            crate::SceneViewClass::Gi.page_demand_priority(),
        );
        for handle in fixture.residency.take_load_requests(8) {
            fixture.residency.complete_load(handle, vec![0_u8; 48]);
        }
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish the gather's page");
        assert_eq!(fixture.residency.stats().resident, 3);

        // The gather is still reading; the camera's old page is not being read at all.
        fixture.residency.begin_frame();
        fixture.residency.demand(
            gather_leaf,
            crate::SceneViewClass::Gi.page_demand_priority(),
        );
        fixture.residency.demand(
            arriving,
            crate::SceneViewClass::Camera.page_demand_priority(),
        );
        for handle in fixture.residency.take_load_requests(8) {
            fixture.residency.complete_load(handle, vec![0_u8; 48]);
        }
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish the arriving page");

        assert_eq!(fixture.residency.stats().evictions, 1);
        assert_eq!(
            fixture
                .gpu_data
                .page_table
                .get(glanced_at)
                .expect("glanced-at record")
                .byte_length,
            0,
            "the page nobody reads went"
        );
        assert_eq!(
            fixture
                .gpu_data
                .page_table
                .get(gather_leaf)
                .expect("gather leaf record")
                .byte_length,
            48,
            "the page a gather reads every frame stayed"
        );

        fixture.device.wait_idle().expect("idle");
    }

    #[test]
    fn unregister_retires_resident_bytes_and_stale_loads_drop() {
        let Some(mut fixture) = fixture(1024) else {
            return;
        };
        let root = insert_page(&mut fixture, None, true);
        for handle in fixture.residency.take_load_requests(8) {
            fixture.residency.complete_load(handle, vec![0_u8; 32]);
        }
        fixture
            .residency
            .publish_ready(&mut fixture.gpu_data, &mut fixture.pending)
            .expect("publish");
        assert_eq!(fixture.residency.stats().resident_bytes, 32);

        fixture
            .residency
            .unregister_page(root, &mut fixture.gpu_data)
            .expect("unregister");
        assert_eq!(fixture.residency.stats().registered, 0);
        assert_eq!(fixture.residency.stats().resident_bytes, 0);

        // A late load for the removed page is dropped silently.
        fixture.residency.complete_load(root, vec![0_u8; 16]);
        assert_eq!(fixture.residency.stats().ready, 0);

        fixture.device.wait_idle().expect("idle");
    }
}
