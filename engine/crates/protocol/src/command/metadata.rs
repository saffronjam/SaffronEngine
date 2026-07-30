/// The e2e contract-test fixture name for each command that has one. Looked up by command name;
/// fed only to the manifest emitter.
pub static COMMAND_FIXTURES: &[(&str, &str)] = &[
    ("ping", "empty"),
    ("render-stats", "empty"),
    ("gpu-scene-stats", "empty"),
    ("vegetation-render-stats", "empty"),
    ("profiler.set-mode", "profiler-timestamps"),
    ("pass-timings", "empty"),
    ("profiler.capture-start", "capture-single"),
    ("profiler.capture-stop", "empty"),
    ("profiler.capture-status", "empty"),
    ("frame-history", "frame-history-samples"),
    ("get-perf-config", "empty"),
    ("set-perf-config", "perf-config-30"),
    ("get-upscale", "empty"),
    ("set-upscale", "upscale"),
    ("drain-alarms", "alarms-since-0"),
    ("list-active-alarms", "empty"),
    ("set-aa", "aa"),
    ("get-taa-params", "empty"),
    ("set-taa-params", "taa-sharpness"),
    ("set-view-mode", "view-mode-wireframe"),
    ("set-clustered", "toggle-on"),
    ("set-ibl", "toggle-on"),
    ("set-sky-occlusion", "toggle-on"),
    ("set-gdf", "toggle-on"),
    ("set-render-quality", "render-quality"),
    ("get-render-quality", "empty"),
    ("set-tonemap", "tonemap"),
    ("set-rt-shadows", "toggle-off"),
    ("set-hierarchy-cut", "empty"),
    ("vsm-page-budget", "empty"),
    ("page-request-budget", "empty"),
    ("set-restir", "toggle-off"),
    ("set-ssr", "toggle-off"),
    ("set-rt-reflections", "toggle-off"),
    ("set-gi", "gi-off"),
    ("set-shadows", "toggle-on"),
    ("set-skinning", "toggle-on"),
    ("set-displacement", "toggle-on"),
    ("set-depth-prepass", "toggle-on"),
    ("viewport-native-info", "empty"),
    ("list-entities", "empty"),
    ("list-components", "empty"),
    ("create-entity", "new-entity"),
    ("destroy-entity", "temp-entity"),
    ("set-parent", "temp-child-under-cube"),
    ("add-component", "temp-camera-entity"),
    ("remove-component", "temp-camera-component"),
    ("set-component", "cube-name-component"),
    ("set-component-order", "cube-component-order"),
    ("set-transform", "cube-transform"),
    ("set-light", "temp-directional-light"),
    ("select", "cube-entity"),
    ("pick", "viewport-center"),
    ("query-surface-ray", "surface-ray-down"),
    ("spatial-cell", "spatial-origin"),
    ("spatial-providers", "empty"),
    ("spatial-sample", "spatial-sample-cube"),
    ("spatial-residency", "empty"),
    ("inspect", "cube-entity"),
    ("focus", "cube-entity"),
    ("get-world-transform", "cube-entity"),
    ("get-environment", "empty"),
    ("get-environment-defaults", "empty"),
    ("list-environment-profiles", "empty"),
    ("save-environment-profile", "environment-profile-save"),
    ("update-environment-profile", "environment-profile-update"),
    ("apply-environment-profile", "environment-profile-clear-day"),
    ("set-environment", "environment-intensity"),
    ("set-atmosphere", "atmosphere-disabled"),
    ("set-fog", "fog-disabled"),
    ("set-clouds", "clouds-disabled"),
    ("set-wind", "wind-calm"),
    ("sample-wind", "wind-sample-origin"),
    ("emit-interaction-impulse", "interaction-impulse"),
    ("set-time-of-day", "time-of-day-noon"),
    ("get-selection", "empty"),
    ("deselect", "empty"),
    ("play", "empty"),
    ("pause", "empty"),
    ("step", "step-one"),
    ("stop", "empty"),
    ("get-skeleton-overlay", "empty"),
    ("set-skeleton-overlay", "skeleton-overlay-on"),
    ("get-debug-overlays", "empty"),
    ("set-debug-overlays", "debug-overlays-bounds"),
    ("get-play-state", "empty"),
    ("get-script-status", "empty"),
    ("physics-state", "empty"),
    ("physics-bodies", "empty"),
    ("drain-contacts", "alarms-since-0"),
    ("drain-script-errors", "alarms-since-0"),
    ("drain-script-logs", "alarms-since-0"),
    ("get-script-schema", "script-schema-file"),
    ("set-script-override", "script-override-slot"),
    ("add-entity", "cube-preset"),
    ("copy-entity", "cube-entity"),
    ("rename-entity", "cube-rename"),
    ("set-component-field", "cube-name-field"),
    ("get-camera", "empty"),
    ("set-camera", "camera-yaw"),
    ("get-gizmo", "empty"),
    ("set-gizmo", "gizmo-rotate-local"),
    ("gizmo-pointer", "gizmo-hover"),
    ("fly-input", "fly-idle"),
    ("script-input", "script-input-w"),
    ("set-viewport-power-state", "power-state-focused"),
    ("set-viewport-size", "viewport-size"),
    ("set-active-view", "active-view-scene"),
    ("set-probes", "toggle-on"),
    ("recapture-probes", "empty"),
    ("list-probes", "empty"),
    ("set-exposure", "exposure-zero"),
    ("set-bloom", "bloom"),
    ("set-color-grading", "color-grading"),
    ("bake-look", "bake-look"),
    ("set-tessellation-quality", "tess-quality"),
    ("vegetation-node-schema", "empty"),
    ("get-project", "empty"),
    ("project-status", "empty"),
    ("cancel-load", "empty"),
    ("new-project", "new-project"),
    ("open-project", "project-name"),
    ("list-assets", "empty"),
    ("rename-asset", "mesh-asset-rename"),
    ("asset-usages", "mesh-asset"),
    ("probe-asset", "mesh-asset"),
    ("assign-asset", "cube-mesh-asset"),
    ("save-project", "empty"),
    ("get-stores", "empty"),
    ("set-stores", "stores-polyhaven"),
    ("load-project", "project-name"),
    ("get-thumbnail", "mesh-asset"),
    ("view-asset", "mesh-asset-view"),
    ("thumbnail-cache", "thumbnail-cache-stats"),
    ("scan-assets", "empty"),
    ("clean-assets", "empty"),
];

/// The skip reason for each command the e2e cannot fixture (external-input, destructive,
/// side-effecting, or stateful commands). Looked up by command name; fed only to the manifest
/// emitter.
pub static COMMAND_SKIPS: &[(&str, &str)] = &[
    (
        "vegetation-compile-biome",
        "requires an imported biome asset or map-local biome instance",
    ),
    (
        "vegetation-mutate",
        "requires an open vegetation map with authored records",
    ),
    (
        "vegetation-map-layer-commit",
        "requires an imported vegetation map asset",
    ),
    (
        "vegetation-map-chunk-commit",
        "requires an imported vegetation map asset",
    ),
    (
        "vegetation-map-chunk-read",
        "requires an imported vegetation map asset",
    ),
    (
        "vegetation-preflight-region",
        "requires an imported vegetation map and bound biome instance",
    ),
    (
        "vegetation-start-evaluation",
        "requires a prepared vegetation evaluation job",
    ),
    (
        "vegetation-evaluation-status",
        "requires a prior vegetation evaluation job",
    ),
    (
        "vegetation-cancel-evaluation",
        "requires a running vegetation evaluation job",
    ),
    (
        "vegetation-explain-point",
        "requires a completed vegetation evaluation and point identity",
    ),
    (
        "vegetation-cook",
        "requires an imported vegetation map and its exact source dependencies",
    ),
    (
        "vegetation-cook-status",
        "requires a prior vegetation cook job",
    ),
    (
        "vegetation-cancel-cook",
        "requires a running vegetation cook job",
    ),
    (
        "vegetation-cell-inspect",
        "requires a completed manifest and content-addressed cell artifact",
    ),
    (
        "vegetation-rejections",
        "requires a completed manifest and content-addressed cell artifact",
    ),
    (
        "vegetation-topology-diff",
        "requires two completed manifests to compare",
    ),
    (
        "vegetation-manifest",
        "requires a completed vegetation cook",
    ),
    (
        "vegetation-runtime-status",
        "requires an enabled VegetationField and completed vegetation cook",
    ),
    (
        "vegetation-runtime-cell",
        "requires a CPU-resident cooked vegetation cell",
    ),
    (
        "vegetation-runtime-query",
        "requires CPU-resident cooked macro vegetation",
    ),
    (
        "vegetation-runtime-inspect",
        "requires a resident plant or persistent plant delta",
    ),
    (
        "vegetation-nav-contributions",
        "requires a navigation-resident cooked vegetation cell",
    ),
    (
        "vegetation-drain-events",
        "requires an exact bound vegetation runtime generation",
    ),
    (
        "vegetation-promote",
        "requires a live play world and a resident macro plant",
    ),
    (
        "vegetation-demote",
        "requires a live play world and a promoted macro plant",
    ),
    (
        "vegetation-fell",
        "requires a live play world and a resident macro plant",
    ),
    (
        "vegetation-state-export",
        "requires an exact bound vegetation runtime generation",
    ),
    (
        "vegetation-state-import",
        "requires an exact bound vegetation runtime generation and snapshot",
    ),
    (
        "vegetation-advance-ecology",
        "requires an exact bound vegetation runtime generation",
    ),
    (
        "vegetation-ecology-status",
        "requires an exact bound vegetation runtime generation",
    ),
    (
        "vegetation-ecology-clock",
        "requires an exact bound vegetation runtime generation",
    ),
    (
        "vegetation-combustion",
        "requires an exact bound vegetation runtime generation",
    ),
    ("vegetation-usd-skeletons", "requires a USD stage on disk"),
    (
        "vegetation-wind-record",
        "requires a mirrored resident plant and at least one rendered frame",
    ),
    (
        "vegetation-budgets",
        "reads and rewrites live mirror budgets",
    ),
    ("vegetation-verify-artifacts", "requires a loaded project"),
    (
        "vegetation-state-baseline",
        "requires a bound vegetation runtime",
    ),
    (
        "vegetation-telemetry",
        "requires a bound vegetation runtime",
    ),
    (
        "vegetation-import-points",
        "requires an authored vegetation map and a readable point file",
    ),
    (
        "vegetation-export-points",
        "requires an authored vegetation map with anchors",
    ),
    (
        "plant-create",
        "creates a catalog asset in the loaded project",
    ),
    (
        "plant-growth",
        "requires an authored native plant-family asset",
    ),
    (
        "plant-graph",
        "requires an authored native plant-family asset",
    ),
    (
        "plant-elements",
        "requires an authored native plant-family asset",
    ),
    (
        "plant-phenotypes",
        "requires an authored plant-family asset in the loaded project",
    ),
    (
        "plant-atlas",
        "requires a cooked plant family carrying a packed coverage atlas",
    ),
    (
        "plant-hierarchy",
        "requires a cooked plant family in the loaded project",
    ),
    (
        "plant-season-phenotype",
        "requires an authored plant-family asset in the loaded project",
    ),
    (
        "plant-proxies",
        "requires an authored plant-family asset in the loaded project",
    ),
    (
        "plant-graph-set",
        "requires an authored native plant-family asset and a graph document",
    ),
    ("plant-validate", "requires an imported plant-family asset"),
    (
        "plant-recook",
        "requires an imported plant family and its retained source recipe",
    ),
    ("import-model", "requires an external model fixture path"),
    (
        "instantiate-model",
        "requires a model asset id from a prior import",
    ),
    (
        "asset-placement",
        "requires a model asset id from a prior import",
    ),
    (
        "extract-subasset",
        "requires a model + sub-asset id from a prior import",
    ),
    (
        "clear-extraction",
        "requires an extracted sub-asset from a prior import",
    ),
    (
        "reimport-model",
        "requires a model asset id from a prior import",
    ),
    (
        "model-info",
        "requires a model asset id from a prior import",
    ),
    (
        "asset-references",
        "requires an asset id from a prior import",
    ),
    (
        "fit-collider",
        "needs an entity with a Collider + a resolvable mesh — covered in make e2e",
    ),
    (
        "set-kinematic-bones",
        "needs an imported rig — covered in make e2e",
    ),
    (
        "set-morph-weights",
        "needs an imported morph mesh — covered in make e2e",
    ),
    (
        "get-morph-weights",
        "needs an imported morph mesh — covered in make e2e",
    ),
    (
        "get-foot-ik",
        "needs a rigged entity with foot-IK state — covered in make e2e",
    ),
    (
        "set-foot-ik",
        "needs a rigged entity with foot-IK state — covered in make e2e",
    ),
    (
        "list-clip-bindings",
        "needs an imported clip + entity forest — covered in make e2e",
    ),
    (
        "move-character",
        "needs a character entity in play — covered in make e2e",
    ),
    (
        "raycast",
        "needs a live physics world (play) — covered in make e2e",
    ),
    (
        "shapecast",
        "needs a live physics world (play) — covered in make e2e",
    ),
    (
        "apply-impulse",
        "needs a live physics world (play) — covered in make e2e",
    ),
    (
        "enable-ragdoll",
        "needs a rigged entity in play — covered in make e2e",
    ),
    (
        "set-ragdoll",
        "needs a live ragdoll on a rig in play — covered in make e2e",
    ),
    (
        "get-ragdoll",
        "needs a rigged entity in play — covered in make e2e",
    ),
    (
        "get-asset-model",
        "needs an imported model — covered in make e2e",
    ),
    (
        "enter-asset-preview",
        "needs an imported model — covered in make e2e",
    ),
    (
        "exit-asset-preview",
        "needs an active asset preview — covered in make e2e",
    ),
    (
        "set-skeleton-highlight",
        "needs an active asset preview — covered in make e2e",
    ),
    (
        "pick-skeleton-joint",
        "needs an active rigged asset preview — covered in make e2e",
    ),
    (
        "set-asset-preview-options",
        "needs an active asset preview — covered in make e2e",
    ),
    (
        "delete-unused",
        "destructive: requires confirmed-unused asset ids",
    ),
    (
        "import-texture",
        "requires an external texture fixture path",
    ),
    ("import-lut", "requires an external .cube fixture path"),
    (
        "import-vegetation-asset",
        "requires an external authored vegetation asset path",
    ),
    (
        "vegetation-asset-summary",
        "needs an imported vegetation asset",
    ),
    ("create-asset-folder", "mutates the project asset catalog"),
    ("rename-asset-folder", "mutates the project asset catalog"),
    ("delete-asset-folder", "mutates the project asset catalog"),
    ("move-asset", "mutates the project asset catalog"),
    ("delete-asset", "removes a project asset"),
    (
        "create-script",
        "writes a script file into the project src/",
    ),
    ("save-scene", "writes a scene file"),
    ("material-create", "writes a .smat material file"),
    ("material-assign", "needs a created material asset"),
    ("material-import", "requires an external texture folder"),
    ("material-list", "lists project material assets"),
    ("material-get", "needs a created material asset"),
    ("material-schema", "needs a created material asset"),
    ("material-update", "needs a created material asset"),
    ("preview-render", "renders a material to a PNG blob"),
    ("material-set-graph", "needs a created material asset"),
    (
        "material-create-instance",
        "needs a created parent material",
    ),
    ("material-set-override", "needs a created material asset"),
    (
        "material-compile-graph",
        "needs a created material with a graph",
    ),
    (
        "material-cook",
        "compiles all codegen materials; side-effecting, exercised by e2e",
    ),
    ("load-scene", "loads and replaces the scene from a file"),
    (
        "reload-project",
        "reloads and replaces the active project's scene and catalog",
    ),
    ("screenshot", "writes an image file and can be deferred"),
    ("quit", "terminates the host process"),
    (
        "get-animation-state",
        "needs a rigged entity with an animation player (covered by the e2e)",
    ),
    (
        "list-clips",
        "needs a project with imported animation clips (covered by the e2e)",
    ),
    (
        "play-animation",
        "needs a rigged entity + an imported clip (covered by the e2e)",
    ),
    (
        "set-animation-playing",
        "needs a rigged entity with an animation player (covered by the e2e)",
    ),
    (
        "seek-animation",
        "needs a rigged entity with an animation player (covered by the e2e)",
    ),
    (
        "set-animation-loop",
        "needs a rigged entity with an animation player (covered by the e2e)",
    ),
    (
        "stop-preview",
        "needs a rigged entity with an animation player (covered by the e2e)",
    ),
    (
        "export-app",
        "writes a staged app folder to disk from a loaded project (covered by the e2e)",
    ),
];

/// Looks up the fixture name for a command, if it has one.
pub fn fixture_for(name: &str) -> Option<&'static str> {
    COMMAND_FIXTURES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
}

/// Looks up the skip reason for a command, if it has one.
pub fn skip_for(name: &str) -> Option<&'static str> {
    COMMAND_SKIPS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
}
