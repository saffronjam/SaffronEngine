use std::path::{Path, PathBuf};

use saffron_assets::{
    load_catalog_material_asset_raw, load_plant_family_asset, lower_graph_to_params,
};
use saffron_core::Uuid;
use saffron_protocol::{AppManifest, ExportAppParams, ExportAppResult, Uuid as WireUuid};
use saffron_scene::AssetType;

use crate::error::{Error, Result};
use crate::registry::EngineContext;

/// Cooks the loaded project into a standalone app at `params.output_dir`: pre-bakes every material's
/// mesh SPIR-V so the shipped player never needs `slangc`, then stages the player binary, the
/// project data, the engine shaders, and an `app.json` manifest. macOS gets a native `.app` bundle
/// carrying its Vulkan runtime in `Contents/Frameworks`; other platforms get a flat directory.
pub(crate) fn export_app(
    ctx: &mut EngineContext<'_>,
    params: &ExportAppParams,
) -> Result<ExportAppResult> {
    if params.output_dir.trim().is_empty() {
        return Err(Error::command("missing 'outputDir'"));
    }
    let project_root = ctx.scene_edit.project_root.clone();
    if project_root.is_empty() {
        return Err(Error::command("no project loaded to export"));
    }
    let project_root = PathBuf::from(&project_root);
    let mut warnings: Vec<String> = Vec::new();

    // 1. Pre-bake every material's mesh shader into the project assets (the player loads only the
    //    baked `.spv`; it never invokes `slangc`). Mirrors the `material-cook` command's loop.
    let material_ids: Vec<Uuid> = ctx
        .assets
        .catalog()
        .entries
        .iter()
        .filter(|e| e.asset_type == AssetType::Material)
        .map(|e| e.id)
        .collect();
    for id in material_ids {
        let Ok(raw) = load_catalog_material_asset_raw(ctx.assets, id) else {
            continue;
        };
        if !raw.graph.is_object() || raw.graph.as_object().is_none_or(|g| g.is_empty()) {
            continue; // a factor-only material has no node graph to bake.
        }
        let mut probe = raw.clone();
        if lower_graph_to_params(&raw.graph, &mut probe) {
            continue; // the graph lowers to plain params — no codegen shader needed.
        }
        if let Err(err) = ctx.assets.compile_material_mesh_shader(&raw.graph, id) {
            warnings.push(format!("material {id}: shader bake failed: {err}"));
        }
    }

    // 2. Stage the platform-native application layout. The player binary + engine shaders sit
    //    beside the running host binary in the build tree.
    let layout = ExportLayout::for_output(Path::new(&params.output_dir));
    std::fs::create_dir_all(&layout.resources).map_err(|e| {
        Error::command(format!(
            "create output dir '{}': {e}",
            layout.root.display()
        ))
    })?;
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .ok_or_else(|| Error::command("cannot resolve the engine binary directory"))?;

    copy_file(
        &project_root.join("project.json"),
        &layout.resources.join("project.json"),
    )
    .map_err(|e| Error::command(format!("copy project.json: {e}")))?;
    // The authored vegetation sources stay behind: a runtime binds a cooked generation from the
    // artifact store, and the project loader drops a catalog row whose file is absent.
    copy_dir_filtered(
        &project_root.join("assets"),
        &layout.resources.join("assets"),
        &|path| !is_authored_vegetation(path),
    )
    .map_err(|e| Error::command(format!("copy assets/: {e}")))?;
    let src = project_root.join("src");
    if src.is_dir() {
        copy_dir_recursive(&src, &layout.resources.join("src"))
            .map_err(|e| Error::command(format!("copy src/: {e}")))?;
    }
    let shaders = exe_dir.join("shaders");
    if shaders.is_dir() {
        copy_dir_recursive(&shaders, &layout.resources.join("shaders"))
            .map_err(|e| Error::command(format!("copy shaders/: {e}")))?;
    } else {
        warnings.push(format!("engine shaders not found at {}", shaders.display()));
    }
    let player = exe_dir.join("saffron-player");
    if player.is_file() {
        copy_file(&player, &layout.executable)
            .map_err(|e| Error::command(format!("copy saffron-player: {e}")))?;
    } else {
        warnings.push(format!(
            "saffron-player binary not found at {} (build it before export)",
            player.display()
        ));
    }
    // The cooked vegetation closure. The store lives beside `assets/` rather than inside it, so the
    // asset copy above never carries it — and without it a player binds no manifest and the world
    // comes up bare.
    let maps: Vec<saffron_core::Uuid> = ctx
        .assets
        .catalog()
        .entries
        .iter()
        .filter(|entry| entry.asset_type == AssetType::VegetationMap)
        .map(|entry| entry.id)
        .collect();
    let declared_maps = maps.len();
    let store = ctx.assets.vegetation_artifact_store();
    let state = ctx.assets.vegetation_state_store();
    let closure = saffron_assets::vegetation_export_closure(&store, &state, maps)
        .map_err(|error| Error::command(format!("vegetation export closure: {error}")))?;
    let store_root = store.root().to_path_buf();
    let packaged_root = layout.resources.join("cache").join("vegetation");
    for file in &closure.files {
        copy_file(
            &store_root.join(&file.relative),
            &packaged_root.join(&file.relative),
        )
        .map_err(|error| Error::command(format!("copy {}: {error}", file.relative.display())))?;
    }
    // The durable persistent state lands outside the package's disposable cache, mirroring the
    // project layout the player's asset root derives both roots from.
    let state_root = state.root().to_path_buf();
    let packaged_state_root = layout.resources.join("state").join("vegetation");
    for file in &closure.state_files {
        copy_file(
            &state_root.join(&file.relative),
            &packaged_state_root.join(&file.relative),
        )
        .map_err(|error| Error::command(format!("copy {}: {error}", file.relative.display())))?;
    }
    for map in &closure.maps {
        if map.missing > 0 {
            warnings.push(format!(
                "vegetation map {} names {} artifact(s) the store does not hold; recook before shipping",
                map.map.value(),
                map.missing
            ));
        }
    }
    // A project with no vegetation map ships no vegetation and that is not a warning; a project that
    // HAS one and never cooked it is.
    if declared_maps > 0 && closure.maps.is_empty() {
        warnings.push(format!(
            "{declared_maps} vegetation map(s) have no cooked generation; the package ships no vegetation"
        ));
    }

    // License attribution: every packaged plant source that says it requires attribution, written
    // where a shipped build can show it. A licence obligation that lives only in the editor is an
    // obligation the shipped product breaks.
    let mut attributions: Vec<String> = Vec::new();
    for entry in ctx
        .assets
        .catalog()
        .entries
        .iter()
        .filter(|entry| entry.asset_type == AssetType::Plant)
        .map(|entry| entry.id)
        .collect::<Vec<_>>()
    {
        let Ok(plant) = load_plant_family_asset(ctx.assets, entry) else {
            continue;
        };
        let saffron_vegetation::PlantFamilySource::Imported(recipe) = &plant.source else {
            continue;
        };
        for source in &recipe.sources {
            let provenance = &source.provenance;
            if !provenance.requires_attribution {
                continue;
            }
            let line = format!(
                "{} — {} ({}) — {}",
                plant.name, provenance.attribution, provenance.license_id, provenance.source_uri
            );
            if !attributions.contains(&line) {
                attributions.push(line);
            }
        }
    }
    attributions.sort();
    if !attributions.is_empty() {
        let text = format!("{}\n", attributions.join("\n"));
        std::fs::write(layout.resources.join("ATTRIBUTION.txt"), text)
            .map_err(|error| Error::command(format!("write ATTRIBUTION.txt: {error}")))?;
    }

    let app_json = serde_json::to_string_pretty(&params.app)
        .map_err(|e| Error::command(format!("serialize app.json: {e}")))?;
    std::fs::write(layout.resources.join("app.json"), app_json)
        .map_err(|e| Error::command(format!("write app.json: {e}")))?;
    stage_platform_runtime(&layout, &params.app, &mut warnings)?;

    Ok(ExportAppResult {
        path: layout.root.to_string_lossy().into_owned(),
        warnings,
        vegetation: closure
            .maps
            .iter()
            .map(|map| saffron_protocol::ExportVegetationMapDto {
                map: WireUuid(map.map.value()),
                manifest_identity: map.manifest_identity.clone(),
                plants: map.plants.to_string(),
                cells: map.cells.to_string(),
                missing: map.missing.to_string(),
                baseline: map.baseline,
                macro_plants: map.macro_plants.to_string(),
                facets: map
                    .facet_bytes
                    .iter()
                    .map(|facet| saffron_protocol::ExportVegetationFacetDto {
                        facet: format!("{:?}", facet.kind),
                        cells: facet.cells.to_string(),
                        bytes: facet.bytes.to_string(),
                    })
                    .collect(),
            })
            .collect(),
        vegetation_bytes: closure.total_bytes.to_string(),
        attributions: attributions.len() as u32,
    })
}

/// The platform-native output paths for one exported application.
pub(crate) struct ExportLayout {
    root: PathBuf,
    resources: PathBuf,
    executable: PathBuf,
}

impl ExportLayout {
    fn for_output(output: &Path) -> Self {
        #[cfg(target_os = "macos")]
        {
            let root = if output.extension().is_some_and(|ext| ext == "app") {
                output.to_path_buf()
            } else {
                PathBuf::from(format!("{}.app", output.display()))
            };
            let contents = root.join("Contents");
            Self {
                resources: contents.join("Resources"),
                executable: contents.join("MacOS").join("saffron-player"),
                root,
            }
        }
        #[cfg(not(target_os = "macos"))]
        Self {
            root: output.to_path_buf(),
            resources: output.to_path_buf(),
            executable: output.join("saffron-player"),
        }
    }
}

/// Stages the Linux C++ runtime beside the player, where its `$ORIGIN` rpath resolves it.
#[cfg(not(target_os = "macos"))]
pub(crate) fn stage_platform_runtime(
    layout: &ExportLayout,
    _app: &AppManifest,
    warnings: &mut Vec<String>,
) -> Result<()> {
    for lib in ["libc++.so.1", "libc++abi.so.1"] {
        match find_runtime_lib(lib) {
            Some(src) => copy_file(&src, &layout.root.join(lib))
                .map_err(|e| Error::command(format!("copy {lib}: {e}")))?,
            None => warnings.push(format!(
                "{lib} not found on the host; the exported app needs it beside saffron-player"
            )),
        }
    }
    Ok(())
}

/// Stages a self-contained macOS application bundle with MoltenVK, metadata, its license, and
/// ad-hoc signatures. The player loads the bundled MoltenVK dynamic library directly.
#[cfg(target_os = "macos")]
pub(crate) fn stage_platform_runtime(
    layout: &ExportLayout,
    app: &AppManifest,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let contents = layout.root.join("Contents");
    let frameworks = contents.join("Frameworks");
    let moltenvk = find_macos_runtime_lib("libMoltenVK.dylib")
        .ok_or_else(|| Error::command("macOS Vulkan driver libMoltenVK.dylib not found"))?;
    let bundled_moltenvk = frameworks.join("libMoltenVK.dylib");
    copy_file(&moltenvk, &bundled_moltenvk)
        .map_err(|e| Error::command(format!("copy libMoltenVK.dylib: {e}")))?;

    std::fs::write(contents.join("Info.plist"), macos_info_plist(app))
        .map_err(|e| Error::command(format!("write Info.plist: {e}")))?;
    stage_macos_runtime_licenses(&layout.resources, warnings)?;

    for code in [&bundled_moltenvk, &layout.executable] {
        ad_hoc_sign(code)?;
    }
    ad_hoc_sign(&layout.root)?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn find_macos_runtime_lib(name: &str) -> Option<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(sdk) = std::env::var_os("VULKAN_SDK") {
        dirs.push(PathBuf::from(sdk).join("lib"));
    }
    dirs.extend(
        ["/opt/homebrew/lib", "/usr/local/lib"]
            .into_iter()
            .map(PathBuf::from),
    );
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_info_plist(app: &AppManifest) -> String {
    let title = xml_escape(&app.title);
    let identifier = bundle_identifier_component(&app.title);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>{title}</string>
  <key>CFBundleExecutable</key><string>saffron-player</string>
  <key>CFBundleIdentifier</key><string>com.saffron.anima.{identifier}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>{title}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{}</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#,
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
pub(crate) fn bundle_identifier_component(title: &str) -> String {
    let mut component = String::new();
    let mut separator = false;
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            component.push(ch);
            separator = false;
        } else if !component.is_empty() && !separator {
            component.push('-');
            separator = true;
        }
    }
    while component.ends_with('-') {
        component.pop();
    }
    if component.is_empty() {
        "app".to_owned()
    } else {
        component
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn stage_macos_runtime_licenses(
    resources: &Path,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let licenses = resources.join("licenses");
    for (name, candidates) in [(
        "MoltenVK-LICENSE.txt",
        [
            "/opt/homebrew/opt/molten-vk/LICENSE",
            "/usr/local/opt/molten-vk/LICENSE",
        ],
    )] {
        if let Some(source) = candidates
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
        {
            copy_file(&source, &licenses.join(name))
                .map_err(|e| Error::command(format!("copy {name}: {e}")))?;
        } else {
            warnings.push(format!("license file for {name} not found"));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn ad_hoc_sign(path: &Path) -> Result<()> {
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--timestamp=none"])
        .arg(path)
        .output()
        .map_err(|e| Error::command(format!("run codesign for '{}': {e}", path.display())))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::command(format!(
            "codesign '{}': {}",
            path.display(),
            stderr.trim()
        )))
    }
}

/// Resolves a shared library by SONAME from the usual Linux library directories (honoring a
/// `LD_LIBRARY_PATH` override first), returning the first match — for bundling the C++ runtime
/// into a standalone export.
#[cfg(not(target_os = "macos"))]
pub(crate) fn find_runtime_lib(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("LD_LIBRARY_PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    dirs.extend(["/usr/lib64", "/usr/lib", "/lib64", "/lib"].map(PathBuf::from));
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|p| p.exists())
}

/// Copies one file, creating the destination's parent directory first.
pub(crate) fn copy_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dst)?;
    Ok(())
}

/// Recursively copies a directory tree (files + subdirectories) into `dst`.
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    copy_dir_filtered(src, dst, &|_| true)
}

/// Authored vegetation sources a runtime never reads: it binds a cooked generation from the artifact
/// store and streams the cells that generation names.
pub(crate) const AUTHORED_VEGETATION: [&str; 3] = ["splant", "sbiome", "svegmap"];

/// Whether one path is an authored vegetation source or its sidecar package.
///
/// Excluding these from an exported package is safe because the project loader treats the filesystem
/// as the source of truth and drops a catalog row whose file is absent, and because vegetation binds
/// by identity through the artifact store rather than through the catalog.
pub(crate) fn is_authored_vegetation(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    AUTHORED_VEGETATION.iter().any(|extension| {
        name.ends_with(&format!(".{extension}")) || name.ends_with(&format!(".{extension}.data"))
    })
}

/// Copies a tree, skipping whatever the filter rejects.
pub(crate) fn copy_dir_filtered(
    src: &Path,
    dst: &Path,
    keep: &dyn Fn(&Path) -> bool,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        if !keep(&from) {
            continue;
        }
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_filtered(&from, &to, keep)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
