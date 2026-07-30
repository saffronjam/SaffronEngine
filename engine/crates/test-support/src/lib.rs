//! Shared test comparators and the golden-byte diff helper. The float tolerances that *are* the
//! contract live here rather than being re-defined per test.
//!
//! [`assert_bytes_match_golden`] diffs `actual` against a committed fixture under the repo-root
//! `fixtures/golden/` and panics with a first-differing-offset hexdump. It catches the silent
//! byte-drift class — `.smesh`/`.smat` shifts, std430 offset moves, shm header changes — that never
//! throws and never fails validation. `UPDATE_GOLDEN=1` reseeds the fixture instead of asserting,
//! for an intentional format change that updates the writer and the fixture together.

#![deny(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use glam::Quat;

/// The general float-equality tolerance, the default for "these two values are the same
/// value".
pub const EPS: f32 = 1e-4;

/// Two-bone IK end-effector reach tolerance: a solved chain lands its end on an in-range
/// target this close.
pub const IK_REACH_EPS: f32 = 1e-3;

/// Two-bone IK over-reach clamp tolerance: an over-extended chain straightens toward the
/// target and lands at max reach this close. Looser than [`IK_REACH_EPS`] because the
/// clamped solve only approximately straightens.
pub const IK_OVER_REACH_EPS: f32 = 1e-2;

/// Returns a process-unique POSIX shared-memory name within macOS's 31-byte name limit.
#[must_use]
pub fn unique_shm_name() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("/sat-{}-{sequence}", std::process::id())
}

/// Whether `a` and `b` are within `eps` of each other.
///
/// The predicate form for use inside a larger boolean; prefer [`assert_close`] in a test so
/// the failure message reports both values.
#[must_use]
pub fn close(a: f32, b: f32, eps: f32) -> bool {
    (a - b).abs() <= eps
}

/// Asserts `a` and `b` are within `eps`, reporting both values and the delta on failure.
///
/// Pass [`EPS`] for the general case, [`IK_REACH_EPS`] / [`IK_OVER_REACH_EPS`] for IK.
#[track_caller]
pub fn assert_close(a: f32, b: f32, eps: f32) {
    assert!(
        close(a, b, eps),
        "values differ by {} (> {eps}): {a} vs {b}",
        (a - b).abs()
    );
}

/// Whether `a` and `b` are the same orientation under the quaternion double cover.
///
/// `q` and `-q` rotate identically, so equality is `|dot(a, b)| > 1 - 1e-4` rather than a
/// component compare. The `1e-4` margin is [`EPS`].
#[must_use]
pub fn quat_close(a: Quat, b: Quat) -> bool {
    a.dot(b).abs() > 1.0 - EPS
}

/// Asserts `a` and `b` are the same orientation under the double cover, reporting both
/// quaternions and their dot on failure.
#[track_caller]
pub fn assert_quat_close(a: Quat, b: Quat) {
    assert!(
        quat_close(a, b),
        "quaternions differ: dot={} (need |dot| > {}): {a:?} vs {b:?}",
        a.dot(b),
        1.0 - EPS
    );
}

/// A byte-level diff of `actual` against a golden `expected`.
///
/// Returns `Ok(())` when the two byte slices are identical, else `Err` with the first
/// differing offset and a windowed hexdump around it — the failure mode an implementer can
/// act on (which byte drifted, in context), not a wall of bytes. Lengths that differ are
/// reported with the shorter side's tail. This is the comparison core
/// [`assert_bytes_match_golden`] wraps with on-disk fixture loading and the `UPDATE_GOLDEN`
/// reseed path.
pub fn golden(actual: &[u8], expected: &[u8]) -> Result<(), String> {
    if actual == expected {
        return Ok(());
    }
    let first = actual
        .iter()
        .zip(expected)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| actual.len().min(expected.len()));
    let mut message = format!(
        "golden mismatch at offset {first}: actual {} bytes, expected {} bytes\n",
        actual.len(),
        expected.len()
    );
    message.push_str(&hexdump_window("actual  ", actual, first));
    message.push_str(&hexdump_window("expected", expected, first));
    Err(message)
}

/// Asserts `actual` is byte-identical to the golden `expected`, panicking with the
/// first-differing-offset hexdump from [`golden`] on mismatch.
#[track_caller]
pub fn assert_golden(actual: &[u8], expected: &[u8]) {
    if let Err(message) = golden(actual, expected) {
        panic!("{message}");
    }
}

/// The repo-root `fixtures/golden/` directory holding the committed byte-exact fixtures.
///
/// Resolved from this crate's `CARGO_MANIFEST_DIR` (`engine/crates/test-support`) up to the
/// repo root, so it is stable regardless of the consuming crate or the test's working
/// directory.
#[must_use]
pub fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/golden")
        .canonicalize()
        .unwrap_or_else(|e| panic!("golden fixtures dir does not exist: {e}"))
}

/// Asserts `actual` matches the committed golden fixture named `name` under
/// `fixtures/golden/`.
///
/// On a match this is silent. On a mismatch it panics with [`golden`]'s
/// first-differing-offset windowed hexdump — the byte an implementer must fix and its
/// neighborhood. A missing fixture (when not reseeding) panics with the resolved path so
/// the cause is obvious.
///
/// When the `UPDATE_GOLDEN` env var is set (to any non-empty value), `actual` is *written*
/// to the fixture instead of asserted — the seed/reseed path, for seeding a fixture the
/// first time or landing an *intentional* format change (the one writer and the one fixture
/// move together). It is never the way to quiet a real drift: a drift is a finding, not a
/// reseed.
#[track_caller]
pub fn assert_bytes_match_golden(name: &str, actual: &[u8]) {
    let path = golden_path(name);
    if update_golden_enabled() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("cannot create golden dir '{}': {e}", parent.display()));
        }
        std::fs::write(&path, actual)
            .unwrap_or_else(|e| panic!("cannot (re)seed golden '{}': {e}", path.display()));
        return;
    }
    let expected = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "golden fixture '{}' is missing ({e}); seed it with UPDATE_GOLDEN=1 from the Rust \
             host, then commit it",
            path.display()
        )
    });
    if let Err(message) = golden(actual, &expected) {
        panic!("golden '{}' drifted:\n{message}", path.display());
    }
}

/// Resolves a fixture `name` (e.g. `"cube.smesh"`) under [`golden_dir`].
///
/// In `UPDATE_GOLDEN` mode the dir is resolved without `canonicalize` (the fixture may not
/// exist yet on a first seed), joined directly from the manifest dir.
fn golden_path(name: &str) -> PathBuf {
    if update_golden_enabled() {
        return Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/golden")
            .join(name);
    }
    golden_dir().join(name)
}

/// Whether `UPDATE_GOLDEN` is set to a non-empty value (the reseed switch).
fn update_golden_enabled() -> bool {
    std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| !v.is_empty())
}

/// 16 bytes per row, centered on `at`, with `±2` rows of context — enough to see the
/// differing byte and its neighbors without dumping the whole buffer.
fn hexdump_window(label: &str, bytes: &[u8], at: usize) -> String {
    const ROW: usize = 16;
    const CONTEXT_ROWS: usize = 2;
    let center_row = at / ROW;
    let start = center_row.saturating_sub(CONTEXT_ROWS) * ROW;
    let end = ((center_row + CONTEXT_ROWS + 1) * ROW).min(bytes.len());
    let mut out = format!("{label}:\n");
    let mut offset = start;
    while offset < end {
        let row_end = (offset + ROW).min(end);
        let hex: Vec<String> = bytes[offset..row_end]
            .iter()
            .enumerate()
            .map(|(i, byte)| {
                let mark = if offset + i == at { '>' } else { ' ' };
                format!("{mark}{byte:02x}")
            })
            .collect();
        out.push_str(&format!("  {offset:08x}  {}\n", hex.join(" ")));
        offset = row_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use glam::Quat;

    use super::*;

    #[test]
    fn close_respects_the_epsilon() {
        assert!(close(1.0, 1.0 + 5e-5, EPS));
        assert!(!close(1.0, 1.0 + 2e-4, EPS));
        // The IK tolerances are progressively looser than the general one.
        const {
            assert!(IK_REACH_EPS > EPS);
            assert!(IK_OVER_REACH_EPS > IK_REACH_EPS);
        }
    }

    #[test]
    fn shared_memory_names_are_unique_and_portable() {
        let first = unique_shm_name();
        let second = unique_shm_name();
        assert_ne!(first, second);
        assert!(first.starts_with('/'));
        assert!(first.len() <= 31);
        assert!(second.len() <= 31);
    }

    #[test]
    #[should_panic(expected = "values differ")]
    fn assert_close_panics_past_epsilon() {
        assert_close(0.0, 1.0, EPS);
    }

    #[test]
    fn quat_close_treats_q_and_negative_q_as_equal() {
        let q = Quat::from_rotation_y(0.7);
        // The double cover: -q is the same orientation.
        assert!(quat_close(q, -q));
        assert!(quat_close(q, q));
        // A genuinely different orientation is rejected.
        assert!(!quat_close(q, Quat::from_rotation_y(1.4)));
    }

    #[test]
    #[should_panic(expected = "quaternions differ")]
    fn assert_quat_close_panics_on_distinct_orientations() {
        assert_quat_close(Quat::IDENTITY, Quat::from_rotation_x(1.0));
    }

    #[test]
    fn golden_accepts_identical_bytes() {
        assert!(golden(b"abc", b"abc").is_ok());
        assert!(golden(&[], &[]).is_ok());
    }

    #[test]
    fn golden_reports_the_first_differing_offset() {
        let actual = [0u8, 1, 2, 0xAA, 4, 5];
        let expected = [0u8, 1, 2, 0xBB, 4, 5];
        let message = golden(&actual, &expected).expect_err("differing bytes must fail");
        assert!(message.contains("offset 3"), "message was: {message}");
        // The hexdump marks the differing byte from each side.
        assert!(message.contains(">aa"), "message was: {message}");
        assert!(message.contains(">bb"), "message was: {message}");
    }

    #[test]
    fn golden_reports_a_length_mismatch() {
        let message = golden(b"abcd", b"abc").expect_err("differing lengths must fail");
        assert!(message.contains("offset 3"), "message was: {message}");
        assert!(
            message.contains("actual 4 bytes, expected 3 bytes"),
            "message was: {message}"
        );
    }

    #[test]
    #[should_panic(expected = "golden mismatch")]
    fn assert_golden_panics_on_drift() {
        assert_golden(b"\x00\x01", b"\x00\x02");
    }

    #[test]
    fn golden_dir_resolves_to_the_committed_fixtures() {
        let dir = golden_dir();
        assert!(dir.is_dir(), "golden dir must exist: {}", dir.display());
        // The committed fixtures are present.
        for name in [
            "cube.smesh",
            "cube.sanim",
            "material.smat",
            "shm_header.layout",
        ] {
            assert!(
                dir.join(name).is_file(),
                "missing committed fixture: {name}"
            );
        }
    }

    #[test]
    fn assert_bytes_match_golden_accepts_the_committed_bytes() {
        // Loading a committed fixture and matching its own bytes is the happy path; this
        // also proves the path resolution + read are wired (separately from the per-format
        // tests, which run in the owning crates).
        let bytes = std::fs::read(golden_dir().join("cube.smesh")).expect("read cube.smesh");
        assert_bytes_match_golden("cube.smesh", &bytes);
    }

    #[test]
    #[should_panic(expected = "drifted")]
    fn assert_bytes_match_golden_panics_on_a_drifted_byte() {
        let mut bytes = std::fs::read(golden_dir().join("cube.smesh")).expect("read cube.smesh");
        // Flip one byte deep in the payload: a silent corruption the byte compare catches.
        bytes[200] ^= 0xFF;
        assert_bytes_match_golden("cube.smesh", &bytes);
    }

    #[test]
    #[should_panic(expected = "is missing")]
    fn assert_bytes_match_golden_panics_on_a_missing_fixture() {
        assert_bytes_match_golden("does-not-exist.golden", b"whatever");
    }
}
