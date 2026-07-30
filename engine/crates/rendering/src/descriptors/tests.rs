use super::*;
use crate::device::SurfaceSource;
use crate::validation_issue_count;

#[test]
fn material_params_binding_covers_vertex_and_fragment_consumers() {
    let binding = instance_layout_bindings(false)[0];
    assert_eq!(binding.binding, 2);
    assert_eq!(binding.descriptor_type, vk::DescriptorType::STORAGE_BUFFER);
    assert_eq!(
        binding.stage_flags,
        vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT
    );
}

/// Builds a headless device or skips the test (no Vulkan ICD in this toolbox).
fn device_or_skip() -> Option<Device> {
    match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => Some(device),
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            None
        }
    }
}

/// Claiming N slots after slot 0 hands out 1..=N; dropping those into the free-list then
/// claiming N more reuses every reclaimed slot LIFO and never grows the high-water mark past
/// N+1 — the bounded-pool invariant.
#[test]
fn slot_allocator_reuses_freed_slots_and_stays_bounded() {
    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let mut allocator = SlotAllocator {
        next_index: 0,
        cap: MAX_BINDLESS_TEXTURES,
        free_list: Arc::clone(&free_list),
    };

    // Slot 0 is the default white, claimed first.
    assert_eq!(allocator.claim(), Some(DEFAULT_WHITE_SLOT));

    // Claim five more: the high-water mark grows 1..=5.
    let claimed: Vec<u32> = (0..5).map(|_| allocator.claim().unwrap()).collect();
    assert_eq!(claimed, vec![1, 2, 3, 4, 5]);
    assert_eq!(allocator.next_index, 6);

    // Return them to the free-list (a GpuTexture drop pushes its slot). The order
    // mimics texture drops: push 1..=5.
    {
        let mut free = free_list.lock().unwrap();
        free.extend_from_slice(&claimed);
    }
    assert_eq!(free_list.lock().unwrap().len(), 5);

    // Claim five more: every slot is reused (LIFO, so 5,4,3,2,1) and the
    // high-water mark does NOT grow past 6.
    let reclaimed: Vec<u32> = (0..5).map(|_| allocator.claim().unwrap()).collect();
    assert_eq!(reclaimed, vec![5, 4, 3, 2, 1]);
    assert_eq!(
        allocator.next_index, 6,
        "the free-list reuse kept next_index bounded — no growth past the prior high-water mark"
    );
    assert!(free_list.lock().unwrap().is_empty());

    // The next claim with an empty free-list grows the high-water mark again.
    assert_eq!(allocator.claim(), Some(6));
    assert_eq!(allocator.next_index, 7);
}

/// A bounded allocator hands out every slot up to its capacity, then returns `None`
/// (never an out-of-range index) — the fix for the SDF-array overflow that wrote past
/// `dstArrayElement`. Freeing a slot lets the next claim reuse it below the cap.
#[test]
fn slot_allocator_returns_none_when_full() {
    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let mut allocator = SlotAllocator {
        next_index: 0,
        cap: 3,
        free_list: Arc::clone(&free_list),
    };

    assert_eq!(allocator.claim(), Some(0));
    assert_eq!(allocator.claim(), Some(1));
    assert_eq!(allocator.claim(), Some(2));
    // The array is full: every further claim is refused, never an out-of-range slot.
    assert_eq!(allocator.claim(), None);
    assert_eq!(allocator.claim(), None);
    assert_eq!(allocator.next_index, 3);

    // Reclaiming a slot lets the next claim reuse it (still within the cap).
    free_list.lock().unwrap().push(1);
    assert_eq!(allocator.claim(), Some(1));
    assert_eq!(allocator.claim(), None);
}

/// Two threads claiming slots concurrently never alias a slot: the thumbnail worker and the
/// main thread both claim, so every handed-out index across both must be distinct.
#[test]
fn concurrent_claims_never_alias_a_slot() {
    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    const PER_THREAD: usize = 2000;
    let allocator = Arc::new(Mutex::new(SlotAllocator {
        next_index: 0,
        cap: (2 * PER_THREAD) as u32,
        free_list: Arc::clone(&free_list),
    }));

    let mut handles = Vec::new();
    for _ in 0..2 {
        let allocator = Arc::clone(&allocator);
        handles.push(std::thread::spawn(move || {
            let mut claimed = Vec::with_capacity(PER_THREAD);
            for _ in 0..PER_THREAD {
                claimed.push(allocator.lock().unwrap().claim().unwrap());
            }
            claimed
        }));
    }

    let mut all: Vec<u32> = Vec::new();
    for handle in handles {
        all.extend(handle.join().expect("worker thread joins"));
    }

    // 4000 claims with no reuse: the high-water mark is exactly 4000, and every
    // index 0..4000 was handed out exactly once (no alias, no gap).
    assert_eq!(all.len(), 2 * PER_THREAD);
    assert_eq!(
        allocator.lock().unwrap().next_index,
        (2 * PER_THREAD) as u32
    );
    all.sort_unstable();
    let expected: Vec<u32> = (0..(2 * PER_THREAD) as u32).collect();
    assert_eq!(
        all, expected,
        "concurrent claims handed out every slot exactly once — no aliasing"
    );
}

/// The full descriptor infrastructure builds against a device with the bindless set created
/// update-after-bind, slot 0 reserved for the default white, and a validation-clean construct +
/// teardown. Skips when no Vulkan device is present.
#[test]
fn descriptors_build_and_teardown_is_validation_clean() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let before = validation_issue_count();

    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");

    // Slot 0 is the default white: the high-water mark starts at 1, the free-list
    // is empty, and every layout/sampler/set handle is non-null.
    assert_eq!(
        descriptors.texture_count(),
        1,
        "slot 0 (default white) is claimed at init"
    );
    assert_eq!(descriptors.free_count(), 0);
    assert_ne!(descriptors.bindless_set(), vk::DescriptorSet::null());
    assert_ne!(
        descriptors.bindless_set_layout(),
        vk::DescriptorSetLayout::null()
    );
    assert_ne!(descriptors.linear_sampler(), vk::Sampler::null());
    assert_ne!(descriptors.shadow_sampler(), vk::Sampler::null());

    // A claim after init hands out slot 1 (the first uploadable slot), and the
    // free-list reclaim path is wired through `free_list`.
    assert_eq!(descriptors.claim_slot(), Some(1));
    assert_eq!(descriptors.texture_count(), 2);

    drop(descriptors);
    device.wait_idle().expect("idle before teardown");
    drop(device);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the descriptor infrastructure's construct + teardown must be \
         validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}
