//! The `slangc` shader pipeline.
//!
//! Compiles every `*.slang` entry-point shader in `engine/assets/shaders/` to
//! `<runtime>/shaders/<name>.spv`, precompiles the shared `lighting.slang` to a reusable
//! `lighting.slang-module`, copies each `.slang` source into `<runtime>/shaders/source/`
//! (the runtime node-graph codegen splices `mesh.slang`), and copies the `models/`, `fonts/`, `icons/`
//! asset trees next to the host binary. A generated qualification manifest binds every SPIR-V
//! artifact to its compiler, flags, defines, and compiler-resolved transitive source closure.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SHADER_ARTIFACT_MANIFEST: &str = "shader-artifacts.generated.json";
const SHADER_ARTIFACT_SCHEMA_VERSION: u32 = 1;
const COMPILE_INPUT_HASH_DOMAIN: &[u8] = b"saffron-anima/shader-compile-input/v1\0";

/// The Slang module half is special: it has no entry points and emits no `.spv`. It is
/// precompiled once to `lighting.slang-module`; `mesh.slang` and codegen material variants
/// `import lighting` against the precompiled module rather than recompiling it.
const LIGHTING_STEM: &str = "lighting";

/// Resource-free lighting types and sampling helpers shared by forward surfaces and volumetric fog.
const LIGHTING_COMMON_STEM: &str = "lighting_common";

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

/// The resource-free order-2 sky SH module shared by projection and every reconstruction consumer.
const SKY_SH_STEM: &str = "sky_sh";

/// The resource-free tonemap-operator module — like `octahedral`, no entry points, no `.spv`.
/// Imported from source (via `-I`) by `tonemap.slang`. No runtime codegen splices it, so it needs no
/// precompiled `.slang-module`; it is only excluded from the entry-point `.spv` compile.
const TONEMAP_OPS_STEM: &str = "tonemap_ops";

/// The authoritative fixed-point and counter-RNG module shared by CPU-parity compute work.
const SPATIAL_NUMERIC_STEM: &str = "spatial_numeric";

/// The resource-free cloud shape/noise module shared by the static bakes, weather fill, density
/// debugger, and production cloud march. Imported from source through the shader include path.
const CLOUDS_STEM: &str = "clouds";
/// The resource-free cloud lighting module shared by the production cloud march.
const CLOUD_LIGHTING_STEM: &str = "cloud_lighting";
/// The resource-parameterized bounded atmosphere march shared by AP fill and cloud compositing.
const ATMOS_AP_STEM: &str = "atmos_ap";
/// The canonical coverage classifier shared by every geometry raster pass.
const COVERAGE_STEM: &str = "coverage";
/// The byte-locked material ABI shared by every geometry raster pass.
const MATERIAL_PARAMS_STEM: &str = "material_params";
/// The byte-locked global GPU table and draw-record ABI.
const GLOBAL_GPU_DATA_STEM: &str = "global_gpu_data";
/// The shared executor-bucket lookup vocabulary.
const SCENE_BIN_COMMON_STEM: &str = "scene_bin_common";
/// The shared micro-field reconstruction vocabulary.
const SCENE_MICRO_COMMON_STEM: &str = "scene_micro_common";
/// The resource-free energy-conserving thin-sheet optical partition.
const THIN_SHEET_STEM: &str = "thin_sheet";
/// The shared deterministic wind-field sampler (mirrors `saffron-wind`).
const WIND_STEM: &str = "wind";

/// The forward/gbuffer übershader stem. It alone gets an RT-off variant (see the fan-out).
const MESH_STEM: &str = "mesh";
/// The preprocessor define that compiles the RT-off übershader variant (strips the ray-tracing
/// descriptor sets 6/7 so the shader interface matches the RT-less PSO layout).
const NO_RT_DEFINE: &str = "SAFFRON_NO_RT=1";
/// The output-name suffix for the RT-off variant (`mesh_nort.spv`).
const NO_RT_SUFFIX: &str = "_nort";

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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShaderArtifactManifest {
    schema_version: u32,
    slangc_version: String,
    spirv_flags: Vec<String>,
    artifacts: Vec<ShaderArtifactEntry>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShaderArtifactEntry {
    shader: String,
    source: String,
    artifact: String,
    defines: Vec<String>,
    source_files: Vec<String>,
    compile_input_sha256: String,
    spirv_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ShaderVariant {
    shader: String,
    source: PathBuf,
    source_name: String,
    artifact: String,
    defines: Vec<String>,
}

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
    let runtime_source_dir = out_dir.join("source");
    std::fs::create_dir_all(&runtime_source_dir).with_context(|| {
        format!(
            "creating shader source dir {}",
            runtime_source_dir.display()
        )
    })?;
    let manifest_path = out_dir.join(SHADER_ARTIFACT_MANIFEST);
    let slangc_version = slangc_version(&config.slangc)?;
    let spirv_flags = SLANGC_SPV_FLAGS
        .iter()
        .map(|flag| (*flag).to_owned())
        .collect::<Vec<_>>();
    let previous_manifest = read_manifest_if_valid(&manifest_path)?;
    let compiler_changed = previous_manifest
        .as_ref()
        .is_none_or(|manifest| manifest.slangc_version != slangc_version);

    let lighting_src = config.shader_src_dir.join("lighting.slang");
    if !lighting_src.is_file() {
        bail!(
            "shared lighting source not found: {}",
            lighting_src.display()
        );
    }

    let lighting_common_src = config.shader_src_dir.join("lighting_common.slang");
    if !lighting_common_src.is_file() {
        bail!(
            "shared lighting-common source not found: {}",
            lighting_common_src.display()
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

    let sky_sh_src = config.shader_src_dir.join("sky_sh.slang");
    if !sky_sh_src.is_file() {
        bail!("shared sky SH source not found: {}", sky_sh_src.display());
    }

    let tonemap_ops_src = config.shader_src_dir.join("tonemap_ops.slang");
    if !tonemap_ops_src.is_file() {
        bail!(
            "shared tonemap_ops source not found: {}",
            tonemap_ops_src.display()
        );
    }

    let spatial_numeric_src = config.shader_src_dir.join("spatial_numeric.slang");
    if !spatial_numeric_src.is_file() {
        bail!(
            "shared spatial numeric source not found: {}",
            spatial_numeric_src.display()
        );
    }

    let clouds_src = config.shader_src_dir.join("clouds.slang");
    if !clouds_src.is_file() {
        bail!("shared clouds source not found: {}", clouds_src.display());
    }
    let cloud_lighting_src = config.shader_src_dir.join("cloud_lighting.slang");
    if !cloud_lighting_src.is_file() {
        bail!(
            "shared cloud lighting source not found: {}",
            cloud_lighting_src.display()
        );
    }
    let atmos_ap_src = config.shader_src_dir.join("atmos_ap.slang");
    if !atmos_ap_src.is_file() {
        bail!(
            "shared atmosphere AP source not found: {}",
            atmos_ap_src.display()
        );
    }
    let coverage_src = config.shader_src_dir.join("coverage.slang");
    if !coverage_src.is_file() {
        bail!(
            "shared coverage source not found: {}",
            coverage_src.display()
        );
    }
    validate_coverage_class_constants(&coverage_src)?;
    let material_params_src = config.shader_src_dir.join("material_params.slang");
    if !material_params_src.is_file() {
        bail!(
            "shared material parameters source not found: {}",
            material_params_src.display()
        );
    }
    let thin_sheet_src = config.shader_src_dir.join("thin_sheet.slang");
    if !thin_sheet_src.is_file() {
        bail!(
            "shared thin-sheet source not found: {}",
            thin_sheet_src.display()
        );
    }

    let shader_sources = shader_sources(&config.shader_src_dir)?;
    for (stem, path) in &shader_sources {
        copy_if_different(path, &runtime_source_dir.join(format!("{stem}.slang")))?;
    }
    let variants = shader_variants(&shader_sources);
    let mut report = Report::default();

    // The `octahedral` module (no imports) compiles first; `sdf` re-exports it and `lighting`
    // imports `sdf`, so an `octahedral` touch fans out to both modules + every entry-point `.spv`.
    let octahedral_module = out_dir.join("octahedral.slang-module");
    if compiler_changed || is_stale(&octahedral_module, &[&octahedral_src])? {
        compile_module(&config.slangc, &octahedral_src, &octahedral_module)?;
        report.module_compiled = true;
    }

    // The `giprobe` module (the DDGI probe-cage sampler, no imports) compiles before `lighting`
    // (which imports it) and `gi_resolve`; a touch fans out to both.
    let giprobe_module = out_dir.join("giprobe.slang-module");
    if compiler_changed || is_stale(&giprobe_module, &[&giprobe_src])? {
        compile_module(&config.slangc, &giprobe_src, &giprobe_module)?;
        report.module_compiled = true;
    }

    let sky_sh_module = out_dir.join("sky_sh.slang-module");
    if compiler_changed || is_stale(&sky_sh_module, &[&sky_sh_src])? {
        compile_module(&config.slangc, &sky_sh_src, &sky_sh_module)?;
        report.module_compiled = true;
    }

    // The `mdf_brick` module (the per-mesh brick sample) is imported by `sdf` and the Global-SDF
    // passes, so it compiles before `sdf` and a touch fans out to both.
    let mdf_brick_module = out_dir.join("mdf_brick.slang-module");
    if compiler_changed || is_stale(&mdf_brick_module, &[&mdf_brick_src])? {
        compile_module(&config.slangc, &mdf_brick_src, &mdf_brick_module)?;
        report.module_compiled = true;
    }

    // The `sdf` module imports `octahedral` + `mdf_brick`; `lighting` imports `sdf`, so a `sdf`
    // touch also rebuilds the lighting module + every entry-point `.spv` (the shared dep edge).
    let sdf_module = out_dir.join("sdf.slang-module");
    if compiler_changed || is_stale(&sdf_module, &[&sdf_src, &mdf_brick_src, &octahedral_src])? {
        compile_module(&config.slangc, &sdf_src, &sdf_module)?;
        report.module_compiled = true;
    }

    let lighting_module = out_dir.join("lighting.slang-module");
    if compiler_changed
        || is_stale(
            &lighting_module,
            &[
                &lighting_src,
                &lighting_common_src,
                &sdf_src,
                &mdf_brick_src,
                &octahedral_src,
                &giprobe_src,
                &sky_sh_src,
                &coverage_src,
                &material_params_src,
                &thin_sheet_src,
            ],
        )?
    {
        compile_module(&config.slangc, &lighting_src, &lighting_module)?;
        report.module_compiled = true;
    }

    let mut artifacts = Vec::with_capacity(variants.len());
    for variant in &variants {
        let artifact_path = out_dir.join(&variant.artifact);
        let source_files = match qualified_source_files(
            previous_manifest.as_ref(),
            variant,
            &slangc_version,
            &spirv_flags,
            &config.shader_src_dir,
            &artifact_path,
        )? {
            Some(source_files) => {
                report.spv_skipped += 1;
                source_files
            }
            None => {
                let depfile = TemporaryDepfile::new(&out_dir, &variant.shader);
                compile_spv(
                    &config.slangc,
                    &variant.source,
                    &config.shader_src_dir,
                    &artifact_path,
                    &depfile.path,
                    &variant.defines,
                )?;
                report.spv_compiled += 1;
                source_files_from_depfile(&depfile.path, &config.shader_src_dir)?
            }
        };
        let compile_input_sha256 = compile_input_sha256(
            &config.shader_src_dir,
            &source_files,
            &spirv_flags,
            &variant.defines,
        )?;
        let spirv_sha256 = sha256_file(&artifact_path)?;
        artifacts.push(ShaderArtifactEntry {
            shader: variant.shader.clone(),
            source: variant.source_name.clone(),
            artifact: variant.artifact.clone(),
            defines: variant.defines.clone(),
            source_files,
            compile_input_sha256,
            spirv_sha256,
        });
    }

    let manifest = ShaderArtifactManifest {
        schema_version: SHADER_ARTIFACT_SCHEMA_VERSION,
        slangc_version,
        spirv_flags,
        artifacts,
    };
    validate_manifest(&manifest, &variants, &config.shader_src_dir, &out_dir)?;

    copy_asset_tree(&config.asset_src_dir, &config.runtime_dir, "models")?;
    copy_asset_tree(&config.asset_src_dir, &config.runtime_dir, "fonts")?;
    copy_asset_tree(&config.asset_src_dir, &config.runtime_dir, "icons")?;
    write_manifest(&manifest_path, &manifest)?;

    Ok(report)
}

fn shader_sources(shader_src_dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut sources = Vec::new();
    for entry in std::fs::read_dir(shader_src_dir)
        .with_context(|| format!("reading shader dir {}", shader_src_dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("slang") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .with_context(|| format!("non-utf8 shader name: {}", path.display()))?
            .to_owned();
        sources.push((stem, path));
    }
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(sources)
}

fn shader_variants(sources: &[(String, PathBuf)]) -> Vec<ShaderVariant> {
    let mut variants = Vec::new();
    for (stem, source) in sources {
        if is_shared_source(stem) {
            continue;
        }
        variants.push(ShaderVariant {
            shader: stem.clone(),
            source: source.clone(),
            source_name: format!("{stem}.slang"),
            artifact: format!("{stem}.spv"),
            defines: Vec::new(),
        });
        if stem == MESH_STEM {
            let shader = format!("{stem}{NO_RT_SUFFIX}");
            variants.push(ShaderVariant {
                shader: shader.clone(),
                source: source.clone(),
                source_name: format!("{stem}.slang"),
                artifact: format!("{shader}.spv"),
                defines: vec![NO_RT_DEFINE.to_owned()],
            });
        }
    }
    variants.sort_by(|left, right| left.shader.cmp(&right.shader));
    variants
}

fn is_shared_source(stem: &str) -> bool {
    matches!(
        stem,
        LIGHTING_STEM
            | LIGHTING_COMMON_STEM
            | SDF_STEM
            | MDF_BRICK_STEM
            | OCTAHEDRAL_STEM
            | GIPROBE_STEM
            | SKY_SH_STEM
            | TONEMAP_OPS_STEM
            | SPATIAL_NUMERIC_STEM
            | CLOUDS_STEM
            | CLOUD_LIGHTING_STEM
            | ATMOS_AP_STEM
            | COVERAGE_STEM
            | MATERIAL_PARAMS_STEM
            | GLOBAL_GPU_DATA_STEM
            | SCENE_BIN_COMMON_STEM
            | SCENE_MICRO_COMMON_STEM
            | THIN_SHEET_STEM
            | WIND_STEM
    )
}

fn validate_coverage_class_constants(path: &Path) -> Result<()> {
    use saffron_vegetation::AlphaClassification;

    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading canonical coverage source {}", path.display()))?;
    for (name, value) in [
        ("COVERAGE_CLASS_OPAQUE", AlphaClassification::Opaque as u32),
        ("COVERAGE_CLASS_MASKED", AlphaClassification::Masked as u32),
        (
            "COVERAGE_CLASS_TRANSMISSIVE",
            AlphaClassification::Transmissive as u32,
        ),
    ] {
        let declaration = format!("public static const uint {name} = {value}u;");
        if !source.contains(&declaration) {
            bail!(
                "{} must declare `{declaration}` from AlphaClassification",
                path.display()
            );
        }
    }
    Ok(())
}

fn slangc_version(slangc: &Path) -> Result<String> {
    let output = Command::new(slangc)
        .arg("-version")
        .output()
        .with_context(|| format!("spawning {} -version", slangc.display()))?;
    if !output.status.success() {
        bail!("{} -version failed ({})", slangc.display(), output.status);
    }
    let stdout = String::from_utf8(output.stdout)
        .context("slangc -version stdout is not UTF-8")?
        .trim()
        .to_owned();
    let stderr = String::from_utf8(output.stderr)
        .context("slangc -version stderr is not UTF-8")?
        .trim()
        .to_owned();
    let version = match (stdout.is_empty(), stderr.is_empty()) {
        (false, true) => stdout,
        (true, false) => stderr,
        (false, false) => format!("{stdout}\n{stderr}"),
        (true, true) => String::new(),
    };
    if version.is_empty() {
        bail!("{} -version returned an empty identity", slangc.display());
    }
    Ok(version)
}

fn read_manifest_if_valid(path: &Path) -> Result<Option<ShaderArtifactManifest>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let Ok(manifest) = serde_json::from_slice::<ShaderArtifactManifest>(&bytes) else {
        return Ok(None);
    };
    let ordered = manifest
        .artifacts
        .windows(2)
        .all(|pair| pair[0].shader < pair[1].shader);
    Ok((manifest.schema_version == SHADER_ARTIFACT_SCHEMA_VERSION && ordered).then_some(manifest))
}

fn qualified_source_files(
    manifest: Option<&ShaderArtifactManifest>,
    variant: &ShaderVariant,
    slangc_version: &str,
    spirv_flags: &[String],
    shader_src_dir: &Path,
    artifact_path: &Path,
) -> Result<Option<Vec<String>>> {
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    if manifest.schema_version != SHADER_ARTIFACT_SCHEMA_VERSION
        || manifest.slangc_version != slangc_version
        || manifest.spirv_flags != spirv_flags
    {
        return Ok(None);
    }
    let Ok(index) = manifest
        .artifacts
        .binary_search_by(|entry| entry.shader.as_str().cmp(&variant.shader))
    else {
        return Ok(None);
    };
    let entry = &manifest.artifacts[index];
    if entry.source != variant.source_name
        || entry.artifact != variant.artifact
        || entry.defines != variant.defines
        || entry.source_files.is_empty()
        || !entry.source_files.contains(&variant.source_name)
        || !source_file_names_are_canonical(&entry.source_files)
        || !artifact_path.is_file()
    {
        return Ok(None);
    }
    for source in &entry.source_files {
        let Some(path) = source_file_path(shader_src_dir, source) else {
            return Ok(None);
        };
        if !path.is_file() {
            return Ok(None);
        }
    }
    let compile_input_sha256 = compile_input_sha256(
        shader_src_dir,
        &entry.source_files,
        spirv_flags,
        &variant.defines,
    )?;
    if compile_input_sha256 != entry.compile_input_sha256
        || sha256_file(artifact_path)? != entry.spirv_sha256
    {
        return Ok(None);
    }
    Ok(Some(entry.source_files.clone()))
}

fn compile_input_sha256(
    shader_src_dir: &Path,
    source_files: &[String],
    spirv_flags: &[String],
    defines: &[String],
) -> Result<String> {
    let source_files = source_files.iter().collect::<BTreeSet<_>>();
    let mut hasher = Sha256::new();
    hasher.update(COMPILE_INPUT_HASH_DOMAIN);
    hash_string_sequence(
        &mut hasher,
        b"spirv-flags",
        spirv_flags.iter().map(String::as_str),
    )?;
    hash_string_sequence(&mut hasher, b"defines", defines.iter().map(String::as_str))?;
    hash_len(&mut hasher, source_files.len())?;
    for source in source_files {
        let path = source_file_path(shader_src_dir, source)
            .with_context(|| format!("invalid canonical shader source path '{source}'"))?;
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading shader compile input {}", path.display()))?;
        hash_bytes(&mut hasher, source.as_bytes())?;
        hash_bytes(&mut hasher, &bytes)?;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_string_sequence<'a>(
    hasher: &mut Sha256,
    label: &[u8],
    values: impl ExactSizeIterator<Item = &'a str>,
) -> Result<()> {
    hash_bytes(hasher, label)?;
    hash_len(hasher, values.len())?;
    for value in values {
        hash_bytes(hasher, value.as_bytes())?;
    }
    Ok(())
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) -> Result<()> {
    hash_len(hasher, bytes.len())?;
    hasher.update(bytes);
    Ok(())
}

fn hash_len(hasher: &mut Sha256, len: usize) -> Result<()> {
    let len = u64::try_from(len).context("shader compile input length exceeds u64")?;
    hasher.update(len.to_be_bytes());
    Ok(())
}

fn source_files_from_depfile(depfile: &Path, shader_src_dir: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(depfile)
        .with_context(|| format!("reading slang dependency file {}", depfile.display()))?;
    let (_, dependencies) = text
        .split_once(": ")
        .with_context(|| format!("invalid slang dependency file {}", depfile.display()))?;
    let shader_src_dir = std::fs::canonicalize(shader_src_dir)
        .with_context(|| format!("canonicalizing {}", shader_src_dir.display()))?;
    let mut sources = BTreeSet::new();
    for dependency in makefile_words(dependencies) {
        let path = std::fs::canonicalize(&dependency)
            .with_context(|| format!("canonicalizing slang dependency {dependency}"))?;
        let relative = path.strip_prefix(&shader_src_dir).with_context(|| {
            format!(
                "slang dependency {} is outside {}",
                path.display(),
                shader_src_dir.display()
            )
        })?;
        let relative = canonical_relative_name(relative)?;
        if Path::new(&relative)
            .extension()
            .and_then(|value| value.to_str())
            != Some("slang")
        {
            bail!("slang dependency '{relative}' is not a .slang source");
        }
        sources.insert(relative);
    }
    if sources.is_empty() {
        bail!("slang dependency file {} is empty", depfile.display());
    }
    Ok(sources.into_iter().collect())
}

fn makefile_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut escaped = false;
    for character in text.chars() {
        if escaped {
            escaped = false;
            if character != '\n' && character != '\r' {
                word.push(character);
            }
        } else if character == '\\' {
            escaped = true;
        } else if character.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(character);
        }
    }
    if escaped {
        word.push('\\');
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn source_file_names_are_canonical(source_files: &[String]) -> bool {
    !source_files.is_empty()
        && source_files.windows(2).all(|pair| pair[0] < pair[1])
        && source_files
            .iter()
            .all(|source| source_file_path(Path::new("."), source).is_some())
}

fn source_file_path(shader_src_dir: &Path, source: &str) -> Option<PathBuf> {
    let path = Path::new(source);
    if path.extension().and_then(|extension| extension.to_str()) != Some("slang")
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(shader_src_dir.join(path))
}

fn canonical_relative_name(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(part) = component else {
            bail!(
                "shader dependency path is not canonical: {}",
                path.display()
            );
        };
        parts.push(
            part.to_str()
                .with_context(|| format!("non-UTF-8 shader dependency {}", path.display()))?,
        );
    }
    Ok(parts.join("/"))
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn validate_manifest(
    manifest: &ShaderArtifactManifest,
    variants: &[ShaderVariant],
    shader_src_dir: &Path,
    out_dir: &Path,
) -> Result<()> {
    if manifest.schema_version != SHADER_ARTIFACT_SCHEMA_VERSION {
        bail!("shader artifact manifest schema version is not current");
    }
    let expected_flags = SLANGC_SPV_FLAGS
        .iter()
        .map(|flag| (*flag).to_owned())
        .collect::<Vec<_>>();
    if manifest.spirv_flags != expected_flags {
        bail!("shader artifact manifest SPIR-V flags do not match the compiler pipeline");
    }
    if manifest.artifacts.len() != variants.len() {
        bail!("shader artifact manifest does not cover every generated variant");
    }
    for (entry, variant) in manifest.artifacts.iter().zip(variants) {
        if entry.shader != variant.shader
            || entry.source != variant.source_name
            || entry.artifact != variant.artifact
            || entry.defines != variant.defines
            || !source_file_names_are_canonical(&entry.source_files)
            || !entry.source_files.contains(&entry.source)
        {
            bail!(
                "shader artifact manifest entry '{}' is not canonical",
                entry.shader
            );
        }
        let compile_input_sha256 = compile_input_sha256(
            shader_src_dir,
            &entry.source_files,
            &manifest.spirv_flags,
            &entry.defines,
        )?;
        if compile_input_sha256 != entry.compile_input_sha256 {
            bail!(
                "shader artifact manifest input hash mismatch for '{}'",
                entry.shader
            );
        }
        let artifact = out_dir.join(&entry.artifact);
        if sha256_file(&artifact)? != entry.spirv_sha256 {
            bail!(
                "shader artifact manifest SPIR-V hash mismatch for '{}'",
                entry.shader
            );
        }
    }
    Ok(())
}

fn write_manifest(path: &Path, manifest: &ShaderArtifactManifest) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(manifest).context("serializing shader manifest")?;
    bytes.push(b'\n');
    let mut file = AtomicWriteFile::options()
        .open(path)
        .with_context(|| format!("opening atomic shader manifest {}", path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("writing atomic shader manifest {}", path.display()))?;
    file.commit()
        .with_context(|| format!("committing atomic shader manifest {}", path.display()))
}

struct TemporaryDepfile {
    path: PathBuf,
}

impl TemporaryDepfile {
    fn new(out_dir: &Path, shader: &str) -> Self {
        Self {
            path: out_dir.join(format!(".{shader}.dependencies.{}.tmp", std::process::id())),
        }
    }
}

impl Drop for TemporaryDepfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
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
fn compile_spv(
    slangc: &Path,
    src: &Path,
    include_dir: &Path,
    out: &Path,
    depfile: &Path,
    defines: &[String],
) -> Result<()> {
    let mut args = spv_arg_vector(src, include_dir, out, defines);
    args.push("-depfile".to_owned());
    args.push(depfile.to_string_lossy().into_owned());
    let status = Command::new(slangc)
        .args(args)
        .status()
        .with_context(|| format!("spawning slangc for {}", src.display()))?;
    if !status.success() {
        bail!("slangc failed for {} ({status})", src.display());
    }
    Ok(())
}

/// The exact argument vector `compile_spv` hands `slangc`, factored out as the single source of
/// truth so the flag-drift test asserts against the same flags the real compile uses. `defines`
/// are appended as `-D<name>` for feature variants (the RT-off übershader).
fn spv_arg_vector(src: &Path, include_dir: &Path, out: &Path, defines: &[String]) -> Vec<String> {
    let mut args = vec![src.to_string_lossy().into_owned()];
    args.extend(SLANGC_SPV_FLAGS.iter().map(|s| (*s).to_owned()));
    args.push("-I".to_owned());
    args.push(include_dir.to_string_lossy().into_owned());
    args.push("-o".to_owned());
    args.push(out.to_string_lossy().into_owned());
    for define in defines {
        args.push(format!("-D{define}"));
    }
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

/// Copies `src` to `dst` only when the contents differ, without churning mtimes on a no-op run.
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
            &[],
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

    #[test]
    fn shader_variants_have_deterministic_name_order() {
        let sources = vec![
            ("zeta".to_owned(), PathBuf::from("zeta.slang")),
            (LIGHTING_STEM.to_owned(), PathBuf::from("lighting.slang")),
            (COVERAGE_STEM.to_owned(), PathBuf::from("coverage.slang")),
            (
                MATERIAL_PARAMS_STEM.to_owned(),
                PathBuf::from("material_params.slang"),
            ),
            (
                GLOBAL_GPU_DATA_STEM.to_owned(),
                PathBuf::from("global_gpu_data.slang"),
            ),
            (
                THIN_SHEET_STEM.to_owned(),
                PathBuf::from("thin_sheet.slang"),
            ),
            (MESH_STEM.to_owned(), PathBuf::from("mesh.slang")),
            ("alpha".to_owned(), PathBuf::from("alpha.slang")),
        ];

        let variants = shader_variants(&sources);

        assert_eq!(
            variants
                .iter()
                .map(|variant| variant.shader.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "mesh", "mesh_nort", "zeta"]
        );
        assert_eq!(variants[2].defines, [NO_RT_DEFINE]);
    }

    #[test]
    fn geometry_passes_use_the_canonical_coverage_module() -> Result<()> {
        let shader_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/shaders");
        for shader in [
            "mesh.slang",
            "gbuffer.slang",
            "motion.slang",
            "point_shadow.slang",
        ] {
            let source = std::fs::read_to_string(shader_dir.join(shader))?;
            assert!(
                source.contains("sampleCanonicalCoverage("),
                "{shader} bypasses the canonical coverage sampler"
            );
        }

        Ok(())
    }

    #[test]
    fn compile_input_hash_is_order_independent_and_invalidates_every_input_class() -> Result<()> {
        let tmp = std::env::temp_dir().join(format!("xtask_shader_hash_{}", std::process::id()));
        std::fs::create_dir_all(&tmp)?;
        std::fs::write(tmp.join("entry.slang"), b"import shared;\n")?;
        std::fs::write(tmp.join("shared.slang"), b"const uint VALUE = 1;\n")?;
        let flags = SLANGC_SPV_FLAGS
            .iter()
            .map(|flag| (*flag).to_owned())
            .collect::<Vec<_>>();
        let forward = vec!["entry.slang".to_owned(), "shared.slang".to_owned()];
        let reverse = vec!["shared.slang".to_owned(), "entry.slang".to_owned()];

        let baseline = compile_input_sha256(&tmp, &forward, &flags, &[])?;
        assert_eq!(baseline, compile_input_sha256(&tmp, &reverse, &flags, &[])?);

        std::fs::write(tmp.join("shared.slang"), b"const uint VALUE = 2;\n")?;
        assert_ne!(baseline, compile_input_sha256(&tmp, &forward, &flags, &[])?);
        assert_ne!(
            baseline,
            compile_input_sha256(&tmp, &forward, &flags, &["FEATURE=1".to_owned()])?
        );
        let mut changed_flags = flags.clone();
        changed_flags.push("-O3".to_owned());
        assert_ne!(
            baseline,
            compile_input_sha256(&tmp, &forward, &changed_flags, &[])?
        );

        std::fs::remove_dir_all(&tmp)?;
        Ok(())
    }

    #[test]
    fn makefile_dependency_words_preserve_escaped_paths_and_continuations() {
        assert_eq!(
            makefile_words(
                "/shader/entry.slang /shader/shared\\ file.slang \\\n/shader/nested.slang\n"
            ),
            [
                "/shader/entry.slang",
                "/shader/shared file.slang",
                "/shader/nested.slang"
            ]
        );
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
