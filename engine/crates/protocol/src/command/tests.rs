use super::*;
use std::collections::HashSet;

#[test]
fn table_is_frozen_with_ping_first_and_quit_last() {
    // The wire order is the contract; the committed `command-manifest.generated.json` (validated
    // live by the control-schema gate) is the snapshot of the full order. Here we only pin the
    // frozen endpoints — a bare length count adds maintenance friction without catching anything
    // the manifest + the set-equality / partition checks below don't.
    assert_eq!(
        COMMANDS.first().unwrap().name,
        "ping",
        "first command must be `ping`"
    );
    assert_eq!(
        COMMANDS.last().unwrap().name,
        "quit",
        "last command must be `quit`"
    );
}

#[test]
fn help_is_not_a_typed_command() {
    assert!(
        !COMMANDS.iter().any(|c| c.name == HELP_COMMAND),
        "`help` is the untyped reflective builtin and must not be in the typed table"
    );
    assert!(fixture_for(HELP_COMMAND).is_none());
    assert!(skip_for(HELP_COMMAND).is_none());
}

#[test]
fn command_names_are_unique() {
    let mut seen = HashSet::new();
    for c in COMMANDS {
        assert!(seen.insert(c.name), "duplicate command name `{}`", c.name);
    }
}

/// The per-domain first/last names, in the order the six `register_*_commands` files
/// register them — the registration domains the catalog groups by. Each command in the table
/// belongs to exactly one domain, and the domain endpoints match the catalog.
#[test]
fn every_command_belongs_to_a_domain_with_catalog_endpoints() {
    let render = render_domain();
    let scene = scene_domain();
    let asset = asset_domain();
    let animation = animation_domain();
    let physics = physics_domain();
    let vegetation = vegetation_domain();

    // The six registration domains partition
    // the table: every command belongs to exactly one. The real invariant is the coverage
    // (`hits == 1`) + the endpoints below; the manifest snapshot carries the exact counts.
    let domains = [render, scene, asset, animation, physics, vegetation];
    for c in COMMANDS {
        let hits = domains.iter().filter(|d| d.contains(&c.name)).count();
        assert_eq!(
            hits, 1,
            "command `{}` must belong to exactly one domain",
            c.name
        );
    }

    // Catalog endpoints per registration domain.
    assert_eq!(*render.first().unwrap(), "ping");
    assert_eq!(*render.last().unwrap(), "set-viewport-size");
    assert_eq!(*scene.first().unwrap(), "list-entities");
    assert_eq!(*scene.last().unwrap(), "list-probes");
    assert_eq!(*asset.first().unwrap(), "get-project");
    assert_eq!(*asset.last().unwrap(), "quit");
    assert_eq!(*animation.first().unwrap(), "get-animation-state");
    assert_eq!(*animation.last().unwrap(), "list-clip-bindings");
    assert_eq!(*physics.first().unwrap(), "physics-state");
    assert_eq!(*physics.last().unwrap(), "get-ragdoll");
    assert_eq!(*vegetation.first().unwrap(), "vegetation-compile-biome");
    assert_eq!(*vegetation.last().unwrap(), "plant-recook");
}

/// Every command has exactly one of a fixture or a skip, so a new command without
/// contract-test metadata fails the build (and none has both).
#[test]
fn every_command_has_exactly_one_of_fixture_or_skip() {
    for c in COMMANDS {
        let has_fixture = fixture_for(c.name).is_some();
        let has_skip = skip_for(c.name).is_some();
        assert!(
            has_fixture ^ has_skip,
            "command `{}` must have exactly one of a fixture or a skip (fixture={}, skip={})",
            c.name,
            has_fixture,
            has_skip
        );
    }
    // No orphan fixture/skip entries naming a command not in the table.
    let names: HashSet<&str> = COMMANDS.iter().map(|c| c.name).collect();
    for (n, _) in COMMAND_FIXTURES {
        assert!(names.contains(n), "fixture names unknown command `{n}`");
    }
    for (n, _) in COMMAND_SKIPS {
        assert!(names.contains(n), "skip names unknown command `{n}`");
    }
    // (No count assert: the XOR loop above already proves every command has exactly one entry
    // and the orphan loops prove there are no extras, so the total is `COMMANDS.len()` by
    // construction — a hardcoded number would only add maintenance friction.)
}

/// Every command's `params`/`result` type name resolves to a DTO the crate defines — the join
/// the OpenRPC/manifest emitters rely on. A typo'd type name fails here, not at emit time.
#[test]
fn every_command_type_name_resolves_to_a_dto() {
    let dtos: HashSet<&str> = DTO_TYPE_NAMES.iter().copied().collect();
    for c in COMMANDS {
        assert!(
            dtos.contains(c.params),
            "command `{}` params type `{}` is not a DTO",
            c.name,
            c.params
        );
        assert!(
            dtos.contains(c.result),
            "command `{}` result type `{}` is not a DTO",
            c.name,
            c.result
        );
    }
}

fn render_domain() -> &'static [&'static str] {
    &[
        "ping",
        "render-stats",
        "gpu-scene-stats",
        "vegetation-render-stats",
        "profiler.set-mode",
        "pass-timings",
        "profiler.capture-start",
        "profiler.capture-stop",
        "profiler.capture-status",
        "frame-history",
        "get-perf-config",
        "set-perf-config",
        "get-upscale",
        "set-upscale",
        "drain-alarms",
        "list-active-alarms",
        "set-aa",
        "get-taa-params",
        "set-taa-params",
        "set-view-mode",
        "set-clustered",
        "set-ibl",
        "set-sky-occlusion",
        "set-gdf",
        "set-render-quality",
        "get-render-quality",
        "set-tonemap",
        "set-rt-shadows",
        "set-hierarchy-cut",
        "set-mesh-executor",
        "vsm-page-budget",
        "page-request-budget",
        "set-restir",
        "set-ssr",
        "set-rt-reflections",
        "set-gi",
        "set-shadows",
        "set-skinning",
        "set-displacement",
        "set-exposure",
        "set-bloom",
        "set-color-grading",
        "bake-look",
        "set-tessellation-quality",
        "set-depth-prepass",
        "viewport-native-info",
        "set-viewport-power-state",
        "set-viewport-size",
    ]
}

fn scene_domain() -> &'static [&'static str] {
    &[
        "list-entities",
        "list-components",
        "create-entity",
        "destroy-entity",
        "set-parent",
        "add-component",
        "remove-component",
        "set-component-order",
        "set-component",
        "set-transform",
        "set-light",
        "select",
        "pick",
        "query-surface-ray",
        "spatial-cell",
        "spatial-providers",
        "spatial-sample",
        "spatial-residency",
        "inspect",
        "focus",
        "get-world-transform",
        "get-environment",
        "get-environment-defaults",
        "list-environment-profiles",
        "save-environment-profile",
        "update-environment-profile",
        "apply-environment-profile",
        "set-environment",
        "set-atmosphere",
        "set-fog",
        "set-clouds",
        "set-wind",
        "sample-wind",
        "emit-interaction-impulse",
        "wind-interaction-field",
        "set-time-of-day",
        "get-selection",
        "deselect",
        "play",
        "pause",
        "step",
        "stop",
        "get-play-state",
        "get-script-status",
        "get-script-schema",
        "set-script-override",
        "drain-script-errors",
        "drain-script-logs",
        "add-entity",
        "copy-entity",
        "rename-entity",
        "set-component-field",
        "get-camera",
        "set-camera",
        "get-gizmo",
        "set-gizmo",
        "get-debug-overlays",
        "set-debug-overlays",
        "gizmo-pointer",
        "fly-input",
        "script-input",
        "set-probes",
        "recapture-probes",
        "list-probes",
    ]
}

fn asset_domain() -> &'static [&'static str] {
    &[
        "get-project",
        "project-status",
        "cancel-load",
        "new-project",
        "create-script",
        "open-project",
        "import-model",
        "instantiate-model",
        "asset-placement",
        "scan-assets",
        "extract-subasset",
        "clear-extraction",
        "reimport-model",
        "model-info",
        "asset-references",
        "get-asset-model",
        "enter-asset-preview",
        "exit-asset-preview",
        "set-active-view",
        "set-asset-preview-options",
        "clean-assets",
        "delete-unused",
        "import-texture",
        "import-lut",
        "import-vegetation-asset",
        "vegetation-map-layer-commit",
        "vegetation-map-chunk-commit",
        "vegetation-map-chunk-read",
        "list-assets",
        "vegetation-asset-summary",
        "rename-asset",
        "create-asset-folder",
        "rename-asset-folder",
        "delete-asset-folder",
        "move-asset",
        "asset-usages",
        "probe-asset",
        "delete-asset",
        "assign-asset",
        "material-create",
        "material-assign",
        "material-cook",
        "material-compile-graph",
        "material-import",
        "material-list",
        "material-get",
        "material-schema",
        "material-update",
        "preview-render",
        "material-set-graph",
        "material-create-instance",
        "material-set-override",
        "save-scene",
        "load-scene",
        "save-project",
        "get-stores",
        "set-stores",
        "load-project",
        "reload-project",
        "screenshot",
        "get-thumbnail",
        "view-asset",
        "thumbnail-cache",
        "export-app",
        "quit",
    ]
}

fn animation_domain() -> &'static [&'static str] {
    &[
        "get-animation-state",
        "list-clips",
        "play-animation",
        "set-animation-playing",
        "seek-animation",
        "set-animation-loop",
        "stop-preview",
        "get-skeleton-overlay",
        "set-skeleton-overlay",
        "set-skeleton-highlight",
        "pick-skeleton-joint",
        "get-foot-ik",
        "set-foot-ik",
        "set-morph-weights",
        "get-morph-weights",
        "list-clip-bindings",
    ]
}

fn physics_domain() -> &'static [&'static str] {
    &[
        "physics-state",
        "physics-bodies",
        "apply-impulse",
        "fit-collider",
        "drain-contacts",
        "set-kinematic-bones",
        "move-character",
        "raycast",
        "shapecast",
        "enable-ragdoll",
        "set-ragdoll",
        "get-ragdoll",
    ]
}

fn vegetation_domain() -> &'static [&'static str] {
    &[
        "vegetation-compile-biome",
        "vegetation-node-schema",
        "vegetation-preflight-region",
        "vegetation-start-evaluation",
        "vegetation-evaluation-status",
        "vegetation-cancel-evaluation",
        "vegetation-explain-point",
        "vegetation-cook",
        "vegetation-cook-status",
        "vegetation-cancel-cook",
        "vegetation-cell-inspect",
        "vegetation-rejections",
        "vegetation-topology-diff",
        "vegetation-manifest",
        "vegetation-mutate",
        "vegetation-runtime-status",
        "vegetation-runtime-cell",
        "vegetation-runtime-query",
        "vegetation-runtime-inspect",
        "vegetation-nav-contributions",
        "vegetation-drain-events",
        "vegetation-promote",
        "vegetation-fell",
        "vegetation-demote",
        "vegetation-plant-vitals",
        "vegetation-state-export",
        "vegetation-state-import",
        "vegetation-advance-ecology",
        "vegetation-combustion",
        "vegetation-ecology-status",
        "vegetation-ecology-clock",
        "vegetation-usd-skeletons",
        "vegetation-wind-record",
        "vegetation-budgets",
        "vegetation-verify-artifacts",
        "vegetation-state-baseline",
        "vegetation-telemetry",
        "vegetation-network-interest",
        "vegetation-network-checkpoint",
        "vegetation-import-points",
        "vegetation-export-points",
        "plant-create",
        "plant-growth",
        "plant-proxies",
        "plant-season-phenotype",
        "plant-hierarchy",
        "plant-atlas",
        "plant-phenotypes",
        "plant-elements",
        "plant-graph",
        "plant-graph-set",
        "plant-validate",
        "plant-recook",
    ]
}
