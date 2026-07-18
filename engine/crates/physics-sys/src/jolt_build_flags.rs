// The cross-platform-deterministic Jolt build flag set, as pure data, one variant per target CPU
// architecture.
//
// This is the single source of truth for the flags, `include!`d into `build.rs` (where
// [`JoltBuildFlags::for_arch`] picks the variant from `CARGO_CFG_TARGET_ARCH` and feeds it to
// `cc`) and declared as a `mod` in the crate's test build (where the flag set is asserted).
// Sharing the data this way keeps the determinism contract testable under `cargo test` without
// coupling the test to `cc` or to compiling Jolt.
//
// The determinism contract is architecture-independent — `JPH_CROSS_PLATFORM_DETERMINISTIC` is
// what makes the SSE and NEON code paths produce bit-identical results — and holds in every
// variant:
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
// A plain `//` header (not `//!`) is deliberate: an inner doc comment is illegal when this file
// is `include!`d mid-`build.rs` rather than parsed as a module root.

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
    /// Jolt builds itself with `-Werror`; clang 21 flags the FP-model/FP-contract pairing
    /// under `-Woverriding-option`, failing Jolt's own build. Drop `-Werror` and silence the
    /// expected `-Woverriding-option` (the pairing is exactly what we want).
    pub(crate) warning_flags: &'static [&'static str],
    /// Native threads linked at link time — `-pthread`, dropped from the per-TU *compile*
    /// options (it only matters at link) and re-emitted as a link flag.
    pub(crate) link_threads: bool,
}

impl JoltBuildFlags {
    /// The x86-64 determinism flag set. `const` so it is a single immutable definition with no
    /// runtime construction.
    pub(crate) const DETERMINISTIC_X86_64: Self = Self {
        defines: &[
            // The master determinism switch and the single-precision contract (the latter by
            // omission — `JPH_DOUBLE_PRECISION` is deliberately never listed).
            ("JPH_CROSS_PLATFORM_DETERMINISTIC", None),
            // ObjectStream + RTTI attributes are ON in Jolt's defaults, and the engine builds
            // the full library, so the shim must agree.
            ("JPH_OBJECT_STREAM", None),
            // x86 instruction-set defines, paired with the `-m*` flags below. AVX512 OFF and
            // FMADD suppressed-by-determinism are the *absence* of `JPH_USE_AVX512`/`JPH_USE_FMADD`.
            ("JPH_USE_AVX2", None),
            ("JPH_USE_AVX", None),
            ("JPH_USE_SSE4_1", None),
            ("JPH_USE_SSE4_2", None),
            ("JPH_USE_LZCNT", None),
            ("JPH_USE_TZCNT", None),
            ("JPH_USE_F16C", None),
            // Distribution-style config: no asserts, profiler, or FP exceptions. This keeps the
            // `-sys` archive identical whether the consuming Rust crate is built dev or release,
            // and is the standard Jolt shipping ABI.
            ("NDEBUG", None),
        ],
        arch_fp_flags: &[
            "-ffp-model=precise",
            "-ffp-contract=off",
            // The x86 instruction-set flags Jolt emits for USE_AVX2 ON + determinism; FMADD's
            // `-mfma` is omitted under determinism.
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

    /// The determinism flag set for the given `CARGO_CFG_TARGET_ARCH` value. Every supported
    /// architecture carries the same determinism contract; only the instruction-set selection
    /// differs. An unsupported architecture is a hard error — a new target must add its variant
    /// deliberately rather than silently inherit x86 `-m*` flags it cannot honor.
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
