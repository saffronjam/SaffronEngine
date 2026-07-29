//! The validation-clean gate's regression probe: a planted Vulkan validation error must be
//! *caught*, proving the detector is wired (the right messenger prefix, the validation layer
//! actually enabled) rather than silently disabled.
//!
//! A gate that can never go red is worthless. Every other render-touching e2e asserts
//! `validation_errors()` is empty; this one boots the host with `SAFFRON_VK_PLANT_VALIDATION_ERROR`
//! set — which records one out-of-spec `vkCmdSetViewport` into each scene frame — and asserts the
//! harness *sees* the resulting `ERROR  vulkan  [validation] …` lines. If this test ever
//! goes green-with-empty-errors, the gate has been silently disabled and the suite has lost its
//! only headless detector for GPU-state bugs.

use std::time::{Duration, Instant};

use saffron_e2e::TestEngine;

/// How long to wait for the planted error to reach the captured log. The control socket opens
/// before the first frame records, and how long that first frame takes is a property of the
/// device — a software rasterizer under a headless compositor is far slower than a discrete
/// GPU — so the gate polls to a generous deadline rather than sleeping a fixed span.
const DETECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Booting with the plant env set surfaces the planted validation error through the harness's
/// `validation_errors()` filter — the detector is live (the inverse of every other test's
/// empty-errors assertion).
#[test]
fn planted_validation_error_is_detected() {
    let mut engine =
        TestEngine::boot(&[("SAFFRON_VK_PLANT_VALIDATION_ERROR", "1")]).expect("boot engine");

    // Poll until the planted out-of-spec command has been recorded, submitted, and its
    // validation message has reached the captured log.
    let deadline = Instant::now() + DETECT_TIMEOUT;
    let mut errors = engine.validation_errors();
    while errors.is_empty() && Instant::now() < deadline {
        engine.settle(Duration::from_millis(100));
        errors = engine.validation_errors();
    }
    assert!(
        !errors.is_empty(),
        "the planted validation error was NOT detected — the gate is silently disabled \
         (wrong messenger prefix, or the validation layer is not enabled). \
         A green here means every other test's `validation_errors() == []` proves nothing.\n\
         captured host log:\n{}",
        engine.log()
    );
    // The planted error is the zero-width viewport VUID, not some unrelated incidental issue.
    assert!(
        errors
            .iter()
            .any(|line| line.contains("VUID-VkViewport-width-01770")),
        "expected the planted zero-width-viewport VUID, saw:\n{}",
        errors.join("\n")
    );

    engine.shutdown();
}
