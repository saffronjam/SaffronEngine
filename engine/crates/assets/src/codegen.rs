//! Node-graph → Slang codegen, compiled at runtime by invoking `slangc` through
//! [`std::process::Command`].
//!
//! Two compile targets, each writing a generated `.slang` then running `slangc` to
//! produce the matching `.spv`:
//!
//! - [`AssetServer::compile_material_graph`] — a self-contained fragment shader (the
//!   proof that the graph emits compilable Slang) → `materials/<uuid>.spv`.
//! - [`AssetServer::compile_material_mesh_shader`] — splices the emitted surface body
//!   into the runtime `mesh.slang` übershader between the `// @graph-begin` /
//!   `// @graph-end` markers, compiles with `-I <shaders dir>` so `import lighting`
//!   resolves, and emits `materials/<uuid>_mesh.spv` plus the RT-off
//!   `materials/<uuid>_mesh_nort.spv` sibling.
//!
//! # The slangc invocation
//!
//! [`build_slangc_command`] uses [`Command::new`] with discrete [`Command::arg`] calls
//! for the flag set, [`Stdio::null`] for both pipes, and inspects `status.success()`
//! plus the `.spv` existence — no shell string, no path-quoting surface.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use saffron_core::Uuid;
use saffron_json::Value;

use crate::AssetServer;
use crate::error::{Error, Result};
use crate::graph::emit_graph_surface;
use crate::load::engine_asset_path;

/// The begin marker that opens the spliceable default-body block in `mesh.slang`.
const GRAPH_BEGIN: &str = "// @graph-begin";
/// The end marker that closes the spliceable default-body block in `mesh.slang`.
const GRAPH_END: &str = "// @graph-end";
/// The feature define used by the RT-off mesh variant, matching the static shader pipeline.
const NO_RT_DEFINE: &str = "SAFFRON_NO_RT=1";

/// Locates `slangc`: the `SAFFRON_SLANGC` env override, else the prebuilt slang cache
/// (`~/.cache/saffron-slang/slang/bin/slangc`), else bare `slangc` on `PATH`.
#[must_use]
pub fn find_slangc() -> PathBuf {
    resolve_slangc(
        std::env::var_os("SAFFRON_SLANGC"),
        std::env::var_os("HOME"),
        Path::exists,
    )
}

/// The pure resolution behind [`find_slangc`], parameterized on the two env values and a
/// path-existence probe so it is testable without mutating the process environment: the
/// non-empty `SAFFRON_SLANGC` override wins, else the slang cache under `HOME` when it
/// exists, else bare `slangc` on `PATH`.
fn resolve_slangc(
    slangc_env: Option<OsString>,
    home_env: Option<OsString>,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(env) = slangc_env
        && !env.is_empty()
    {
        return PathBuf::from(env);
    }
    if let Some(home) = home_env {
        let cached = PathBuf::from(home).join(".cache/saffron-slang/slang/bin/slangc");
        if exists(&cached) {
            return cached;
        }
    }
    PathBuf::from("slangc")
}

/// The `slangc` flag set every compile shares, offline and at runtime. Defined once in
/// `saffron-core` because these two compilers previously kept separate copies and drifted.
use saffron_core::SLANGC_SPV_FLAGS as SLANGC_FLAGS;

/// Builds the full `slangc` argv (program first) for compiling `slang_path` to
/// `spv_path`, optionally adding `-I <include_dir>` (the mesh variant, so `import
/// lighting` resolves).
///
/// This is the single source of the argv shape, shared by [`build_slangc_command`] and
/// the argv test. No element is shell-quoted and none carries a redirection token —
/// each path is one discrete argv element.
fn slangc_argv(
    slangc: &Path,
    slang_path: &Path,
    spv_path: &Path,
    include_dir: Option<&Path>,
    defines: &[&str],
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = Vec::new();
    argv.push(slangc.as_os_str().to_owned());
    argv.push(slang_path.as_os_str().to_owned());
    for flag in SLANGC_FLAGS {
        argv.push(OsString::from(flag));
    }
    if let Some(dir) = include_dir {
        argv.push(OsString::from("-I"));
        argv.push(dir.as_os_str().to_owned());
    }
    argv.push(OsString::from("-o"));
    argv.push(spv_path.as_os_str().to_owned());
    for define in defines {
        argv.push(OsString::from(format!("-D{define}")));
    }
    argv
}

/// Builds the [`Command`] that compiles `slang_path` to `spv_path` (the argv from
/// [`slangc_argv`]), with both stdout and stderr silenced.
fn build_slangc_command(
    slangc: &Path,
    slang_path: &Path,
    spv_path: &Path,
    include_dir: Option<&Path>,
    defines: &[&str],
) -> Command {
    let argv = slangc_argv(slangc, slang_path, spv_path, include_dir, defines);
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Writes the generated source, runs `slangc`, and verifies the `.spv`.
///
/// Returns the `.spv` path on success. A write failure is an [`Error::Io`]; a spawn
/// failure, a non-zero exit, or a missing `.spv` is an [`Error::SlangcFailed`] naming the
/// target (`label`).
fn write_and_compile(
    slangc: &Path,
    slang_path: &Path,
    spv_path: &Path,
    source: &str,
    include_dir: Option<&Path>,
    defines: &[&str],
    label: &str,
) -> Result<PathBuf> {
    std::fs::write(slang_path, source).map_err(|err| {
        Error::Io(format!(
            "cannot write generated shader {slang_path:?}: {err}"
        ))
    })?;

    let status = build_slangc_command(slangc, slang_path, spv_path, include_dir, defines)
        .status()
        .map_err(|err| Error::SlangcFailed(format!("{label}: cannot spawn slangc: {err}")))?;
    if !status.success() || !spv_path.exists() {
        return Err(Error::SlangcFailed(format!(
            "{label}: slangc exited {status}"
        )));
    }
    Ok(spv_path.to_path_buf())
}

/// The self-contained fragment shader for a material graph: a `[[vk::push_constant]] Mat`
/// push, bindless `textures[]`, and the complete `SurfaceData`, with `evalSurface` filled
/// by the emitted surface body.
fn graph_shader_source(surface_body: &str) -> String {
    format!(
        "[[vk::binding(0, 0)]] Sampler2D textures[1024];\n\
         struct SurfaceData {{ float3 albedo; float metallic; float roughness; float3 normal; float3 sheetNormal; float3 emissive; float occlusion; float opacity; uint surfaceModel; float frontFace; float frontResponse; float backResponse; float thickness; float3 absorption; float3 transmission; float energyLimit; }};\n\
         struct Mat {{ float4 baseColor; uint4 tex; }};\n\
         [[vk::push_constant]] Mat mat;\n\
         SurfaceData evalSurface(float2 uv)\n{{\n    SurfaceData s;\n\
         {surface_body}\
         \x20\x20\x20\x20return s;\n}}\n\
         [shader(\"fragment\")] float4 fragmentMain(float2 uv : TEXCOORD0) : SV_Target\n{{\n\
         \x20\x20\x20\x20SurfaceData s = evalSurface(uv);\n    return float4(s.albedo + s.emissive, 1.0);\n}}\n"
    )
}

/// Splices the emitted surface `body` into the übershader `src` between the
/// `// @graph-begin` / `// @graph-end` markers: keeps the begin-marker line, drops the
/// default body, inserts `body`, then resumes at the end marker. Errors if either marker
/// is missing or out of order.
fn splice_mesh_source(src: &str, body: &str) -> Result<String> {
    let begin = src.find(GRAPH_BEGIN);
    let end = src.find(GRAPH_END);
    let (Some(begin), Some(end)) = (begin, end) else {
        return Err(Error::SlangcFailed(
            "übershader source is missing the @graph markers".to_owned(),
        ));
    };
    if end < begin {
        return Err(Error::SlangcFailed(
            "übershader source is missing the @graph markers".to_owned(),
        ));
    }
    // Keep the begin-marker line (through the newline that closes it), drop the default
    // body, insert the emitted surface, then resume at the end marker (indented).
    let body_start = src[begin..]
        .find('\n')
        .map_or(src.len(), |offset| begin + offset);
    let prefix_end = (body_start + 1).min(src.len());
    Ok(format!("{}{body}    {}", &src[..prefix_end], &src[end..]))
}

impl AssetServer {
    /// Emits a self-contained fragment shader for a material's node graph and compiles it
    /// with `slangc` to `materials/<uuid>.spv`, returning the `.spv` path. This proves
    /// the graph → compilable-Slang pipeline.
    pub fn compile_material_graph(&self, graph: &Value, id: Uuid) -> Result<PathBuf> {
        let slangc = find_slangc();
        let source = graph_shader_source(&emit_graph_surface(graph, false));
        self.ensure_asset_directories();
        let slang_path = self.material_artifact_path(id, ".slang");
        let spv_path = self.material_artifact_path(id, ".spv");
        write_and_compile(
            &slangc,
            &slang_path,
            &spv_path,
            &source,
            None,
            &[],
            &format!("material graph {}", id.value()),
        )
    }

    /// Splices the graph's emitted surface body into the runtime `mesh.slang` übershader
    /// and compiles RT and RT-off per-material variants from the staged source bundle,
    /// returning the `materials/<uuid>_mesh.spv` path. `render_scene` points a codegen
    /// material's `shader` at this path; pipeline selection chooses its `_nort` sibling
    /// on a device without ray tracing.
    pub fn compile_material_mesh_shader(&self, graph: &Value, id: Uuid) -> Result<PathBuf> {
        let slangc = find_slangc();
        let shaders_dir = engine_asset_path("shaders").join("source");
        let mesh_src_path = shaders_dir.join("mesh.slang");
        let src = std::fs::read_to_string(&mesh_src_path).map_err(|err| {
            Error::Io(format!(
                "cannot read übershader source {mesh_src_path:?} (is the shader source bundle installed?): {err}"
            ))
        })?;
        let spliced = splice_mesh_source(&src, &emit_graph_surface(graph, true))?;

        self.ensure_asset_directories();
        let slang_path = self.material_artifact_path(id, "_mesh.slang");
        let spv_path = self.material_artifact_path(id, "_mesh.spv");
        write_and_compile(
            &slangc,
            &slang_path,
            &spv_path,
            &spliced,
            Some(&shaders_dir),
            &[],
            &format!("übershader variant {}", id.value()),
        )?;
        let no_rt_path = self.material_artifact_path(id, "_mesh_nort.spv");
        write_and_compile(
            &slangc,
            &slang_path,
            &no_rt_path,
            &spliced,
            Some(&shaders_dir),
            &[NO_RT_DEFINE],
            &format!("RT-off übershader variant {}", id.value()),
        )?;
        Ok(spv_path)
    }

    /// `<root>/materials/<uuid><suffix>` — the codegen artifact path for a material id
    /// (`suffix` is e.g. `.slang`, `.spv`, `_mesh.slang`).
    ///
    /// Absolutized (against the cwd) so the `.spv` variants handed to the renderer load
    /// directly: the renderer's shader loaders treat a relative path as relative to the
    /// engine's *shader* directory (joining `resolve_shader_dir()`), so a project-relative
    /// artifact root (`appdata/userdata/<project>/assets`) would mis-resolve to
    /// `shaders/appdata/…`. An absolute path bypasses that join (the scene loader takes the
    /// `is_absolute` branch verbatim).
    fn material_artifact_path(&self, id: Uuid, suffix: &str) -> PathBuf {
        let relative = self
            .root
            .join("materials")
            .join(format!("{}{suffix}", id.value()));
        std::path::absolute(&relative).unwrap_or(relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small folded-but-codegen graph (a math node forces codegen): one constant, one
    /// textureSlot, a multiply, and the `materialOutput`.
    fn small_graph() -> Value {
        serde_json::json!({
            "nodes": [
                { "id": "c1", "type": "constant", "props": { "value": [0.5, 0.25, 1.0, 1.0] } },
                { "id": "tx", "type": "textureSlot", "props": { "slot": "normal" } },
                { "id": "mul", "type": "multiply" },
                { "id": "out", "type": "materialOutput" }
            ],
            "edges": [
                { "from": ["c1", "out"], "to": ["mul", "a"] },
                { "from": ["tx", "out"], "to": ["mul", "b"] },
                { "from": ["mul", "out"], "to": ["out", "baseColor"] }
            ]
        })
    }

    /// The fixed flag set every variant carries, in order, between the `.slang` input and
    /// the `-o <spv>` tail (and, for the mesh variant, the `-I <dir>` insertion).
    ///
    /// This is the shared constant, not a transcription of it. A local copy here is what let
    /// the runtime compiler drift away from the offline one unnoticed: the test happily pinned
    /// the stale list it carried itself, so a capability missing from the real compile looked
    /// correct right up until a shader needed it.
    const EXPECTED_FLAGS: &[&str] = SLANGC_FLAGS;

    /// No argv element may contain a shell quote or a redirection token.
    fn assert_no_shell_tokens(argv: &[OsString]) {
        for element in argv {
            let text = element.to_string_lossy();
            for token in ['"', '\'', '>', '<', '|', '&', ';', '`', '$'] {
                assert!(
                    !text.contains(token),
                    "argv element {text:?} contains shell token {token:?}"
                );
            }
            assert!(
                !text.contains("/dev/null"),
                "argv element {text:?} carries a redirection target"
            );
        }
    }

    #[test]
    fn graph_and_preview_argv_is_the_flat_flag_set() {
        let slangc = Path::new("slangc");
        let slang_path = Path::new("/proj/assets/materials/42.slang");
        let spv_path = Path::new("/proj/assets/materials/42.spv");
        let argv = slangc_argv(slangc, slang_path, spv_path, None, &[]);

        let mut expected: Vec<OsString> = vec![
            OsString::from("slangc"),
            OsString::from("/proj/assets/materials/42.slang"),
        ];
        for flag in EXPECTED_FLAGS {
            expected.push(OsString::from(flag));
        }
        expected.push(OsString::from("-o"));
        expected.push(OsString::from("/proj/assets/materials/42.spv"));

        assert_eq!(argv, expected);
        assert_no_shell_tokens(&argv);
    }

    #[test]
    fn rt_off_mesh_argv_carries_the_include_dir_and_define() {
        let slangc = Path::new("slangc");
        let slang_path = Path::new("/proj/assets/materials/42_mesh.slang");
        let spv_path = Path::new("/proj/assets/materials/42_mesh.spv");
        let include = Path::new("/opt/saffron/shaders");
        let argv = slangc_argv(slangc, slang_path, spv_path, Some(include), &[NO_RT_DEFINE]);

        let mut expected: Vec<OsString> = vec![
            OsString::from("slangc"),
            OsString::from("/proj/assets/materials/42_mesh.slang"),
        ];
        for flag in EXPECTED_FLAGS {
            expected.push(OsString::from(flag));
        }
        expected.push(OsString::from("-I"));
        expected.push(OsString::from("/opt/saffron/shaders"));
        expected.push(OsString::from("-o"));
        expected.push(OsString::from("/proj/assets/materials/42_mesh.spv"));
        expected.push(OsString::from("-DSAFFRON_NO_RT=1"));

        assert_eq!(argv, expected);
        assert_no_shell_tokens(&argv);
    }

    #[test]
    fn resolve_slangc_honors_env_then_cache_then_path() {
        // The cache path that exists only when HOME is set (and the probe says so).
        let cache_path = PathBuf::from("/home/dev/.cache/saffron-slang/slang/bin/slangc");
        let probe = {
            let cache_path = cache_path.clone();
            move |p: &Path| p == cache_path
        };

        // 1. A non-empty SAFFRON_SLANGC override wins outright (cache ignored).
        assert_eq!(
            resolve_slangc(
                Some(OsString::from("/custom/slangc")),
                Some(OsString::from("/home/dev")),
                &probe,
            ),
            PathBuf::from("/custom/slangc"),
        );

        // 2. An empty override is ignored; HOME's cache path exists → use it.
        assert_eq!(
            resolve_slangc(
                Some(OsString::new()),
                Some(OsString::from("/home/dev")),
                &probe,
            ),
            cache_path,
        );

        // 3. No override, HOME set, but the cache does not exist → bare PATH.
        assert_eq!(
            resolve_slangc(None, Some(OsString::from("/home/dev")), |_| false),
            PathBuf::from("slangc"),
        );

        // 4. No override and no HOME → bare PATH.
        assert_eq!(
            resolve_slangc(None, None, |_| true),
            PathBuf::from("slangc"),
        );
    }

    #[test]
    fn mesh_splice_keeps_markers_and_inserts_the_emitted_body() {
        let src = concat!(
            "SurfaceData evalSurface(MaterialInput m)\n{\n",
            "    SurfaceData s;\n",
            "    // @graph-begin\n",
            "    float4 base = sampleDefault();\n",
            "    s.albedo = base.rgb;\n",
            "    // @graph-end\n",
            "    return s;\n}\n"
        );
        let body = "    s.albedo = float3(1.0, 0.0, 0.0);\n    s.opacity = 1.0;\n";
        let spliced = splice_mesh_source(src, body).expect("splice");

        // The begin-marker line survives.
        assert!(spliced.contains("    // @graph-begin\n"));
        // The default body is gone.
        assert!(!spliced.contains("float4 base = sampleDefault();"));
        // The emitted surface is present.
        assert!(spliced.contains("s.albedo = float3(1.0, 0.0, 0.0);"));
        // It resumes at the end marker.
        assert!(spliced.contains("// @graph-end\n    return s;"));
        // The begin marker precedes the emitted body, which precedes the end marker.
        let begin = spliced.find("// @graph-begin").unwrap();
        let emitted = spliced.find("s.albedo = float3(1.0, 0.0, 0.0)").unwrap();
        let end = spliced.find("// @graph-end").unwrap();
        assert!(begin < emitted && emitted < end);
    }

    #[test]
    fn mesh_splice_against_the_real_ubershader() {
        // The actual mesh.slang must splice cleanly (the markers are present and ordered).
        let src = std::fs::read_to_string(
            engine_asset_path("shaders")
                .join("source")
                .join("mesh.slang"),
        );
        let Ok(src) = src else {
            eprintln!("skipping: mesh.slang not staged beside the test binary");
            return;
        };
        let body = emit_graph_surface(&small_graph(), true);
        let spliced = splice_mesh_source(&src, &body).expect("real mesh.slang splices");
        // The default glTF metallic-roughness body is dropped.
        assert!(!spliced.contains("float4 base = albedoTextures"));
        // The emitted surface body is in place.
        assert!(spliced.contains("float4 n_mul = n_c1 * n_tx;"));
        // `import lighting` survives at the top (the -I resolves it).
        assert!(spliced.starts_with("import lighting;"));
    }

    #[test]
    fn mesh_splice_missing_markers_is_an_error() {
        let no_markers = "SurfaceData evalSurface() { return s; }\n";
        assert!(matches!(
            splice_mesh_source(no_markers, "    s.albedo = float3(1.0);\n"),
            Err(Error::SlangcFailed(_))
        ));
        // Out-of-order markers are also rejected.
        let swapped = "// @graph-end\n// @graph-begin\n";
        assert!(matches!(
            splice_mesh_source(swapped, "    body\n"),
            Err(Error::SlangcFailed(_))
        ));
    }

    #[test]
    fn graph_shader_source_wraps_the_surface_body() {
        let body = emit_graph_surface(&small_graph(), false);
        let source = graph_shader_source(&body);
        assert!(source.contains("[[vk::push_constant]] Mat mat;"));
        assert!(source.contains("SurfaceData evalSurface(float2 uv)"));
        assert!(source.contains("float4 n_mul = n_c1 * n_tx;"));
        assert!(source.contains("[shader(\"fragment\")] float4 fragmentMain"));
    }

    #[test]
    fn graph_shader_source_is_byte_exact_with_an_empty_body() {
        // Byte-for-byte the graph shader template (empty surfaceBody).
        let expected = concat!(
            "[[vk::binding(0, 0)]] Sampler2D textures[1024];\n",
            "struct SurfaceData { float3 albedo; float metallic; float roughness; float3 normal; float3 sheetNormal; float3 emissive; float occlusion; float opacity; uint surfaceModel; float frontFace; float frontResponse; float backResponse; float thickness; float3 absorption; float3 transmission; float energyLimit; };\n",
            "struct Mat { float4 baseColor; uint4 tex; };\n",
            "[[vk::push_constant]] Mat mat;\n",
            "SurfaceData evalSurface(float2 uv)\n{\n    SurfaceData s;\n",
            "    return s;\n}\n",
            "[shader(\"fragment\")] float4 fragmentMain(float2 uv : TEXCOORD0) : SV_Target\n{\n",
            "    SurfaceData s = evalSurface(uv);\n    return float4(s.albedo + s.emissive, 1.0);\n}\n",
        );
        assert_eq!(graph_shader_source(""), expected);
    }

    /// The integration path: compile a folded graph end-to-end, gated on `slangc` being
    /// resolvable + runnable (else skipped + logged). Asserts a non-empty `.spv`.
    #[test]
    fn compile_material_graph_produces_a_non_empty_spv_when_slangc_present() {
        let slangc = find_slangc();
        // Probe slangc: spawn `slangc -v`; skip if it cannot run at all.
        let probe = Command::new(&slangc)
            .arg("-v")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if probe.is_err() {
            eprintln!("skipping: slangc not runnable ({slangc:?})");
            return;
        }

        let tmp = std::env::temp_dir().join(format!(
            "saffron-codegen-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let root = tmp.join("project").join("assets");
        let _ = std::fs::remove_dir_all(&tmp);
        let assets = AssetServer::new(&root);

        let id = Uuid(7777);
        match assets.compile_material_graph(&small_graph(), id) {
            Ok(spv) => {
                let bytes = std::fs::read(&spv).expect("read .spv");
                assert!(!bytes.is_empty(), "compiled .spv is non-empty");
                assert_eq!(spv, root.join("materials").join("7777.spv"));
            }
            Err(err) => panic!("compile failed with slangc present: {err}"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn compile_material_mesh_shader_produces_a_non_empty_spv_when_slangc_present() {
        let slangc = find_slangc();
        let probe = Command::new(&slangc)
            .arg("-v")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if probe.is_err() {
            eprintln!("skipping: slangc not runnable ({slangc:?})");
            return;
        }

        let tmp = std::env::temp_dir().join(format!(
            "saffron-codegen-mesh-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let root = tmp.join("project").join("assets");
        let _ = std::fs::remove_dir_all(&tmp);
        let assets = AssetServer::new(&root);
        let graph = serde_json::json!({
            "nodes": [
                { "id": "c1", "type": "constant", "props": { "value": [0, 1, 0, 1] } },
                { "id": "c2", "type": "constant", "props": { "value": [1, 1, 1, 1] } },
                { "id": "mul", "type": "multiply" },
                { "id": "out", "type": "materialOutput" }
            ],
            "edges": [
                { "from": ["c1", "rgba"], "to": ["mul", "a"] },
                { "from": ["c2", "rgba"], "to": ["mul", "b"] },
                { "from": ["mul", "rgba"], "to": ["out", "baseColor"] }
            ]
        });

        let spv = assets
            .compile_material_mesh_shader(&graph, Uuid(7778))
            .unwrap_or_else(|err| {
                panic!("mesh compile failed; generated source is under {tmp:?}: {err}")
            });
        let bytes = std::fs::read(&spv).expect("read mesh .spv");
        assert!(!bytes.is_empty(), "compiled mesh .spv is non-empty");
        assert_eq!(spv, root.join("materials").join("7778_mesh.spv"));
        let no_rt = root.join("materials").join("7778_mesh_nort.spv");
        assert!(
            std::fs::metadata(&no_rt).is_ok_and(|meta| meta.len() > 0),
            "compiled RT-off mesh .spv is non-empty"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A non-zero `slangc` exit (invalid source) surfaces [`Error::SlangcFailed`] and
    /// leaves no `.spv`, gated on `slangc` being runnable (else skipped + logged).
    #[test]
    fn write_and_compile_maps_a_non_zero_exit_to_slangc_failed() {
        let slangc = find_slangc();
        let probe = Command::new(&slangc)
            .arg("-v")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if probe.is_err() {
            eprintln!("skipping: slangc not runnable ({slangc:?})");
            return;
        }

        let tmp = std::env::temp_dir().join(format!(
            "saffron-codegen-fail-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let slang_path = tmp.join("broken.slang");
        let spv_path = tmp.join("broken.spv");

        let result = write_and_compile(
            &slangc,
            &slang_path,
            &spv_path,
            "this is not valid slang @@@\n",
            None,
            &[],
            "broken test shader",
        );
        assert!(
            matches!(result, Err(Error::SlangcFailed(_))),
            "a non-zero slangc exit is SlangcFailed, got {result:?}"
        );
        assert!(
            !spv_path.exists(),
            "no .spv is produced on a failed compile"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
