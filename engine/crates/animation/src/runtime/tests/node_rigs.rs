use super::*;

#[test]
fn node_player_writes_pose_override_on_bound_node() {
    // A skinless node-forest player ticks in Play and writes a PoseOverride onto the
    // node bound by name; the world transform reflects it.
    let clip_id = Uuid(20);
    let (mut scene, _c, nodes) = node_forest_scene(clip_id);
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(node_translate_clip("NodeA", 4.0));

    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);

    let over = scene
        .component::<PoseOverride>(nodes[0])
        .expect("the bound node gets a PoseOverride");
    assert!(over.translation.distance(Vec3::new(2.0, 0.0, 0.0)) < 1e-4);
    // The unbound nested node is untouched.
    assert!(!scene.has_component::<PoseOverride>(nodes[1]));
    scene.update_world_transforms();
    assert!(
        scene
            .world_translation(nodes[0])
            .distance(Vec3::new(2.0, 0.0, 0.0))
            < 1e-4
    );
}

#[test]
fn node_rig_ignores_bone_tracks_no_cross_routing() {
    // The joint-index-coupling hazard: a Bone track on a node-forest player must never
    // be interpreted as a node index. The node track drives its node; the bone track is
    // skipped (no panic, no stray override).
    let clip_id = Uuid(22);
    let (mut scene, _c, nodes) = node_forest_scene(clip_id);
    let mut clip = node_translate_clip("NodeA", 4.0);
    clip.tracks.push(AnimTrack {
        target: AnimTarget::Bone,
        index: 0,
        target_name: "NodeB".to_string(),
        path: AnimPath::Translation,
        interp: AnimInterp::Linear,
        morph_count: 0,
        times: vec![0.0, 1.0],
        values: vec![0.0, 0.0, 0.0, 9.0, 0.0, 0.0],
    });
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(clip);
    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);
    assert!(scene.has_component::<PoseOverride>(nodes[0]));
}

#[test]
fn node_binding_is_scoped_per_instance() {
    // Two instances of the same-named forest: each player binds its OWN NodeA, never the
    // other instance's (the global-scan cross-instance hazard).
    let clip_id = Uuid(23);
    let (mut scene, _c0, nodes0) = node_forest_scene(clip_id);
    // A second forest in the same scene.
    let container1 = scene.create_entity("Forest");
    let node_a1 = scene.create_entity("NodeA");
    scene.set_parent(node_a1, Some(container1), false).unwrap();
    scene
        .add_component(
            container1,
            AnimationPlayer {
                clip: clip_id,
                playing: true,
                wrap: Wrap::Loop,
                ..AnimationPlayer::default()
            },
        )
        .unwrap();
    scene.relink_hierarchy();

    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(node_translate_clip("NodeA", 4.0));
    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);

    // Each instance's own NodeA is driven; neither leaks into the other.
    assert!(scene.has_component::<PoseOverride>(nodes0[0]));
    assert!(scene.has_component::<PoseOverride>(node_a1));
    let a0 = scene.component::<PoseOverride>(nodes0[0]).unwrap();
    let a1 = scene.component::<PoseOverride>(node_a1).unwrap();
    assert!(a0.translation.distance(Vec3::new(2.0, 0.0, 0.0)) < 1e-4);
    assert!(a1.translation.distance(Vec3::new(2.0, 0.0, 0.0)) < 1e-4);
}
