use super::*;

#[test]
fn tick_writes_pose_override_on_driven_bone() {
    // The acceptance smoke: one rig + a clip loader, ticked once in Play, leaves a
    // PoseOverride on the driven bone holding the sampled value.
    let clip_id = Uuid(7);
    let (mut scene, _rig, bones) = rig_scene(2, clip_id);
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(translate_clip(0, 10.0));

    // Half a second into the 1s clip: the joint-0 translation lerps to 5 on +X.
    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);

    let over = scene
        .component::<PoseOverride>(bones[0])
        .expect("a driven bone gets a PoseOverride");
    assert!(over.translation.distance(Vec3::new(5.0, 0.0, 0.0)) < 1e-4);
    // The untracked second bone keeps its authored rest translation.
    let over1 = scene
        .component::<PoseOverride>(bones[1])
        .expect("every bone gets a PoseOverride seeded from rest");
    assert!(over1.translation.distance(Vec3::new(1.0, 0.0, 0.0)) < 1e-4);
}

#[test]
fn weights_track_writes_override_and_clears_on_stop() {
    let clip_id = Uuid(21);
    let (mut scene, _c, nodes) = node_forest_scene(clip_id);
    scene
        .add_component(
            nodes[0],
            MorphComponent {
                weights: vec![0.0, 0.0],
                names: vec!["a".to_string(), "b".to_string()],
            },
        )
        .unwrap();
    let clip = AnimClip {
        name: "w".to_string(),
        duration: 1.0,
        tracks: vec![AnimTrack {
            target: AnimTarget::Node,
            index: -1,
            target_name: "NodeA".to_string(),
            path: AnimPath::Weights,
            interp: AnimInterp::Linear,
            morph_count: 2,
            times: vec![0.0, 1.0],
            values: vec![0.0, 0.0, 1.0, 0.5],
        }],
    };
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(clip);

    // Half a second into the 1s clip: lane 0 lerps 0→1 to 0.5, lane 1 lerps 0→0.5 to 0.25.
    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);
    let (w0, w1) = scene
        .with_component::<MorphWeightOverride, _>(nodes[0], |m| (m.weights[0], m.weights[1]))
        .expect("a weights track writes a MorphWeightOverride");
    assert!((w0 - 0.5).abs() < 1e-4 && (w1 - 0.25).abs() < 1e-4);

    // Inactive (Edit, no preview) clears the runtime-only override + the binding, so the
    // mesh reverts to the durable MorphComponent weights.
    tick_animation(&mut runtime, &mut scene, 0.0, AnimMode::Edit, &mut load);
    assert!(!scene.has_component::<MorphWeightOverride>(nodes[0]));
    assert_eq!(runtime.node_bindings.len(), 0);
}

#[test]
fn no_clip_clears_overrides_and_drops_state() {
    // Seed an override + transition/last-pose state on a rig with an unset clip
    // (`Uuid(0)` ⇒ no clip resolves): the overrides are removed and the maps cleared.
    let (mut scene, rig, bones) = rig_scene(1, Uuid(0));
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(AnimClip::default()); // never consulted for the unset clip
    scene
        .add_component(
            bones[0],
            PoseOverride {
                translation: Vec3::splat(9.0),
                ..PoseOverride::default()
            },
        )
        .unwrap();
    let key = scene.component::<IdComponent>(rig).unwrap().id.0;
    runtime.transitions.insert(key, TransitionState::default());
    runtime.last_pose.insert(key, vec![JointPose::default()]);

    tick_animation(&mut runtime, &mut scene, 0.1, AnimMode::Play, &mut load);

    assert!(!scene.has_component::<PoseOverride>(bones[0]));
    assert!(!runtime.transitions.contains_key(&key));
    assert!(runtime.last_pose(key).is_none());
}

#[test]
fn edit_without_preview_is_inert() {
    // Edit with no preview rig: nothing animates, so no override appears.
    let (mut scene, _rig, root_bone, clip) = spin_preview_scene();
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(clip);
    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load);
    assert!(
        !scene.has_component::<PoseOverride>(root_bone),
        "edit without preview is inert"
    );
}

#[test]
fn preview_writes_override_and_advances() {
    // Edit + preview + playing: the playhead reaches 0.5 and the override holds the 45°
    // Y rotation, while the rest-pose Transform stays at identity and world composition
    // prefers the override.
    let (mut scene, rig, root_bone, clip) = spin_preview_scene();
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(clip);
    scene
        .with_component_mut::<AnimationPlayer, _>(rig, |p| {
            p.preview_in_edit = true;
            p.playing = true;
            p.time = 0.0;
        })
        .unwrap();

    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load);

    let time = scene
        .with_component::<AnimationPlayer, _>(rig, |p| p.time)
        .unwrap();
    assert!(
        (time - 0.5).abs() < EPS,
        "edit preview advances the playhead"
    );

    let q45 = Quat::from_axis_angle(Vec3::Y, 45.0_f32.to_radians());
    let over = scene
        .component::<PoseOverride>(root_bone)
        .expect("preview writes a pose override");
    assert!(
        quat_close(over.rotation, q45),
        "override holds the sampled 45° rotation"
    );

    // The rest-pose Transform stays at identity (non-destructive Edit preview).
    let rest_rotation = scene
        .with_component::<Transform, _>(root_bone, |t| t.rotation)
        .unwrap();
    assert!(
        rest_rotation.length() < EPS,
        "rest-pose Transform stays at identity"
    );

    // World composition prefers the override.
    scene.update_world_transforms();
    assert!(
        quat_close(scene.world_rotation(root_bone), q45),
        "world transform reflects the override"
    );
}

#[test]
fn clearing_preview_reverts_to_rest() {
    // After previewing the override, clearing preview removes it and the bone reverts to
    // rest on the next tick.
    let (mut scene, rig, root_bone, clip) = spin_preview_scene();
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(clip);
    scene
        .with_component_mut::<AnimationPlayer, _>(rig, |p| {
            p.preview_in_edit = true;
            p.playing = true;
            p.time = 0.0;
        })
        .unwrap();
    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load);
    assert!(scene.has_component::<PoseOverride>(root_bone));

    scene
        .with_component_mut::<AnimationPlayer, _>(rig, |p| p.preview_in_edit = false)
        .unwrap();
    tick_animation(&mut runtime, &mut scene, 0.0, AnimMode::Edit, &mut load);
    assert!(
        !scene.has_component::<PoseOverride>(root_bone),
        "clearing preview removes the override"
    );
    scene.update_world_transforms();
    assert!(
        quat_close(scene.world_rotation(root_bone), Quat::IDENTITY),
        "bone reverts to rest after preview clears"
    );
}

#[test]
fn play_animates_without_preview() {
    // Play animates every rig regardless of preview_in_edit.
    let (mut scene, rig, root_bone, clip) = spin_preview_scene();
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(clip);
    scene
        .with_component_mut::<AnimationPlayer, _>(rig, |p| {
            p.preview_in_edit = false;
            p.playing = true;
            p.time = 0.0;
        })
        .unwrap();
    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);
    assert!(
        scene.has_component::<PoseOverride>(root_bone),
        "play animates without preview"
    );
}

#[test]
fn last_pose_snapshot_records_the_driven_pose() {
    let clip_id = Uuid(5);
    let (mut scene, rig, _bones) = rig_scene(2, clip_id);
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(translate_clip(0, 8.0));
    let key = scene.component::<IdComponent>(rig).unwrap().id.0;

    tick_animation(&mut runtime, &mut scene, 0.25, AnimMode::Play, &mut load);
    let snapshot = runtime.last_pose(key).expect("last_pose snapshot exists");
    assert_eq!(snapshot.len(), 2);
    // Joint 0 lerps to 0.25 * 8 = 2 on +X at t = 0.25s.
    assert!(snapshot[0].translation.distance(Vec3::new(2.0, 0.0, 0.0)) < 1e-4);
}

#[test]
fn negative_cache_loads_a_broken_clip_once() {
    // A loader that fails is negative-cached: it is called exactly once, then the rig
    // drives to rest (an empty clip), and no override is dropped on later ticks.
    use std::cell::Cell;
    use std::rc::Rc;

    let calls = Rc::new(Cell::new(0u32));
    let calls_inner = Rc::clone(&calls);
    let (mut scene, _rig, bones) = rig_scene(1, Uuid(99));
    let mut runtime = AnimationRuntime::new();
    let mut load = move |_id| {
        calls_inner.set(calls_inner.get() + 1);
        Err(Error::ClipLoad("boom".to_string()))
    };

    tick_animation(&mut runtime, &mut scene, 0.1, AnimMode::Play, &mut load);
    tick_animation(&mut runtime, &mut scene, 0.1, AnimMode::Play, &mut load);
    assert_eq!(
        calls.get(),
        1,
        "a failed load is negative-cached, not retried"
    );
    // An empty clip resolves: the bone is driven to its rest pose, not cleared.
    let over = scene
        .component::<PoseOverride>(bones[0])
        .expect("an empty (negative-cached) clip still drives the rig to rest");
    assert!(over.translation.distance(Vec3::new(1.0, 0.0, 0.0)) < 1e-4);
}

#[test]
fn advance_time_once_clamps_and_stops() {
    let mut p = AnimationPlayer {
        time: 0.9,
        speed: 1.0,
        playing: true,
        wrap: Wrap::Once,
        ..AnimationPlayer::default()
    };
    advance_time(&mut p, 1.0, 0.5);
    assert_eq!(p.time, 1.0);
    assert!(!p.playing, "Once stops at the clip end");
}

#[test]
fn advance_time_loop_wraps() {
    let mut p = AnimationPlayer {
        time: 0.8,
        speed: 1.0,
        playing: true,
        wrap: Wrap::Loop,
        ..AnimationPlayer::default()
    };
    advance_time(&mut p, 1.0, 0.5);
    // 0.8 + 0.5 = 1.3, wrapped into [0, 1) = 0.3.
    assert!((p.time - 0.3).abs() < 1e-6);
    assert!(p.playing, "Loop keeps playing across the seam");
}

#[test]
fn advance_time_pingpong_bounces() {
    let mut p = AnimationPlayer {
        time: 0.8,
        speed: 1.0,
        playing: true,
        wrap: Wrap::PingPong,
        ping_forward: true,
        ..AnimationPlayer::default()
    };
    advance_time(&mut p, 1.0, 0.5);
    // 0.8 + 0.5 = 1.3 → reflected to 2 - 1.3 = 0.7, direction flips to backward.
    assert!((p.time - 0.7).abs() < 1e-6);
    assert!(!p.ping_forward, "PingPong flips direction at the end");
}
