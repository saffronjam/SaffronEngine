use super::*;

/// A class's region is its own: what it loses is decided by its own volume, and what
/// it keeps cannot be taken by a louder neighbour.
///
/// This is the property the whole partition exists for. Before it, every view in the
/// frame appended to one queue in atomic order, so a gather sweeping a hundred-metre
/// box could crowd out the camera — and a dropped camera request is a page the image
/// is made of arriving a frame late, repeatedly, with nothing saying so.
#[test]
fn a_flooded_class_loses_only_its_own_requests() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
    uploader.set_page_request_budget(4);

    append_page_requests(&uploader, crate::SceneViewClass::Camera, &[10, 11]);
    // The gather raises three times what its region holds.
    let flood: Vec<u32> = (100..112).collect();
    append_page_requests(&uploader, crate::SceneViewClass::Gi, &flood);
    append_page_requests(&uploader, crate::SceneViewClass::ShadowPage, &[50]);

    let drain = uploader.drain_page_requests(0);
    assert_eq!(
        drain.dropped, 8,
        "only the gather's overflow is lost (12 raised, 4 held)"
    );
    assert_eq!(
        drain.overflow_classes,
        crate::SceneViewClass::Gi.bit(),
        "and it is named as the gather's"
    );
    let camera: Vec<u32> = drain
        .requests
        .iter()
        .filter(|(_, class)| *class == crate::SceneViewClass::Camera)
        .map(|(slot, _)| *slot)
        .collect();
    assert_eq!(
        camera,
        vec![10, 11],
        "the camera keeps every request it made"
    );
    assert!(
        drain
            .requests
            .iter()
            .any(|(slot, class)| *slot == 50 && *class == crate::SceneViewClass::ShadowPage),
        "so does the shadow view"
    );

    // Draining resets every count word, so the next frame starts clean.
    assert_eq!(uploader.drain_page_requests(0), PageRequestDrain::default());
    device.wait_idle().expect("idle");
}

/// A page several classes missed is streamed once, under the most urgent of them.
/// Taking whichever arrived first would let a gather set the priority of a page the
/// camera is waiting on, and eviction would then price it as the gather's.
#[test]
fn a_page_two_classes_missed_arrives_once_at_the_higher_band() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
    append_page_requests(&uploader, crate::SceneViewClass::Gi, &[7]);
    append_page_requests(&uploader, crate::SceneViewClass::Camera, &[7]);

    let drain = uploader.drain_page_requests(0);
    assert_eq!(drain.requests, vec![(7, crate::SceneViewClass::Camera)]);
    assert_eq!(drain.dropped, 0);
    assert_eq!(drain.overflow_classes, 0);
    device.wait_idle().expect("idle");
}
