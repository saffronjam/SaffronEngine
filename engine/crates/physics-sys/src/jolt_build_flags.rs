// The cross-platform-deterministic Jolt build flag set, as pure data, one variant per target CPU
// architecture.
//
// `include!`d into `build.rs`, which picks the variant from `CARGO_CFG_TARGET_ARCH` and feeds it
// to `cc`, and declared as a `mod` in the test build, which asserts the set.
//
// `JPH_CROSS_PLATFORM_DETERMINISTIC` is what makes the SSE and NEON code paths produce
// bit-identical results, so the contract is architecture-independent and holds in every variant:
//
//   - CROSS_PLATFORM_DETERMINISTIC ON  → `JPH_CROSS_PLATFORM_DETERMINISTIC`
//   - DOUBLE_PRECISION OFF             → single precision = the *absence* of `JPH_DOUBLE_PRECISION`
//   - `-ffp-model=precise -ffp-contract=off` is the determinism FP pairing (contracted FMAs
//     diverge across micro-architectures, so `JPH_USE_FMADD` / `-mfma` are never enabled).
//   - `-Wno-error` overrides Jolt's own `-Werror`.
//   - `-pthread` is dropped from compile and re-added at link only.
//
// The instruction-set selection is what differs per architecture:
//
//   - x86-64: the default x86 options (USE_AVX2/AVX/SSE4.x/LZCNT/TZCNT/F16C ON, AVX512 OFF) →
//     the `-m*` flags and matching `JPH_USE_*` defines, emitted in lockstep. The defines and the
//     `-m` flags MUST stay paired: a define without its flag (or vice versa) is a silent ABI
//     skew.
//   - aarch64: NEON is baseline, so Jolt's `Core.h` defines `JPH_USE_NEON` unconditionally from
//     `__aarch64__` — no arch define or `-m` flag is emitted here (defining `JPH_USE_NEON`
//     ourselves would redefine Jolt's, and the x86 `-m*` flags are invalid on ARM).
//
// The header must be `//`, not `//!`: an inner doc comment is illegal in an `include!`d file.

/// The frozen Saffron determinism flag set for vendored Jolt's translation units, for one target
/// CPU architecture.
pub(crate) struct JoltBuildFlags {
    /// Preprocessor defines applied to every Jolt TU *and* the shim TU (they change Jolt's
    /// struct layout, so they must reach all of Jolt and the shim identically).
    pub(crate) defines: &'static [(&'static str, Option<&'static str>)],
    /// Arch + floating-point flags confined to this crate's TUs. The FP pair
    /// (`-ffp-model=precise` + `-ffp-contract=off`) is the determinism contract, present on every
    /// architecture; the `-m*` arch flags are the x86 instruction-set selection, applied here
    /// only and empty on architectures where the SIMD path is baseline (aarch64/NEON).
    pub(crate) arch_fp_flags: &'static [&'static str],
    /// Jolt builds itself with `-Werror`, and clang flags the FP-model/FP-contract pairing under
    /// `-Woverriding-option`. Drop `-Werror` and silence that warning; the pairing is intended.
    pub(crate) warning_flags: &'static [&'static str],
    /// Native threads linked at link time — `-pthread`, dropped from the per-TU *compile*
    /// options (it only matters at link) and re-emitted as a link flag.
    pub(crate) link_threads: bool,
}

impl JoltBuildFlags {
    /// The x86-64 determinism flag set.
    pub(crate) const DETERMINISTIC_X86_64: Self = Self {
        defines: &[
            // Single precision is the absence of `JPH_DOUBLE_PRECISION`; never list it.
            ("JPH_CROSS_PLATFORM_DETERMINISTIC", None),
            ("JPH_OBJECT_STREAM", None),
            // Paired with the `-m*` flags below; AVX512 and FMADD are off by omission.
            ("JPH_USE_AVX2", None),
            ("JPH_USE_AVX", None),
            ("JPH_USE_SSE4_1", None),
            ("JPH_USE_SSE4_2", None),
            ("JPH_USE_LZCNT", None),
            ("JPH_USE_TZCNT", None),
            ("JPH_USE_F16C", None),
            // No asserts, profiler, or FP exceptions, so the archive is identical whether the
            // consuming Rust crate builds dev or release.
            ("NDEBUG", None),
        ],
        arch_fp_flags: &[
            "-ffp-model=precise",
            "-ffp-contract=off",
            // `-mfma` is omitted: contracted FMAs diverge across micro-architectures.
            "-mavx2",
            "-mbmi",
            "-mpopcnt",
            "-mlzcnt",
            "-mf16c",
            "-mfpmath=sse",
        ],
        warning_flags: &["-Wno-error", "-Wno-overriding-option"],
        link_threads: true,
    };

    /// The aarch64 (ARM NEON) determinism flag set. NEON is baseline on aarch64, so Jolt's
    /// `Core.h` enables `JPH_USE_NEON` on its own from `__aarch64__`; this variant therefore
    /// carries no arch SIMD define and no `-m*` flag (the FP-determinism pair is the whole of
    /// `arch_fp_flags`).
    pub(crate) const DETERMINISTIC_AARCH64: Self = Self {
        defines: &[
            ("JPH_CROSS_PLATFORM_DETERMINISTIC", None),
            ("JPH_OBJECT_STREAM", None),
            ("NDEBUG", None),
        ],
        arch_fp_flags: &["-ffp-model=precise", "-ffp-contract=off"],
        warning_flags: &["-Wno-error", "-Wno-overriding-option"],
        link_threads: true,
    };

    /// The flag set for a `CARGO_CFG_TARGET_ARCH` value. An unsupported architecture is a hard
    /// error, so a new target adds its variant deliberately rather than inheriting x86 `-m*` flags
    /// it cannot honor.
    pub(crate) fn for_arch(target_arch: &str) -> Self {
        match target_arch {
            "x86_64" => Self::DETERMINISTIC_X86_64,
            "aarch64" => Self::DETERMINISTIC_AARCH64,
            other => panic!(
                "no Jolt determinism flag set for target arch '{other}' — add a variant to \
                 jolt_build_flags.rs"
            ),
        }
    }
}
