use std::marker::PhantomData;

use super::*;
use crate::MAX_FRAMES_IN_FLIGHT;

pub(super) struct SceneSlot<T> {
    generation: u32,
    value: Option<T>,
}

pub(super) struct RetiredSceneSlot {
    index: u32,
    pending_frames: u64,
}

pub(super) struct SceneTable<T, K> {
    slots: Vec<SceneSlot<T>>,
    reusable: Vec<u32>,
    retired: Vec<RetiredSceneSlot>,
    marker: PhantomData<fn() -> K>,
}

impl<T, K> Default for SceneTable<T, K> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            reusable: Vec::new(),
            retired: Vec::new(),
            marker: PhantomData,
        }
    }
}

impl<T, K> SceneTable<T, K> {
    pub(super) fn insert(
        &mut self,
        value: T,
        kind: &'static str,
    ) -> Result<GpuSceneHandle<K>, GpuSceneError> {
        let index = if let Some(index) = self.reusable.pop() {
            index
        } else {
            let index =
                u32::try_from(self.slots.len()).map_err(|_| GpuSceneError::TableCapacity(kind))?;
            self.slots.push(SceneSlot {
                generation: 1,
                value: None,
            });
            index
        };
        let slot = &mut self.slots[index as usize];
        debug_assert!(slot.value.is_none());
        slot.value = Some(value);
        Ok(GpuSceneHandle::from_raw(GpuHandle {
            index,
            generation: slot.generation,
        }))
    }

    pub(super) fn get(&self, handle: GpuSceneHandle<K>) -> Option<&T> {
        let slot = self.slots.get(handle.raw.index as usize)?;
        (slot.generation == handle.raw.generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    pub(super) fn update(
        &mut self,
        handle: GpuSceneHandle<K>,
        value: T,
        kind: &'static str,
    ) -> Result<(), GpuSceneError> {
        let slot = self
            .slots
            .get_mut(handle.raw.index as usize)
            .filter(|slot| slot.generation == handle.raw.generation)
            .ok_or(GpuSceneError::StaleHandle {
                kind,
                handle: handle.raw,
            })?;
        let stored = slot.value.as_mut().ok_or(GpuSceneError::StaleHandle {
            kind,
            handle: handle.raw,
        })?;
        *stored = value;
        Ok(())
    }

    pub(super) fn remove(
        &mut self,
        handle: GpuSceneHandle<K>,
        kind: &'static str,
    ) -> Result<T, GpuSceneError> {
        let slot = self
            .slots
            .get_mut(handle.raw.index as usize)
            .filter(|slot| slot.generation == handle.raw.generation)
            .ok_or(GpuSceneError::StaleHandle {
                kind,
                handle: handle.raw,
            })?;
        let value = slot.value.take().ok_or(GpuSceneError::StaleHandle {
            kind,
            handle: handle.raw,
        })?;
        self.retired.push(RetiredSceneSlot {
            index: handle.raw.index,
            pending_frames: live_frame_mask(),
        });
        Ok(value)
    }

    pub(super) fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<(), GpuSceneError> {
        let completed = frame_bit(completed_frame_slot)?;
        let mut ready = Vec::new();
        self.retired.retain_mut(|retired| {
            retired.pending_frames &= !completed;
            if retired.pending_frames == 0 {
                ready.push(retired.index);
                false
            } else {
                true
            }
        });
        for index in ready {
            let slot = &mut self.slots[index as usize];
            if let Some(generation) = slot.generation.checked_add(1) {
                slot.generation = generation;
                self.reusable.push(index);
            }
        }
        Ok(())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.slots.iter().all(|slot| slot.value.is_none())
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (GpuSceneHandle<K>, &T)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.value.as_ref().map(|value| {
                (
                    GpuSceneHandle::from_raw(GpuHandle {
                        index: index as u32,
                        generation: slot.generation,
                    }),
                    value,
                )
            })
        })
    }
}

pub(super) fn live_frame_mask() -> u64 {
    (1_u64 << MAX_FRAMES_IN_FLIGHT) - 1
}

pub(super) fn frame_bit(frame_slot: usize) -> Result<u64, GpuSceneError> {
    if frame_slot >= MAX_FRAMES_IN_FLIGHT || frame_slot >= u64::BITS as usize {
        return Err(GpuSceneError::FrameSlot {
            slot: frame_slot,
            count: MAX_FRAMES_IN_FLIGHT,
        });
    }
    Ok(1_u64 << frame_slot)
}

/// One slot in a complete reconstructible table snapshot.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuSceneSnapshotSlot<T> {
    /// Vacant slot carrying the last generation that occupied it.
    Vacant { generation: u32 },
    /// Live slot and immutable snapshot value.
    Occupied { generation: u32, value: T },
}

/// Complete table snapshot preserving live handles and stale-handle rejection.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSceneTableSnapshot<T> {
    /// Direct-index slots.
    pub slots: Vec<GpuSceneSnapshotSlot<T>>,
}

impl<T> Default for GpuSceneTableSnapshot<T> {
    fn default() -> Self {
        Self { slots: Vec::new() }
    }
}

impl<T: Clone, K> SceneTable<T, K> {
    pub(super) fn snapshot(&self) -> GpuSceneTableSnapshot<T> {
        GpuSceneTableSnapshot {
            slots: self
                .slots
                .iter()
                .map(|slot| match &slot.value {
                    Some(value) => GpuSceneSnapshotSlot::Occupied {
                        generation: slot.generation,
                        value: value.clone(),
                    },
                    None => GpuSceneSnapshotSlot::Vacant {
                        generation: slot.generation,
                    },
                })
                .collect(),
        }
    }

    pub(super) fn from_snapshot(snapshot: GpuSceneTableSnapshot<T>) -> Result<Self, GpuSceneError> {
        let mut table = Self::default();
        for (index, snapshot_slot) in snapshot.slots.into_iter().enumerate() {
            let index = u32::try_from(index)
                .map_err(|_| GpuSceneError::InvalidSnapshot("table exceeds u32 slots"))?;
            match snapshot_slot {
                GpuSceneSnapshotSlot::Vacant { generation } => {
                    if generation == 0 {
                        return Err(GpuSceneError::InvalidSnapshot("zero slot generation"));
                    }
                    let reusable_generation = generation.checked_add(1);
                    table.slots.push(SceneSlot {
                        generation: reusable_generation.unwrap_or(generation),
                        value: None,
                    });
                    if reusable_generation.is_some() {
                        table.reusable.push(index);
                    }
                }
                GpuSceneSnapshotSlot::Occupied { generation, value } => {
                    if generation == 0 {
                        return Err(GpuSceneError::InvalidSnapshot("zero slot generation"));
                    }
                    table.slots.push(SceneSlot {
                        generation,
                        value: Some(value),
                    });
                }
            }
        }
        table.reusable.reverse();
        Ok(table)
    }
}
