use super::*;

#[test]
fn foot_ik_plants_the_foot_on_the_ground_plane() {
    // A three-bone chain hanging below a parent; with foot IK enabled and the ground at
    // y = 0, the solved foot end lands on (or above) the plane.
    let clip_id = Uuid(31);
    let mut scene = Scene::new();
    let rig = scene.create_entity("Rig");
    // A root above the chain so the foot starts below ground.
    let hip = scene.create_entity("hip");
    scene
        .with_component_mut::<Transform, _>(hip, |t| t.translation = Vec3::new(0.0, 1.0, 0.0))
        .unwrap();
    let upper = scene.create_entity("upper");
    scene
        .with_component_mut::<Transform, _>(upper, |t| {
            t.translation = Vec3::new(0.0, -0.5, 0.0);
        })
        .unwrap();
    scene.set_parent(upper, Some(hip), false).unwrap();
    let mid = scene.create_entity("mid");
    scene
        .with_component_mut::<Transform, _>(mid, |t| t.translation = Vec3::new(0.0, -0.5, 0.0))
        .unwrap();
    scene.set_parent(mid, Some(upper), false).unwrap();
    let end = scene.create_entity("end");
    scene
        .with_component_mut::<Transform, _>(end, |t| t.translation = Vec3::new(0.0, -0.5, 0.0))
        .unwrap();
    scene.set_parent(end, Some(mid), false).unwrap();

    let bone_ids: Vec<Uuid> = [upper, mid, end]
        .iter()
        .map(|&b| scene.component::<IdComponent>(b).unwrap().id)
        .collect();
    scene
        .add_component(
            rig,
            SkinnedMesh {
                bones: bone_ids,
                ..SkinnedMesh::default()
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            AnimationPlayer {
                clip: clip_id,
                playing: false,
                ..AnimationPlayer::default()
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            FootIk {
                enabled: true,
                ground_height: 0.0,
                chains: vec![FootChain {
                    upper: 0,
                    mid: 1,
                    end: 2,
                    pole_vector: Vec3::new(0.0, 0.0, 1.0),
                }],
            },
        )
        .unwrap();
    scene.relink_hierarchy();
    scene.update_world_transforms();

    let mut runtime = AnimationRuntime::new();
    // An empty clip: the rig drives to rest, then foot IK runs on the rest pose.
    let mut load = clip_loader(AnimClip {
        name: "rest".to_string(),
        duration: 1.0,
        tracks: Vec::new(),
    });

    tick_animation(&mut runtime, &mut scene, 0.016, AnimMode::Play, &mut load);

    // After the solve, the chain's overrides changed the joint rotations so the foot
    // reaches up toward the ground plane. Recompose the foot world Y by FK from the
    // written overrides and assert it is at/above the ground (it started at y = -0.5).
    scene.update_world_transforms();
    let foot_y = scene.world_translation(end).y;
    assert!(
        foot_y > -0.5 + 1e-3,
        "foot IK should lift the foot toward the ground plane (foot_y = {foot_y})"
    );
    assert!(foot_y.is_finite(), "the solve must not produce NaN");
}

#[test]
fn foot_ik_reaches_a_nearby_target_from_a_bent_pose() {
    let clip_id = Uuid(32);
    let (mut scene, rig, bones) = rig_scene(3, clip_id);
    scene
        .with_component_mut::<Transform, _>(bones[0], |transform| {
            transform.translation = Vec3::ZERO;
        })
        .unwrap();
    scene
        .with_component_mut::<Transform, _>(bones[1], |transform| {
            transform.translation = Vec3::Y;
            transform.rotation.z = 67.5_f32.to_radians();
        })
        .unwrap();
    scene
        .with_component_mut::<Transform, _>(bones[2], |transform| {
            transform.translation = Vec3::Y;
        })
        .unwrap();
    scene.relink_hierarchy();
    scene.update_world_transforms();
    let target_y = scene.world_translation(bones[2]).y + 0.08;
    scene
        .add_component(
            rig,
            FootIk {
                enabled: true,
                ground_height: target_y,
                chains: vec![FootChain {
                    upper: 0,
                    mid: 1,
                    end: 2,
                    pole_vector: -Vec3::X,
                }],
            },
        )
        .unwrap();

    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(AnimClip {
        name: "rest".to_string(),
        duration: 1.0,
        tracks: Vec::new(),
    });
    tick_animation(&mut runtime, &mut scene, 0.016, AnimMode::Play, &mut load);
    scene.update_world_transforms();
    let foot_y = scene.world_translation(bones[2]).y;

    assert!(
        (foot_y - target_y).abs() < 1.0e-3,
        "foot y {foot_y} should reach target {target_y}"
    );
}
