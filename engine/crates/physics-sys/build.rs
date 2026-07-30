//! Determinism build driver for Jolt — fetches Jolt 5.3.0 on demand, then compiles it + the
//! `cxx` shim.
//!
//! Jolt's arch/FP flags live in this `build.rs` and reach nothing else in the workspace — so the
//! rest of the tree is never silently recompiled with `-mavx2` / `-ffp-model=precise`, which
//! would change its float results.
//!
//! The flag set is `JoltBuildFlags::DETERMINISTIC`. Two flag-confined `cc` invocations link into
//! this crate: one compiles every `.cpp` under the fetched `Jolt/` tree (the static
//! `saffron_jolt` archive), the other — produced by `cxx_build::bridge` so it carries the
//! generated bridge glue — compiles the shim TU (`shim/jolt_bridge.cpp`). Both see the identical
//! `JPH_*` defines because those defines change Jolt's struct layout; a skew between the shim and
//! Jolt is silent memory corruption. A `cargo::rustc-cfg` marker lets a `#[test]` confirm the
//! flags were active.
//!
//! The Jolt source is **not** stored in this repo. It is fetched on demand from the pinned
//! official release tarball, checksum-verified, and extracted into the gitignored
//! `vendor/JoltPhysics-5.3.0/` cache (`fetch::ensure_vendored_jolt`). The pin is exact: a fixed
//! tag + a fixed SHA-256 over the release archive, so the build is reproducible byte-for-byte and
//! a Jolt bump is a deliberate, reviewed replay-format migration — never a silent dependency
//! update. The cache survives `cargo clean`; only `just clean-deps` removes it.

use std::path::{Path, PathBuf};

// `src/lib.rs` declares the same file as a `mod`, so the tests assert the flag set without
// depending on `cc`.
include!("src/jolt_build_flags.rs");

/// Downloads Jolt's pinned release tarball, verifies it against the embedded SHA-256, and extracts
/// it into the gitignored vendor cache. `curl` and `tar` do the transport and unpack; the checksum
/// is computed in pure std so verification is identical on every host.
mod fetch {
    use std::path::Path;
    use std::process::Command;

    /// The pinned Jolt release. A bump here is a replay-format migration: change the tag, the
    /// commit, *and* re-derive [`ARCHIVE_SHA256`] from the new tarball in lockstep.
    pub const VERSION: &str = "5.3.0";
    /// The upstream git commit `v5.3.0` resolves to — the pin record, kept for provenance.
    pub const COMMIT: &str = "0373ec0dd762e4bc2f6acdb08371ee84fa23c6db";

    /// The official source tarball for the pinned tag.
    pub const ARCHIVE_URL: &str =
        "https://github.com/jrouwe/JoltPhysics/archive/refs/tags/v5.3.0.tar.gz";

    /// SHA-256 of the tarball at [`ARCHIVE_URL`]. The fetch fails closed on a mismatch, so a
    /// tampered or truncated download never reaches the compiler. Re-derive with `sha256sum` when
    /// bumping [`VERSION`].
    pub const ARCHIVE_SHA256: &str =
        "e7f9621e480646c434150e1fbe3a9410f4ec4b04ffe54791e0678326b741b918";

    /// The directory the tarball extracts into. GitHub's tag tarballs unpack to `<repo>-<tag>/`,
    /// which is exactly the vendor cache leaf, so the tree lands where `build.rs` compiles it.
    pub const ARCHIVE_TOP_DIR: &str = "JoltPhysics-5.3.0";

    /// Fetches, verifies, and extracts Jolt into `vendor_dir` unless `jolt_root` is already
    /// populated. The cache outlives `cargo clean`; only `just clean-deps` removes it.
    pub fn ensure_vendored_jolt(vendor_dir: &Path, jolt_root: &Path) -> Result<(), String> {
        // `jolt_root/Jolt` is the subtree the compile walks. A partially-extracted tree is
        // repaired by `just clean-deps` and a rebuild.
        if jolt_root.join("Jolt").is_dir() {
            return Ok(());
        }

        eprintln!(
            "saffron-physics-sys: Jolt {VERSION} (commit {COMMIT}) source absent — fetching the \
             pinned release (cache: {})",
            vendor_dir.display()
        );

        std::fs::create_dir_all(vendor_dir)
            .map_err(|e| format!("creating vendor cache dir {}: {e}", vendor_dir.display()))?;

        let archive = vendor_dir.join("jolt-source.tar.gz");
        download(ARCHIVE_URL, &archive)?;
        verify_sha256(&archive, ARCHIVE_SHA256)?;
        extract(&archive, vendor_dir)?;

        // Confirm the compile target materialized rather than trusting `tar`'s exit code.
        if !jolt_root.join("Jolt").is_dir() {
            return Err(format!(
                "extracted {ARCHIVE_TOP_DIR} but {} is missing — the release layout changed",
                jolt_root.join("Jolt").display()
            ));
        }

        let _ = std::fs::remove_file(&archive);
        Ok(())
    }

    /// Downloads `url` to `dest` via `curl`, surfacing the cold-start hint on any failure.
    fn download(url: &str, dest: &Path) -> Result<(), String> {
        let status = Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--output",
            ])
            .arg(dest)
            .arg(url)
            .status()
            .map_err(|e| {
                format!(
                    "could not run `curl` to fetch Jolt {VERSION} ({e}). Install curl, or fetch \
                     the source up front with `just fetch-deps` on a machine with network access."
                )
            })?;

        if !status.success() {
            let _ = std::fs::remove_file(dest);
            return Err(format!(
                "failed to download Jolt {VERSION} from {url} (curl exit {}). The network may be \
                 unavailable; fetch the source up front with `just fetch-deps`, or build on a \
                 host with network access.",
                status.code().unwrap_or(-1)
            ));
        }
        Ok(())
    }

    /// Extract `archive` into `into` via `tar`.
    fn extract(archive: &Path, into: &Path) -> Result<(), String> {
        let status = Command::new("tar")
            .arg("--extract")
            .arg("--gzip")
            .arg("--file")
            .arg(archive)
            .arg("--directory")
            .arg(into)
            .status()
            .map_err(|e| format!("could not run `tar` to unpack Jolt {VERSION} ({e})"))?;
        if !status.success() {
            return Err(format!(
                "failed to extract {} (tar exit {})",
                archive.display(),
                status.code().unwrap_or(-1)
            ));
        }
        Ok(())
    }

    /// Verifies `path`'s SHA-256 equals `expected` (lowercase hex).
    fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("reading downloaded archive {}: {e}", path.display()))?;
        let actual = sha256_hex(&bytes);
        if actual != expected {
            let _ = std::fs::remove_file(path);
            return Err(format!(
                "Jolt {VERSION} archive checksum mismatch — refusing to build.\n  expected: \
                 {expected}\n  actual:   {actual}\nThe download was corrupted or tampered with; \
                 retry, or if you are bumping the pinned version re-derive ARCHIVE_SHA256."
            ));
        }
        Ok(())
    }

    /// Lowercase-hex SHA-256 of `data`, a self-contained FIPS 180-4 implementation.
    fn sha256_hex(data: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a_2f98,
            0x7137_4491,
            0xb5c0_fbcf,
            0xe9b5_dba5,
            0x3956_c25b,
            0x59f1_11f1,
            0x923f_82a4,
            0xab1c_5ed5,
            0xd807_aa98,
            0x1283_5b01,
            0x2431_85be,
            0x550c_7dc3,
            0x72be_5d74,
            0x80de_b1fe,
            0x9bdc_06a7,
            0xc19b_f174,
            0xe49b_69c1,
            0xefbe_4786,
            0x0fc1_9dc6,
            0x240c_a1cc,
            0x2de9_2c6f,
            0x4a74_84aa,
            0x5cb0_a9dc,
            0x76f9_88da,
            0x983e_5152,
            0xa831_c66d,
            0xb003_27c8,
            0xbf59_7fc7,
            0xc6e0_0bf3,
            0xd5a7_9147,
            0x06ca_6351,
            0x1429_2967,
            0x27b7_0a85,
            0x2e1b_2138,
            0x4d2c_6dfc,
            0x5338_0d13,
            0x650a_7354,
            0x766a_0abb,
            0x81c2_c92e,
            0x9272_2c85,
            0xa2bf_e8a1,
            0xa81a_664b,
            0xc24b_8b70,
            0xc76c_51a3,
            0xd192_e819,
            0xd699_0624,
            0xf40e_3585,
            0x106a_a070,
            0x19a4_c116,
            0x1e37_6c08,
            0x2748_774c,
            0x34b0_bcb5,
            0x391c_0cb3,
            0x4ed8_aa4a,
            0x5b9c_ca4f,
            0x682e_6ff3,
            0x748f_82ee,
            0x78a5_636f,
            0x84c8_7814,
            0x8cc7_0208,
            0x90be_fffa,
            0xa450_6ceb,
            0xbef9_a3f7,
            0xc671_78f2,
        ];
        let mut state: [u32; 8] = [
            0x6a09_e667,
            0xbb67_ae85,
            0x3c6e_f372,
            0xa54f_f53a,
            0x510e_527f,
            0x9b05_688c,
            0x1f83_d9ab,
            0x5be0_cd19,
        ];

        let bit_len = (data.len() as u64).wrapping_mul(8);
        let mut padded = data.to_vec();
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&bit_len.to_be_bytes());

        for block in padded.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (i, chunk) in block.chunks_exact(4).enumerate() {
                w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }

            let mut h = state;
            for (k, wi) in K.iter().zip(w.iter()) {
                let s1 = h[4].rotate_right(6) ^ h[4].rotate_right(11) ^ h[4].rotate_right(25);
                let ch = (h[4] & h[5]) ^ (!h[4] & h[6]);
                let t1 = h[7]
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(*k)
                    .wrapping_add(*wi);
                let s0 = h[0].rotate_right(2) ^ h[0].rotate_right(13) ^ h[0].rotate_right(22);
                let maj = (h[0] & h[1]) ^ (h[0] & h[2]) ^ (h[1] & h[2]);
                let t2 = s0.wrapping_add(maj);
                h[7] = h[6];
                h[6] = h[5];
                h[5] = h[4];
                h[4] = h[3].wrapping_add(t1);
                h[3] = h[2];
                h[2] = h[1];
                h[1] = h[0];
                h[0] = t1.wrapping_add(t2);
            }
            for (i, hv) in h.iter().enumerate() {
                state[i] = state[i].wrapping_add(*hv);
            }
        }

        let mut hex = String::with_capacity(64);
        for word in &state {
            for byte in word.to_be_bytes() {
                hex.push_str(&format!("{byte:02x}"));
            }
        }
        hex
    }
}

/// Apply the confined determinism flag set to a `cc::Build` (Jolt TUs or the shim TU).
fn apply_flags(flags: &JoltBuildFlags, build: &mut cc::Build) {
    // The determinism flags (`-ffp-model=precise`, `-stdlib=libc++`) are clang-specific; `cc`'s
    // default `c++` resolves to GCC in the toolbox, which rejects them. Default to clang while
    // honoring an explicit `CXX` override so a different toolchain can still be pointed at this
    // build.
    if std::env::var_os("CXX").is_none() {
        build.compiler("clang++");
    }
    // Jolt and `cxx`'s generated glue both require C++17; libc++ is the toolbox stdlib.
    build.cpp(true).std("c++17").cpp_set_stdlib("c++");
    for (key, value) in flags.defines {
        build.define(key, *value);
    }
    for flag in flags.arch_fp_flags {
        build.flag(flag);
    }
    for flag in flags.warning_flags {
        build.flag_if_supported(flag);
    }
}

/// Emit the link directives so the static Jolt archive links cleanly.
fn emit_link_directives(flags: &JoltBuildFlags) {
    if flags.link_threads {
        // `-pthread`, link-only: Jolt's `JobSystemThreadPool` needs the platform threads library
        // at link time.
        println!("cargo::rustc-link-lib=pthread");
    }
    // `cc` links the chosen stdlib for the compiled objects, but the final Rust link needs the
    // C++ runtime named explicitly.
    println!("cargo::rustc-link-lib=c++");
    println!("cargo::rustc-link-lib=c++abi");
}

/// The Jolt source root in the gitignored vendor cache, populated on demand by
/// `fetch::ensure_vendored_jolt`. A Jolt bump is a replay-format migration.
fn vendored_jolt_root() -> PathBuf {
    vendor_dir().join(fetch::ARCHIVE_TOP_DIR)
}

/// The gitignored vendor cache directory (`.gitignore`: `engine/crates/physics-sys/vendor/`).
fn vendor_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor")
}

/// Every `.cpp` under the fetched `Jolt/` tree. Jolt guards platform-specific code with the
/// preprocessor, so the full set compiles given the right defines.
fn jolt_sources(jolt_lib_dir: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    collect_cpp(jolt_lib_dir, &mut sources);
    sources.sort(); // stable order so the archive is reproducible
    assert!(
        !sources.is_empty(),
        "no Jolt sources found under {} — the fetched tree is missing or empty",
        jolt_lib_dir.display()
    );
    sources
}

fn collect_cpp(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_cpp(&path, out);
        } else if path.extension().is_some_and(|e| e == "cpp") {
            out.push(path);
        }
    }
}

fn main() {
    // `CARGO_CFG_TARGET_ARCH` is the target's arch even under a cross-compile, where the build
    // script's own `cfg!(target_arch)` would report the host's.
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH")
        .expect("CARGO_CFG_TARGET_ARCH is always set for a build script");
    let flags = JoltBuildFlags::for_arch(&target_arch);
    let jolt_root = vendored_jolt_root();
    let jolt_lib_dir = jolt_root.join("Jolt");
    let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shim");

    if let Err(err) = fetch::ensure_vendored_jolt(&vendor_dir(), &jolt_root) {
        panic!("{err}");
    }

    assert!(
        jolt_lib_dir.is_dir(),
        "Jolt source missing at {} — run `just fetch-deps` to fetch the pinned release, or build \
         on a host with network access (build.rs fetches it on demand)",
        jolt_lib_dir.display()
    );

    // The fetched Jolt tree is content-pinned by tag and checksum, so it is not a
    // `rerun-if-changed` input: it cannot change without a version bump in this file.
    println!("cargo::rerun-if-changed={}", shim_dir.display());
    println!("cargo::rerun-if-changed=src/bridge.rs");
    println!("cargo::rerun-if-changed=src/jolt_build_flags.rs");

    // Jolt, the shim, and the `cxx`-generated glue compile into ONE static archive: two archives
    // would impose a static-link order the shim's Jolt references cannot satisfy. Every TU must
    // see the identical `JPH_*` defines, because an ABI skew between shim and Jolt is silent
    // memory corruption.
    let mut build = cxx_build::bridge("src/bridge.rs");
    apply_flags(&flags, &mut build);
    build.include(&jolt_root);
    build.include(&shim_dir);
    for src in jolt_sources(&jolt_lib_dir) {
        build.file(src);
    }
    build.file(shim_dir.join("jolt_bridge.cpp"));
    build.compile("saffron_jolt");

    emit_link_directives(&flags);

    // The cfg lets a `#[test]` confirm the build carried the flags, not just that the data table
    // lists them.
    println!("cargo::rustc-check-cfg=cfg(jolt_deterministic)");
    if flags
        .defines
        .iter()
        .any(|(k, _)| *k == "JPH_CROSS_PLATFORM_DETERMINISTIC")
        && !flags
            .defines
            .iter()
            .any(|(k, _)| *k == "JPH_DOUBLE_PRECISION")
    {
        println!("cargo::rustc-cfg=jolt_deterministic");
    }
}
