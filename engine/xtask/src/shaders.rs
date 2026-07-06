//! The `slangc` shader pipeline.
//!
//! Compiles every `*.slang` entry-point shader in `engine/assets/shaders/` to
//! `<runtime>/shaders/<name>.spv`, precompiles the shared `lighting.slang` to a reusable
//! `lighting.slang-module`, copies each `.slang` source next to its `.spv` (the runtime
//! node-graph codegen splices `mesh.slang`), and copies the `models/`, `fonts/`, `icons/`
//! asset trees next to the host binary. Staleness is tracked by source vs output mtime with
//! the `lighting.slang` shared-dependency edge, so a second run recompiles nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The Slang module half is special: it has no entry points and emits no `.spv`. It is
/// precompiled once to `lighting.slang-module`; `mesh.slang` and codegen material variants
/// `import lighting` against the precompiled module rather than recompiling it.
const LIGHTING_STEM: &str = "lighting";

/// The shared SDF sampling module — like `lighting`, it has no entry points and emits no
/// `.spv`. Precompiled to `sdf.slang-module`; both `lighting` (the GDF reflection-occlusion cone)
/// and `ddgi_trace` (the unified near/far field sphere-march) `import sdf` against it.
const SDF_STEM: &str = "sdf";

/// The shared per-mesh MDF brick-sample module — like `sdf`, no entry points, no `.spv`.
/// Precompiled to `mdf_brick.slang-module`; `sdf` (the cone trace) and the Global-SDF
/// `gdf_cull` / `gdf_composite` passes `import mdf_brick` for the one brick-sampling impl.
const MDF_BRICK_STEM: &str = "mdf_brick";

/// The resource-free octahedral encode module — like `lighting`/`sdf`, no entry points, no
/// `.spv`. Precompiled to `octahedral.slang-module`; `sdf` re-exports it (the DDGI trace + blend
/// reach octEncode/octDecode through that re-export) without pulling in `sdf`'s field bindings.
const OCTAHEDRAL_STEM: &str = "octahedral";

/// The shared DDGI probe-cage sampling module — like `sdf`, no entry points, no `.spv`. Precompiled
/// to `giprobe.slang-module`; both `lighting` (the mesh forward shade) and `gi_resolve` (the half-res
/// screen-space GI resolve) `import giprobe` for the one `ddgiSampleIrradiance` implementation.
const GIPROBE_STEM: &str = "giprobe";

/// The resource-free tonemap-operator module — like `octahedral`, no entry points, no `.spv`.
/// Imported from source (via `-I`) by `tonemap.slang` and the offscreen thumbnail/preview shaders
/// so scene + thumbnails encode identically. No runtime codegen splices it, so it needs no
/// precompiled `.slang-module`; it is only excluded from the entry-point `.spv` compile.
const TONEMAP_OPS_STEM: &str = "tonemap_ops";

/// The pinned Slang version the toolbox provides (the `SAFFRON_SLANG_VERSION` pin). Used only
/// to point at the conventional toolbox cache location when `slangc` is not otherwise found.
const SLANG_VERSION: &str = "2026.10";

/// The exact per-shader `slangc` flag set. A named constant so the flag-drift guard test asserts
/// against one source of truth.
pub const SLANGC_SPV_FLAGS: &[&str] = &[
    "-profile",
    "glsl_450",
    "-target",
    "spirv",
    "-emit-spirv-directly",
    "-fvk-use-entrypoint-name",
    "-matrix-layout-column-major",
    "-capability",
    SLANGC_CAPABILITIES,
];

/// Capabilities the shaders actually use that `glsl_450` does not imply (bindless non-uniform
/// indexing, sparse residency + min-LOD texture sampling, fragment-fully-covered, inline ray query,
/// the `VK_EXT_mesh_shader` task/mesh stages, and the SPIR-V debug-info extensions). Declared up
/// front so Slang does not implicitly upgrade the profile and emit an informational warning per
/// entry point.
const SLANGC_CAPABILITIES: &str = "SPV_KHR_non_semantic_info+SPV_GOOGLE_user_type+spvSparseResidency+spvMinLod+spvFragmentFullyCoveredEXT+spvShaderNonUniformEXT+spvRayQueryKHR+spvMeshShadingEXT+spvGroupNonUniform+spvGroupNonUniformBallot";

/// Inputs to one shader-pipeline run, resolved from the workspace layout + the build profile.
pub struct Config {
    /// `engine/assets/shaders/` — the `.slang` source tree (runtime data, not crate source).
    pub shader_src_dir: PathBuf,
    /// `engine/assets/` — holds the `models/`, `fonts/`, `icons/` trees to copy.
    pub asset_src_dir: PathBuf,
    /// The cargo target profile dir the host binary lands in (`target/<profile>/`). Shaders go
    /// under `<runtime>/shaders/`; asset trees are copied directly beside the binary.
    pub runtime_dir: PathBuf,
    /// The resolved `slangc` executable.
    pub slangc: PathBuf,
}

impl Config {
    /// Resolves the pipeline inputs from the workspace root and a cargo profile name.
    pub fn resolve(workspace_root: &Path, profile: &str) -> Result<Self> {
        let asset_src_dir = workspace_root.join("assets");
        let shader_src_dir = asset_src_dir.join("shaders");
        if !shader_src_dir.is_dir() {
            bail!(
                "shader source dir not found: {} (expected the engine/assets/shaders tree)",
                shader_src_dir.display()
            );
        }
        let runtime_dir = workspace_root.join("target").join(profile);
        let slangc = find_slangc()?;
        Ok(Self {
            shader_src_dir,
            asset_src_dir,
            runtime_dir,
            slangc,
        })
    }
}

/// What a single pipeline run did, for the caller to report.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Entry-point shaders recompiled to `.spv` this run (skips excluded).
    pub spv_compiled: usize,
    /// Entry-point shaders skipped because their `.spv` was already up to date.
    pub spv_skipped: usize,
    /// Whether `lighting.slang-module` was recompiled this run.
    pub module_compiled: bool,
}

/// Runs the full shader pipeline + asset copy.
pub fn run(config: &Config) -> Result<Report> {
    let out_dir = config.runtime_dir.join("shaders");
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating shader output dir {}", out_dir.display()))?;

    let lighting_src = config.shader_src_dir.join("lighting.slang");
    if !lighting_src.is_file() {
        bail!(
            "shared lighting source not found: {}",
            lighting_src.display()
        );
    }

    let sdf_src = config.shader_src_dir.join("sdf.slang");
    if !sdf_src.is_file() {
        bail!("shared sdf source not found: {}", sdf_src.display());
    }

    let mdf_brick_src = config.shader_src_dir.join("mdf_brick.slang");
    if !mdf_brick_src.is_file() {
        bail!(
            "shared mdf_brick source not found: {}",
            mdf_brick_src.display()
        );
    }

    let octahedral_src = config.shader_src_dir.join("octahedral.slang");
    if !octahedral_src.is_file() {
        bail!(
            "shared octahedral source not found: {}",
            octahedral_src.display()
        );
    }

    let giprobe_src = config.shader_src_dir.join("giprobe.slang");
    if !giprobe_src.is_file() {
        bail!("shared giprobe source not found: {}", giprobe_src.display());
    }

    let tonemap_ops_src = config.shader_src_dir.join("tonemap_ops.slang");
    if !tonemap_ops_src.is_file() {
        bail!(
            "shared tonemap_ops source not found: {}",
            tonemap_ops_src.display()
        );
    }

    let mut report = Report::default();

    // The `octahedral` module (no imports) compiles first; `sdf` re-exports it and `lighting`
    // imports `sdf`, so an `octahedral` touch fans out to both modules + every entry-point `.spv`.
    let octahedral_module = out_dir.join("octahedral.slang-module");
    if is_stale(&octahedral_module, &[&octahedral_src])? {
        compile_module(&config.slangc, &octahedral_src, &octahedral_module)?;
        report.module_compiled = true;
    }

    // The `giprobe` module (the DDGI probe-cage sampler, no imports) compiles before `lighting`
    // (which imports it) and `gi_resolve`; a touch fans out to both.
    let giprobe_module = out_dir.join("giprobe.slang-module");
    if is_stale(&giprobe_module, &[&giprobe_src])? {
        compile_module(&config.slangc, &giprobe_src, &giprobe_module)?;
        report.module_compiled = true;
    }

    // The `mdf_brick` module (the per-mesh brick sample) is imported by `sdf` and the Global-SDF
    // passes, so it compiles before `sdf` and a touch fans out to both.
    let mdf_brick_module = out_dir.join("mdf_brick.slang-module");
    if is_stale(&mdf_brick_module, &[&mdf_brick_src])? {
        compile_module(&config.slangc, &mdf_brick_src, &mdf_brick_module)?;
        report.module_compiled = true;
    }

    // The `sdf` module imports `octahedral` + `mdf_brick`; `lighting` imports `sdf`, so a `sdf`
    // touch also rebuilds the lighting module + every entry-point `.spv` (the shared dep edge).
    let sdf_module = out_dir.join("sdf.slang-module");
    if is_stale(&sdf_module, &[&sdf_src, &mdf_brick_src, &octahedral_src])? {
        compile_module(&config.slangc, &sdf_src, &sdf_module)?;
        report.module_compiled = true;
    }

    let lighting_module = out_dir.join("lighting.slang-module");
    if is_stale(
        &lighting_module,
        &[
            &lighting_src,
            &sdf_src,
            &mdf_brick_src,
            &octahedral_src,
            &giprobe_src,
        ],
    )? {
        compile_module(&config.slangc, &lighting_src, &lighting_module)?;
        report.module_compiled = true;
    }

    for entry in std::fs::read_dir(&config.shader_src_dir)
        .with_context(|| format!("reading shader dir {}", config.shader_src_dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("slang") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .with_context(|| format!("non-utf8 shader name: {}", path.display()))?;
        if stem == LIGHTING_STEM
            || stem == SDF_STEM
            || stem == MDF_BRICK_STEM
            || stem == OCTAHEDRAL_STEM
            || stem == GIPROBE_STEM
            || stem == TONEMAP_OPS_STEM
        {
            continue;
        }

        let spv = out_dir.join(format!("{stem}.spv"));
        let src_copy = out_dir.join(format!("{stem}.slang"));

        // Every shader depends on the shared lighting + sdf + mdf_brick + octahedral modules (the
        // dep edge), so a touch of any forces a full fan-out rebuild.
        if is_stale(
            &spv,
            &[
                &path,
                &lighting_src,
                &sdf_src,
                &mdf_brick_src,
                &octahedral_src,
                &giprobe_src,
                &tonemap_ops_src,
            ],
        )? {
            compile_spv(&config.slangc, &path, &config.shader_src_dir, &spv)?;
            report.spv_compiled += 1;
        } else {
            report.spv_skipped += 1;
        }
        copy_if_different(&path, &src_copy)?;
    }

    copy_asset_tree(&config.asset_src_dir, &config.runtime_dir, "models")?;
    copy_asset_tree(&config.asset_src_dir, &config.runtime_dir, "fonts")?;
    copy_asset_tree(&config.asset_src_dir, &config.runtime_dir, "icons")?;

    Ok(report)
}

/// Resolves `slangc`: a `PATH` lookup, then `SAFFRON_SLANG_DIR/bin`, then the conventional
/// toolbox cache (`$HOME/.cache/saffron-slang/slang/bin`). A missing `slangc` is a hard error —
/// the toolbox provisions it; there is no silent prebuilt fetch.
fn find_slangc() -> Result<PathBuf> {
    if let Ok(found) = which("slangc") {
        return Ok(found);
    }
    if let Ok(dir) = std::env::var("SAFFRON_SLANG_DIR") {
        let candidate = Path::new(&dir).join("bin").join("slangc");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidate = Path::new(&home)
            .join(".cache")
            .join("saffron-slang")
            .join("slang")
            .join("bin")
            .join("slangc");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "slangc not found on PATH, under SAFFRON_SLANG_DIR/bin, or in the toolbox cache \
         ($HOME/.cache/saffron-slang/slang/bin). The saffron-build toolbox provisions slangc \
         {SLANG_VERSION}; install it there rather than fetching a prebuilt at build time."
    );
}

/// A minimal `PATH` lookup for an executable, avoiding an extra crate dependency.
fn which(name: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("PATH not set")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("{name} not found on PATH")
}

/// `<name>.slang -> <name>.slang-module`: `slangc <src> -emit-ir -o <module>`, no entry
/// points, no `.spv`. Shared by the `lighting` + `sdf` module precompiles.
fn compile_module(slangc: &Path, src: &Path, module: &Path) -> Result<()> {
    let status = Command::new(slangc)
        .arg(src)
        .arg("-emit-ir")
        .arg("-o")
        .arg(module)
        .status()
        .with_context(|| format!("spawning slangc for {}", src.display()))?;
    if !status.success() {
        bail!("slangc failed for {} ({status})", src.display());
    }
    Ok(())
}

/// `<name>.slang -> <name>.spv`: the per-shader entry-point compile with the frozen flag set
/// plus the `-I <shader_dir>` include path and the `-o <out>`.
fn compile_spv(slangc: &Path, src: &Path, include_dir: &Path, out: &Path) -> Result<()> {
    let status = Command::new(slangc)
        .args(spv_arg_vector(src, include_dir, out))
        .status()
        .with_context(|| format!("spawning slangc for {}", src.display()))?;
    if !status.success() {
        bail!("slangc failed for {} ({status})", src.display());
    }
    Ok(())
}

/// The exact argument vector `compile_spv` hands `slangc`, factored out as the single source of
/// truth so the flag-drift test asserts against the same flags the real compile uses.
fn spv_arg_vector(src: &Path, include_dir: &Path, out: &Path) -> Vec<String> {
    let mut args = vec![src.to_string_lossy().into_owned()];
    args.extend(SLANGC_SPV_FLAGS.iter().map(|s| (*s).to_owned()));
    args.push("-I".to_owned());
    args.push(include_dir.to_string_lossy().into_owned());
    args.push("-o".to_owned());
    args.push(out.to_string_lossy().into_owned());
    args
}

/// An output is stale if it is missing or older than any of its source dependencies.
fn is_stale(output: &Path, deps: &[&Path]) -> Result<bool> {
    let out_mtime = match std::fs::metadata(output).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return Ok(true),
    };
    for dep in deps {
        let dep_mtime = std::fs::metadata(dep)
            .and_then(|m| m.modified())
            .with_context(|| format!("stat dependency {}", dep.display()))?;
        if dep_mtime > out_mtime {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Copies `src` to `dst` only when the contents differ — keeps each `.slang` next to its `.spv`
/// without churning mtimes on a no-op run.
fn copy_if_different(src: &Path, dst: &Path) -> Result<()> {
    if files_equal(src, dst)? {
        return Ok(());
    }
    std::fs::copy(src, dst)
        .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
    Ok(())
}

fn files_equal(a: &Path, b: &Path) -> Result<bool> {
    let (Ok(am), Ok(bm)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return Ok(false);
    };
    if am.len() != bm.len() {
        return Ok(false);
    }
    let ab = std::fs::read(a).with_context(|| format!("reading {}", a.display()))?;
    let bb = std::fs::read(b).with_context(|| format!("reading {}", b.display()))?;
    Ok(ab == bb)
}

/// Copies `<asset_src>/<name>` recursively to `<runtime>/<name>` (models/fonts/icons next to the
/// binary so `asset_path(...)` resolves).
fn copy_asset_tree(asset_src: &Path, runtime: &Path, name: &str) -> Result<()> {
    let from = asset_src.join(name);
    if !from.is_dir() {
        bail!("asset tree not found: {}", from.display());
    }
    let to = runtime.join(name);
    copy_dir_recursive(&from, &to)
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating dir {}", to.display()))?;
    for entry in
        std::fs::read_dir(from).with_context(|| format!("reading dir {}", from.display()))?
    {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            copy_if_different(&src, &dst)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    /// Guards against flag drift: the per-shader argument vector must be exactly `<src> -profile
    /// glsl_450 -target spirv -emit-spirv-directly -fvk-use-entrypoint-name
    /// -matrix-layout-column-major -capability <atoms> -I <dir> -o <out>`.
    #[test]
    fn spv_flag_set_is_frozen() {
        let args = spv_arg_vector(
            Path::new("/shaders/mesh.slang"),
            Path::new("/shaders"),
            Path::new("/out/mesh.spv"),
        );
        assert_eq!(
            args,
            vec![
                "/shaders/mesh.slang",
                "-profile",
                "glsl_450",
                "-target",
                "spirv",
                "-emit-spirv-directly",
                "-fvk-use-entrypoint-name",
                "-matrix-layout-column-major",
                "-capability",
                SLANGC_CAPABILITIES,
                "-I",
                "/shaders",
                "-o",
                "/out/mesh.spv",
            ]
        );
    }

    /// The module flag set: no spirv flags, just `-emit-ir`.
    #[test]
    fn lighting_module_is_excluded_from_spv_flags() {
        assert!(!SLANGC_SPV_FLAGS.contains(&"-emit-ir"));
        assert!(SLANGC_SPV_FLAGS.contains(&"-emit-spirv-directly"));
    }

    /// A missing output is always stale; an output newer than every dep is fresh; an output
    /// older than any dep is stale — the core of the recompile decision.
    #[test]
    fn staleness_tracks_mtime_against_deps() -> Result<()> {
        let tmp = std::env::temp_dir().join(format!("xtask_stale_{}", std::process::id()));
        std::fs::create_dir_all(&tmp)?;
        let out = tmp.join("out.spv");
        let dep = tmp.join("dep.slang");

        std::fs::write(&dep, b"a")?;
        // No output yet -> stale.
        assert!(is_stale(&out, &[&dep])?);

        // Write the output after the dep -> fresh.
        std::fs::write(&out, b"x")?;
        assert!(!is_stale(&out, &[&dep])?);

        // Touch the dep to be strictly newer -> stale again.
        let later = SystemTime::now() + std::time::Duration::from_secs(2);
        let f = std::fs::File::open(&dep)?;
        f.set_modified(later)?;
        assert!(is_stale(&out, &[&dep])?);

        std::fs::remove_dir_all(&tmp)?;
        Ok(())
    }

    /// `copy_if_different` writes when contents differ and leaves an identical target untouched.
    #[test]
    fn copy_if_different_skips_identical() -> Result<()> {
        let tmp = std::env::temp_dir().join(format!("xtask_copy_{}", std::process::id()));
        std::fs::create_dir_all(&tmp)?;
        let src = tmp.join("src.slang");
        let dst = tmp.join("dst.slang");
        std::fs::write(&src, b"hello")?;

        assert!(!files_equal(&src, &dst)?);
        copy_if_different(&src, &dst)?;
        assert!(files_equal(&src, &dst)?);

        // Mark dst's mtime in the past; an identical-contents copy must not rewrite it.
        let past = SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::open(&dst)?.set_modified(past)?;
        let before = std::fs::metadata(&dst)?.modified()?;
        copy_if_different(&src, &dst)?;
        let after = std::fs::metadata(&dst)?.modified()?;
        assert_eq!(before, after, "no-op copy must not touch mtime");

        std::fs::remove_dir_all(&tmp)?;
        Ok(())
    }
}
