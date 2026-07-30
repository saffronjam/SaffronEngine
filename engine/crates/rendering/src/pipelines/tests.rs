use super::*;
use crate::device::SurfaceSource;
use crate::resources::BindlessFreeList;
use crate::validation_issue_count;
use std::sync::Mutex;

/// Builds a headless device + descriptors + the cache, or skips (no Vulkan ICD).
fn fixture_or_skip() -> Option<(Device, Descriptors, Pipelines)> {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return None;
        }
    };
    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
    let pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
    Some((device, descriptors, pipelines))
}

/// A `PsoKey` is the matchable cache key: equal tuples are equal + hash equal, a
/// differing flag is a distinct key. Runs on any host (the key is GPU-free logic).
#[test]
fn pso_key_distinguishes_every_variant() {
    use std::collections::HashSet;
    let base = PsoKey {
        shader: "shaders/mesh.spv".to_string(),
        unlit: false,
        wireframe: false,
        blend: false,
        alpha_to_coverage: false,
        sample_count: vk::SampleCountFlags::TYPE_1,
        mesh_shader: false,
    };
    let mut set = HashSet::new();
    set.insert(base.clone());
    // Re-inserting the identical key does not grow the set (the cache-hit path).
    assert!(!set.insert(base.clone()));
    assert_eq!(set.len(), 1);
    // Each toggled flag is a distinct key.
    for variant in [
        PsoKey {
            unlit: true,
            ..base.clone()
        },
        PsoKey {
            wireframe: true,
            ..base.clone()
        },
        PsoKey {
            blend: true,
            ..base.clone()
        },
        PsoKey {
            alpha_to_coverage: true,
            ..base.clone()
        },
        PsoKey {
            sample_count: vk::SampleCountFlags::TYPE_4,
            ..base.clone()
        },
        // The mesh-stage executor is a distinct PSO over the same records, so it must not
        // collide with the indexed one in the cache.
        PsoKey {
            mesh_shader: true,
            ..base.clone()
        },
    ] {
        assert!(set.insert(variant));
    }
    assert_eq!(set.len(), 7);
}

/// The same variant requested twice returns the same `Arc`, distinct variants produce distinct
/// entries, and `pipeline_count` reflects the cache size. Skips when no Vulkan device is
/// present.
#[test]
fn request_executor_mesh_pipeline_caches_per_variant() {
    let Some((device, _descriptors, mut pipelines)) = fixture_or_skip() else {
        return;
    };
    let before = validation_issue_count();

    let lit = Material::default();
    // First request builds + caches; the second is a cache hit (same Arc).
    let a = pipelines
        .request_executor_mesh_pipeline(&lit, false, false)
        .expect("lit PSO builds on llvmpipe");
    let b = pipelines
        .request_executor_mesh_pipeline(&lit, false, false)
        .expect("second request hits the cache");
    assert!(
        Arc::ptr_eq(&a, &b),
        "the same variant returns the same cached Arc (one PSO)"
    );
    assert_eq!(pipelines.pipeline_count(), 1, "many requests, one PSO");
    assert_eq!(pipelines.pipelines_created(), 1);

    // The unlit permutation is a distinct cache entry.
    let unlit = Material {
        unlit: true,
        ..Material::default()
    };
    let c = pipelines
        .request_executor_mesh_pipeline(&unlit, false, false)
        .expect("unlit PSO builds");
    assert!(!Arc::ptr_eq(&a, &c), "unlit is a distinct PSO");
    assert_eq!(pipelines.pipeline_count(), 2);
    assert_eq!(pipelines.pipelines_created(), 2);

    // The wireframe permutation is a third distinct entry.
    let _wire = pipelines
        .request_executor_mesh_pipeline(&lit, true, false)
        .expect("wireframe PSO builds");
    assert_eq!(
        pipelines.pipeline_count(),
        3,
        "wireframe adds a third distinct PSO"
    );
    assert_eq!(pipelines.pipelines_created(), 3);

    drop(a);
    drop(b);
    drop(c);
    drop(_wire);
    drop(pipelines);
    drop(_descriptors);
    device.wait_idle().expect("idle before teardown");
    drop(device);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the PSO cache build + teardown must be validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}

/// The AA PSOs build on llvmpipe (motion graphics + TAA/FXAA compute), and
/// `set_sample_count` clears the sample-count-baked cache + drops the depth-prepass so
/// the next request rebuilds for the new count, while leaving the always-1× motion PSO
/// intact. Skips when no device.
#[test]
fn aa_pipelines_build_and_sample_count_change_clears_the_baked_cache() {
    let Some((device, descriptors, mut pipelines)) = fixture_or_skip() else {
        return;
    };
    let before = validation_issue_count();

    // The three AA PSOs compile on llvmpipe.
    let motion = pipelines
        .request_motion_executor()
        .expect("motion PSO builds");
    let _taa = pipelines
        .request_taa(descriptors.taa_set_layout())
        .expect("taa PSO builds");
    let _fxaa = pipelines
        .request_fxaa(descriptors.fxaa_set_layout())
        .expect("fxaa PSO builds");

    // A sample-count-baked mesh PSO + the depth-prepass populate the count-keyed cache.
    let lit = Material::default();
    let _mesh = pipelines
        .request_executor_mesh_pipeline(&lit, false, false)
        .expect("mesh PSO builds");
    let _depth = pipelines
        .request_depth_prepass_executor()
        .expect("depth-prepass builds");
    assert_eq!(pipelines.pipeline_count(), 1, "one mesh PSO cached at 1×");
    assert_eq!(pipelines.sample_count(), vk::SampleCountFlags::TYPE_1);

    // Changing the count clears the mesh cache + drops the depth-prepass so they rebuild
    // for the new count; the motion PSO (always 1×) is untouched (same Arc on re-request).
    pipelines.set_sample_count(vk::SampleCountFlags::TYPE_4);
    assert_eq!(pipelines.sample_count(), vk::SampleCountFlags::TYPE_4);
    assert_eq!(
        pipelines.pipeline_count(),
        0,
        "the sample-count change cleared the mesh cache"
    );
    let motion_again = pipelines
        .request_motion_executor()
        .expect("motion re-request");
    assert!(
        Arc::ptr_eq(&motion, &motion_again),
        "the always-1× motion PSO survives a sample-count change"
    );
    // The next mesh request rebuilds at the new count.
    let _mesh4 = pipelines
        .request_executor_mesh_pipeline(&lit, false, false)
        .expect("mesh PSO rebuilds at 4×");
    assert_eq!(pipelines.pipeline_count(), 1);

    drop(motion);
    drop(motion_again);
    drop(_taa);
    drop(_fxaa);
    drop(_mesh);
    drop(_depth);
    drop(_mesh4);
    drop(pipelines);
    drop(descriptors);
    device.wait_idle().expect("idle before teardown");
    drop(device);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the AA PSO build + teardown must be validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}
