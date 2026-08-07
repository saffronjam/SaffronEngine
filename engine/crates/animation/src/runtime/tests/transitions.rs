use super::*;

/// Runs the transition oracle for one mode: a single-bone rig switched in from the
/// rest (identity) outgoing pose to a clip holding 90° about Y, over a 1s transition.
/// Ticks the switch frame (`x=0`), then runs the transition out, returning the switch
/// rotation and the steady-state incoming rotation.
fn run_transition(mode: Transition) -> (Quat, Quat) {
    let s = 0.5_f32.sqrt();
    let clip_id = Uuid(9001);
    let mut scene = Scene::new();
    let bone = scene.create_entity("J0");
    scene.add_component(bone, Bone::default()).unwrap();
    let rig = scene.create_entity("Rig");
    let bone_id = scene.component::<IdComponent>(bone).unwrap().id;
    scene
        .add_component(
            rig,
            SkinnedMesh {
                bones: vec![bone_id],
                ..SkinnedMesh::default()
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            AnimationPlayer {
                clip: clip_id,
                preview_in_edit: true,
                playing: true,
                transition_mode: mode,
                // A distinct outgoing clip id; its pose comes from the bone's rest.
                prev_clip: Uuid(1),
                transition: 0.0,
                transition_duration: 1.0,
                ..AnimationPlayer::default()
            },
        )
        .unwrap();
    scene.relink_hierarchy();

    // A clip holding 90° about Y at every key (xyzw constant).
    let clip = AnimClip {
        name: "spin90".to_string(),
        duration: 1.0,
        tracks: vec![AnimTrack {
            index: 0,
            target_name: "J0".to_string(),
            path: AnimPath::Rotation,
            interp: AnimInterp::Linear,
            times: vec![0.0, 1.0],
            values: vec![0.0, s, 0.0, s, 0.0, s, 0.0, s],
            ..Default::default()
        }],
    };
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(clip);

    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load); // switch frame, x=0
    let switch = scene.component::<PoseOverride>(bone).unwrap().rotation;
    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load); // x=0.5 -> 1, ends
    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load); // steady incoming
    let end = scene.component::<PoseOverride>(bone).unwrap().rotation;
    (switch, end)
}

#[test]
fn crossfade_starts_outgoing_ends_incoming() {
    // The cross-fade begins at the outgoing (rest identity) pose at the switch frame and
    // settles on the incoming 90° clip once the transition runs out.
    let q90 = Quat::from_axis_angle(Vec3::Y, 90.0_f32.to_radians());
    let (switch, end) = run_transition(Transition::CrossFade);
    assert!(
        quat_close(switch, Quat::IDENTITY),
        "crossfade starts at the outgoing pose"
    );
    assert!(quat_close(end, q90), "crossfade ends at the incoming clip");
}

#[test]
fn inertialize_c0_at_switch() {
    // Inertialization is C0 at the switch (no pop): it starts at the outgoing pose and
    // decays the offset to the incoming 90° clip.
    let q90 = Quat::from_axis_angle(Vec3::Y, 90.0_f32.to_radians());
    let (switch, end) = run_transition(Transition::Inertialize);
    assert!(
        quat_close(switch, Quat::IDENTITY),
        "inertialization is C0 at the switch (no pop)"
    );
    assert!(
        quat_close(end, q90),
        "inertialization ends at the incoming clip"
    );
}

#[test]
fn loop_wrap_holds_pre_wrap_pose() {
    // A clip ramping 0°→90° about Y would pop at the loop seam; loop_blend > 0
    // inertializes across it, so the wrap frame holds the pre-wrap (end) pose rather
    // than snapping to the start. A hard cut would jump ~72°, which quat_close rejects.
    let s = 0.5_f32.sqrt();
    let clip_id = Uuid(9002);
    let mut scene = Scene::new();
    let bone = scene.create_entity("J0");
    scene.add_component(bone, Bone::default()).unwrap();
    let rig = scene.create_entity("Rig");
    let bone_id = scene.component::<IdComponent>(bone).unwrap().id;
    scene
        .add_component(
            rig,
            SkinnedMesh {
                bones: vec![bone_id],
                ..SkinnedMesh::default()
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            AnimationPlayer {
                clip: clip_id,
                preview_in_edit: true,
                playing: true,
                wrap: Wrap::Loop,
                loop_blend: 0.5,
                time: 0.8,
                ..AnimationPlayer::default()
            },
        )
        .unwrap();
    scene.relink_hierarchy();

    let clip = AnimClip {
        name: "ramp".to_string(),
        duration: 1.0,
        tracks: vec![AnimTrack {
            index: 0,
            target_name: "J0".to_string(),
            path: AnimPath::Rotation,
            interp: AnimInterp::Linear,
            times: vec![0.0, 1.0],
            // identity -> 90° about Y.
            values: vec![0.0, 0.0, 0.0, 1.0, 0.0, s, 0.0, s],
            ..Default::default()
        }],
    };
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(clip);

    tick_animation(&mut runtime, &mut scene, 0.1, AnimMode::Edit, &mut load); // time -> 0.9
    let pre_wrap = scene.component::<PoseOverride>(bone).unwrap().rotation;
    tick_animation(&mut runtime, &mut scene, 0.2, AnimMode::Edit, &mut load); // wraps past the end
    let wrap_frame = scene.component::<PoseOverride>(bone).unwrap().rotation;
    assert!(
        quat_close(wrap_frame, pre_wrap),
        "loop wrap holds the pre-wrap pose (no pop)"
    );
}

#[test]
fn skinning_seam_palette_reflects_animation() {
    // The cross-area contract animation → rendering: a ticked rig writes a PoseOverride
    // that flows through update_world_transforms + joint_matrices into the joint palette
    // the renderer consumes — so the palette must reflect the animated pose, not the
    // rest pose. No GPU here: the prepass that blends this palette lives in 06-rendering.
    let clip_id = Uuid(4242);
    let (mut scene, rig, _bones) = rig_scene(2, clip_id);
    scene
        .with_component_mut::<AnimationPlayer, _>(rig, |p| p.time = 0.0)
        .unwrap();

    // Capture the rest-pose palette (no override yet) for the baseline comparison.
    let skin = scene
        .with_component::<SkinnedMesh, _>(rig, Clone::clone)
        .unwrap();
    scene.update_world_transforms();
    let rest_palette = scene.joint_matrices(&skin);

    // Tick the rig with a clip that moves joint 0 far down +X, then recompose world
    // matrices and rebuild the palette.
    let mut runtime = AnimationRuntime::new();
    let mut load = clip_loader(translate_clip(0, 10.0));
    tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);
    scene.update_world_transforms();
    let animated_palette = scene.joint_matrices(&skin);

    // The animated palette must differ from the rest palette by more than 1e-3,
    // confirming the override flowed into world composition and the palette.
    let drift = (animated_palette[0] - rest_palette[0])
        .to_cols_array()
        .iter()
        .fold(0.0_f32, |acc, c| acc.max(c.abs()));
    assert!(
        drift > 1e-3,
        "the joint palette must reflect the animated pose, got drift {drift}"
    );
}
