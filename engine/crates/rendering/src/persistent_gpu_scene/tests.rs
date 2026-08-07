use saffron_geometry::glam::{Mat4, Vec3, Vec4};
use saffron_spatial::{
    DecisionScalar, QuantizedLocalPosition, QuantizedOrientation, WorldCellKey, WorldPosition,
};
use std::sync::Arc;

use crate::GpuLight;

use super::*;

fn device_handle(index: u32) -> GpuHandle {
    GpuHandle {
        index,
        generation: 1,
    }
}

fn material(index: u32) -> GpuSceneMaterialRecord {
    GpuSceneMaterialRecord {
        table: device_handle(index),
        source_revision: 1,
    }
}

fn page(index: u32, parent: Option<GpuScenePageHandle>) -> GpuScenePageRecord {
    GpuScenePageRecord {
        table: device_handle(index),
        parent,
        source_generation: 1,
        flags: 0,
    }
}

fn prototype(
    material: GpuSceneMaterialHandle,
    root_page: GpuScenePageHandle,
) -> GpuScenePrototypeRecord {
    GpuScenePrototypeRecord {
        geometry: device_handle(20),
        materials: Arc::from([material]),
        deformation: None,
        sdfs: Vec::new().into(),
        root_page,
        page_bounds: Arc::from([]),
        bounds: [0.0, 0.0, 0.0, 1.0],
        source_generation: 1,
        flags: 0,
        mechanics: [0; 4],
    }
}

fn static_transform() -> GpuSceneTransform {
    let position = WorldPosition::new(
        WorldCellKey::base(-4, 2, 9),
        QuantizedLocalPosition::new([10, 20, 30]).unwrap(),
    )
    .unwrap();
    let scale = [
        DecisionScalar::from_integer(1).unwrap(),
        DecisionScalar::from_integer(2).unwrap(),
        DecisionScalar::from_integer(1).unwrap(),
    ];
    GpuSceneTransform::Static(GpuSceneStaticTransform::new(
        position,
        QuantizedOrientation::identity(),
        scale,
        0,
    ))
}

fn instance(prototype: GpuScenePrototypeHandle) -> GpuSceneInstanceRecord {
    GpuSceneInstanceRecord {
        prototype,
        transform: static_transform(),
        material_overrides: Arc::from([]),
        deformation: None,
        source_generation: 1,
        flags: 0,
        combination: 0,
        vegetation: None,
    }
}

fn insert_shared(
    scene: &mut PersistentGpuScene,
) -> (
    GpuSceneMaterialHandle,
    GpuScenePageHandle,
    GpuScenePrototypeHandle,
) {
    let GpuSceneSharedDeltaResult::MaterialCreated(material) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(self::material(1)))
        .unwrap()
    else {
        panic!("material create result")
    };
    let GpuSceneSharedDeltaResult::PageCreated(page) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreatePage(self::page(2, None)))
        .unwrap()
    else {
        panic!("page create result")
    };
    let GpuSceneSharedDeltaResult::PrototypeCreated(prototype_handle) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(prototype(
            material, page,
        )))
        .unwrap()
    else {
        panic!("prototype create result")
    };
    (material, page, prototype_handle)
}

#[test]
fn static_transform_is_compact_and_exact() {
    let GpuSceneTransform::Static(transform) = static_transform() else {
        unreachable!()
    };
    assert_eq!(std::mem::size_of::<GpuSceneStaticTransform>(), 64);
    assert_eq!(transform.cell, [-4, 2, 9]);
    assert_eq!(transform.local_ticks, [10, 20, 30]);
    assert_eq!(transform.orientation, [0, 0, 0, i16::MAX]);
    assert_eq!(transform.scale, [65_536, 131_072, 65_536]);
}

#[test]
fn dynamic_transform_advances_current_to_previous() {
    let mut transform = GpuSceneDynamicTransform::stationary(Mat4::IDENTITY).unwrap();
    let next = Mat4::from_translation(Vec3::new(2.0, 3.0, 4.0));
    transform.advance(next).unwrap();
    assert_eq!(transform.previous, Mat4::IDENTITY);
    assert_eq!(transform.current, next);
    assert!(
        GpuSceneDynamicTransform::stationary(Mat4::from_cols_array(&[
            f32::NAN,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ]))
        .is_err()
    );
}

#[test]
fn handles_reuse_only_after_every_frame_slot_completes() {
    let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
    let GpuSceneSharedDeltaResult::MaterialCreated(first) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(1)))
        .unwrap()
    else {
        unreachable!()
    };
    scene
        .apply_shared_delta(GpuSceneSharedDelta::RemoveMaterial(first))
        .unwrap();
    let GpuSceneSharedDeltaResult::MaterialCreated(second) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(2)))
        .unwrap()
    else {
        unreachable!()
    };
    assert_ne!(first.raw().index, second.raw().index);
    scene.begin_frame(0).unwrap();
    let GpuSceneSharedDeltaResult::MaterialCreated(third) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(3)))
        .unwrap()
    else {
        unreachable!()
    };
    assert_ne!(first.raw().index, third.raw().index);
    scene.begin_frame(1).unwrap();
    let GpuSceneSharedDeltaResult::MaterialCreated(reused) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(4)))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(first.raw().index, reused.raw().index);
    assert_eq!(first.raw().generation + 1, reused.raw().generation);
    assert!(scene.material(first).is_none());
}

#[test]
fn updates_coalesce_and_batches_merge_consecutive_slots() {
    let limits = GpuSceneUploadLimits {
        max_batch_records: 2,
        max_batch_bytes: 1_024,
        max_frame_records: 4,
        max_frame_bytes: 2_048,
    };
    let mut scene = PersistentGpuScene::new(limits).unwrap();
    let GpuSceneSharedDeltaResult::MaterialCreated(first) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(1)))
        .unwrap()
    else {
        unreachable!()
    };
    let GpuSceneSharedDeltaResult::MaterialCreated(second) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(2)))
        .unwrap()
    else {
        unreachable!()
    };
    scene
        .apply_shared_delta(GpuSceneSharedDelta::UpdateMaterial {
            handle: first,
            record: material(9),
        })
        .unwrap();
    scene.begin_frame(0).unwrap();
    let batch = scene.stage_upload_batch(0).unwrap();
    assert_eq!(batch.record_count, 2);
    assert_eq!(batch.ranges.len(), 1);
    assert_eq!(batch.ranges[0].first_slot, first.raw().index);
    assert_eq!(batch.ranges[0].records.len(), 2);
    assert_eq!(batch.ranges[0].records[0].revision, 3);
    assert_eq!(second.raw().index, first.raw().index + 1);
    assert!(!batch.more_pending);
}

#[test]
fn sparse_overrides_are_references_and_strictly_ordered() {
    let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
    let (base, page, _) = insert_shared(&mut scene);
    let GpuSceneSharedDeltaResult::MaterialCreated(replacement) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(7)))
        .unwrap()
    else {
        unreachable!()
    };
    let GpuSceneSharedDeltaResult::PrototypeCreated(two_slots) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
            GpuScenePrototypeRecord {
                materials: Arc::from([base, base]),
                ..prototype(base, page)
            },
        ))
        .unwrap()
    else {
        unreachable!()
    };
    scene.create_world(GpuSceneWorldId(4)).unwrap();
    let mut record = instance(two_slots);
    record.material_overrides = Arc::from([
        GpuSceneMaterialOverride {
            slot: 1,
            material: replacement,
        },
        GpuSceneMaterialOverride {
            slot: 0,
            material: replacement,
        },
    ]);
    assert_eq!(
        scene
            .apply_world_delta(
                GpuSceneWorldId(4),
                GpuSceneWorldDelta::CreateInstance(record)
            )
            .unwrap_err(),
        GpuSceneError::MaterialOverrideOrder
    );
}

#[test]
fn prototype_update_preserves_live_override_slots() {
    let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
    let (base, page, _) = insert_shared(&mut scene);
    let GpuSceneSharedDeltaResult::PrototypeCreated(two_slots) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
            GpuScenePrototypeRecord {
                materials: Arc::from([base, base]),
                ..prototype(base, page)
            },
        ))
        .unwrap()
    else {
        unreachable!()
    };
    let world = GpuSceneWorldId(4);
    scene.create_world(world).unwrap();
    let mut record = instance(two_slots);
    record.material_overrides = Arc::from([GpuSceneMaterialOverride {
        slot: 1,
        material: base,
    }]);
    scene
        .apply_world_delta(world, GpuSceneWorldDelta::CreateInstance(record))
        .unwrap();
    assert_eq!(
        scene
            .apply_shared_delta(GpuSceneSharedDelta::UpdatePrototype {
                handle: two_slots,
                record: prototype(base, page),
            })
            .unwrap_err(),
        GpuSceneError::MaterialOverrideSlot { slot: 1, count: 1 }
    );
}

#[test]
fn referenced_shared_records_cannot_be_removed() {
    let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
    let (material, _, prototype) = insert_shared(&mut scene);
    scene.create_world(GpuSceneWorldId(1)).unwrap();
    scene
        .apply_world_delta(
            GpuSceneWorldId(1),
            GpuSceneWorldDelta::CreateInstance(instance(prototype)),
        )
        .unwrap();
    assert!(matches!(
        scene.apply_shared_delta(GpuSceneSharedDelta::RemovePrototype(prototype)),
        Err(GpuSceneError::ReferencedHandle {
            kind: "prototype",
            ..
        })
    ));
    assert!(matches!(
        scene.apply_shared_delta(GpuSceneSharedDelta::RemoveMaterial(material)),
        Err(GpuSceneError::ReferencedHandle {
            kind: "material",
            ..
        })
    ));
}

#[test]
fn snapshot_rebuild_preserves_live_handles_and_queues_full_upload() {
    let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
    let (_, _, prototype) = insert_shared(&mut scene);
    let world = GpuSceneWorldId(8);
    scene.create_world(world).unwrap();
    let GpuSceneWorldDeltaResult::InstanceCreated(instance_handle) = scene
        .apply_world_delta(
            world,
            GpuSceneWorldDelta::CreateInstance(instance(prototype)),
        )
        .unwrap()
    else {
        unreachable!()
    };
    let snapshot = scene.snapshot();
    let mut rebuilt =
        PersistentGpuScene::from_snapshot(snapshot, GpuSceneUploadLimits::default()).unwrap();
    assert!(rebuilt.prototype(prototype).is_some());
    assert!(rebuilt.instance(world, instance_handle).is_some());
    rebuilt.begin_frame(0).unwrap();
    let batch = rebuilt.stage_upload_batch(0).unwrap();
    assert_eq!(batch.record_count, 4);
    assert!(!batch.more_pending);
    assert!(rebuilt.views.is_empty());
}

#[test]
fn view_state_is_independent_from_shared_and_world_records() {
    let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
    scene.create_world(GpuSceneWorldId(1)).unwrap();
    scene.create_world(GpuSceneWorldId(2)).unwrap();
    scene
        .create_view(GpuSceneViewId(10), GpuSceneWorldId(1))
        .unwrap();
    scene
        .create_view(GpuSceneViewId(11), GpuSceneWorldId(1))
        .unwrap();
    scene
        .view_mut(GpuSceneViewId(10))
        .unwrap()
        .publish(3, 4)
        .unwrap();
    assert!(scene.view(GpuSceneViewId(10)).unwrap().history_valid);
    assert!(!scene.view(GpuSceneViewId(11)).unwrap().history_valid);
    scene
        .view_mut(GpuSceneViewId(10))
        .unwrap()
        .invalidate(GpuSceneHistoryInvalidation::CameraCut)
        .unwrap();
    assert_eq!(
        scene.view(GpuSceneViewId(10)).unwrap().invalidation,
        GpuSceneHistoryInvalidation::CameraCut
    );
    assert_eq!(scene.world_revision(GpuSceneWorldId(2)).unwrap(), 0);
}

#[test]
fn page_updates_reject_cycles() {
    let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
    let GpuSceneSharedDeltaResult::PageCreated(root) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreatePage(page(1, None)))
        .unwrap()
    else {
        unreachable!()
    };
    let GpuSceneSharedDeltaResult::PageCreated(child) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreatePage(page(2, Some(root))))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(
        scene
            .apply_shared_delta(GpuSceneSharedDelta::UpdatePage {
                handle: root,
                record: page(1, Some(child)),
            })
            .unwrap_err(),
        GpuSceneError::PageCycle
    );
}

#[test]
fn light_validation_rejects_non_finite_payloads() {
    let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
    let world = GpuSceneWorldId(1);
    scene.create_world(world).unwrap();
    let light = GpuSceneLightRecord {
        light: GpuLight {
            position_range: Vec4::new(0.0, 0.0, 0.0, f32::INFINITY),
            color_intensity: Vec4::ONE,
            direction_type: Vec4::ZERO,
            spot_cos: Vec4::ZERO,
        },
        source_revision: 1,
    };
    assert_eq!(
        scene
            .apply_world_delta(world, GpuSceneWorldDelta::CreateLight(light))
            .unwrap_err(),
        GpuSceneError::NonFinite("light")
    );
}

/// A moved instance reports one swept box per cooked leaf page — one cluster or brick each —
/// tight around that cluster's own extent over BOTH poses, rather than one box over the whole
/// prototype.
#[test]
fn moved_bounds_are_reported_per_leaf_cluster() {
    let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
    let GpuSceneSharedDeltaResult::MaterialCreated(material) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(self::material(1)))
        .unwrap()
    else {
        panic!("material create result")
    };
    let GpuSceneSharedDeltaResult::PageCreated(root) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreatePage(self::page(2, None)))
        .unwrap()
    else {
        panic!("page create result")
    };
    // Two unit pages ten metres apart along X: a canopy's clusters sit far from its trunk's, and
    // the whole point of page granularity is that the gap between them stays clean.
    let mut record = prototype(material, root);
    record.bounds = [5.0, 0.0, 0.0, 6.0];
    record.page_bounds = Arc::from([
        GpuScenePageBounds {
            min: [-0.5, -0.5, -0.5],
            max: [0.5, 0.5, 0.5],
        },
        GpuScenePageBounds {
            min: [9.5, -0.5, -0.5],
            max: [10.5, 0.5, 0.5],
        },
    ]);
    let GpuSceneSharedDeltaResult::PrototypeCreated(prototype_handle) = scene
        .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(record))
        .unwrap()
    else {
        panic!("prototype create result")
    };
    scene.create_world(GpuSceneWorldId(1)).unwrap();
    let mut instance = self::instance(prototype_handle);
    // Three metres along X between the two poses: a page's box has to cover where the content
    // was and where it is, because both frames' shadow pages have to re-render.
    instance.transform = GpuSceneTransform::Dynamic(
        GpuSceneDynamicTransform::new(
            Mat4::from_translation(Vec3::new(3.0, 0.0, 0.0)),
            Mat4::IDENTITY,
        )
        .unwrap(),
    );
    scene
        .apply_world_delta(
            GpuSceneWorldId(1),
            GpuSceneWorldDelta::CreateInstance(instance),
        )
        .unwrap();

    let (moved, overflow) = scene.take_moved_bounds();
    assert!(!overflow, "one instance is far below the cap");
    assert_eq!(moved.len(), 2, "one swept box per cooked page");
    // Exactly the union of each page's two poses, in the prototype's cooked page order: 1 m of
    // page plus 3 m of travel along X from that page's own rest position, and the page's own 1 m
    // across the axes it did not move on. Positionally, so a swap that put the canopy's box where
    // the trunk's belongs is a failure — a box over one pose measures 1 m along X and a box over
    // the prototype's 12 m reach measures far more, so this pins the sweep and the split alike.
    let expected: [([f32; 3], [f32; 3]); 2] = [
        ([-0.5, -0.5, -0.5], [3.5, 0.5, 0.5]),
        ([9.5, -0.5, -0.5], [13.5, 0.5, 0.5]),
    ];
    for (page, ((min, max), (want_min, want_max))) in moved.iter().zip(expected).enumerate() {
        assert_eq!(*min, want_min, "page {page} sweeps from its own rest pose");
        assert_eq!(*max, want_max, "page {page} sweeps to its own moved pose");
    }
}
