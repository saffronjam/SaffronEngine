use super::*;

fn spline_point(ticks: [i128; 3]) -> WorldPosition {
    WorldPosition::from_global_ticks(ticks).unwrap()
}

#[test]
fn spline_arclength_samples_a_joint_once() {
    let points = [
        spline_point([0, 0, 0]),
        spline_point([10, 0, 0]),
        spline_point([10, 0, 10]),
    ];
    let samples = sample_spline_segments(&spline_segments(&points).unwrap(), 5, 0).unwrap();
    assert_eq!(
        samples,
        vec![[0, 0, 0], [5, 0, 0], [10, 0, 0], [10, 0, 5], [10, 0, 10],]
    );
}

#[test]
fn spline_samples_and_ordinals_are_invariant_to_collinear_subdivision() {
    let direct = [spline_point([0, 0, 0]), spline_point([20, 0, 0])];
    let subdivided = [
        spline_point([0, 0, 0]),
        spline_point([7, 0, 0]),
        spline_point([13, 0, 0]),
        spline_point([20, 0, 0]),
    ];
    let direct = sample_spline_segments(&spline_segments(&direct).unwrap(), 4, 2).unwrap();
    let subdivided = sample_spline_segments(&spline_segments(&subdivided).unwrap(), 4, 2).unwrap();
    assert_eq!(direct, subdivided);
    assert_eq!(direct.len(), 6);
}

#[test]
fn spline_lateral_frame_tracks_direction_and_transports_through_turns() {
    let forward = [spline_point([0, 0, 0]), spline_point([10, 0, 0])];
    let reverse = [spline_point([10, 0, 0]), spline_point([0, 0, 0])];
    assert_eq!(
        sample_spline_segments(&spline_segments(&forward).unwrap(), 10, 2).unwrap(),
        vec![[0, 0, -2], [10, 0, -2]]
    );
    assert_eq!(
        sample_spline_segments(&spline_segments(&reverse).unwrap(), 10, 2).unwrap(),
        vec![[10, 0, 2], [0, 0, 2]]
    );

    let turn = [
        spline_point([0, 0, 0]),
        spline_point([10, 0, 0]),
        spline_point([10, 0, 10]),
    ];
    assert_eq!(
        sample_spline_segments(&spline_segments(&turn).unwrap(), 10, 2).unwrap(),
        vec![[0, 0, -2], [12, 0, 0], [12, 0, 10]]
    );
}
