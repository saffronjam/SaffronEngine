+++
title = 'Control commands'
weight = 7
math = false
+++

# Control commands

The control plane exposes 223 typed commands over its local Unix socket. This table follows the frozen order in `saffron_protocol::COMMANDS`; the generated OpenRPC methods use the same names, parameter DTOs, and result DTOs.

`register_builtin_commands` installs `ping`, the reflective `help` command, and the render, scene, animation, physics, and asset handlers. The host adds `get-script-schema` because its handler depends on the script crate. A registry test compares the registered names with `COMMANDS` as sets, with that host-owned command accounted for explicitly.

## Calling a command

A request is one newline-terminated JSON object. The response echoes `id` and contains either `result` or `error`.

```json
{"id": 1, "cmd": "set-transform", "params": {"entity": "42", "translation": {"x": 0, "y": 1, "z": 0}}}
{"id": 1, "ok": true, "result": {"id": "42", "name": "Box"}}
```

The `sa` form accepts named flags or positional values. Named flags use the DTO's camelCase field names; positional values follow field declaration order.

```sh
sa -o json set-transform --entity 42 --translation '{"x":0,"y":1,"z":0}'
```

`EmptyParams` means `{}`. The parameter and result names below refer to schemas under `components.schemas` in `schemas/control/openrpc.generated.json`. Entity and asset IDs are `u64` values encoded as decimal JSON strings; see the [shared type contract](../../explanations/tooling-and-control/shared-types/).

## Reflective command

`help` is the only untyped command and therefore does not appear in `COMMANDS` or the OpenRPC methods.

| Command | Params | Result | Purpose |
|---|---|---|---|
| `help` | `{}` | `{commands: [{name, help}]}` | List the live registry in registration order. |

## Typed command inventory

| Command | Params | Result | Purpose |
|---|---|---|---|
| `ping` | `PingParams` | `PingResult` | liveness + engine info |
| `render-stats` | `EmptyParams` | `RenderStatsDto` | last frame draw counters + the `vsm` shadow-page activity block |
| `gpu-scene-stats` | `EmptyParams` | `GpuSceneMirrorStatsDto` | persistent GPU-scene mirror population and rebuild counters |
| `vegetation-mutate` | `VegetationMutateParams` | `VegetationMutateResult` | apply typed vegetation mutations (tombstone/override/anchor/…) through the reducer |
| `vegetation-render-stats` | `EmptyParams` | `VegetationRenderStatsDto` | per-family and per-cell vegetation render population plus page faults |
| `profiler.set-mode` | `ProfilerSetModeParams` | `ProfilerModeResult` | set the GPU profiler mode |
| `pass-timings` | `EmptyParams` | `RenderPassTimingsDto` | last frame per-pass GPU timings |
| `profiler.capture-start` | `CaptureStartParams` | `CaptureStartResult` | arm a bounded profiler capture |
| `profiler.capture-stop` | `EmptyParams` | `CaptureStopResult` | finish + return the armed profiler capture |
| `profiler.capture-status` | `EmptyParams` | `CaptureStatusResult` | non-destructive capture progress |
| `frame-history` | `FrameHistoryParams` | `FrameHistoryDto` | frame-time percentiles + stutter count |
| `get-perf-config` | `EmptyParams` | `PerfConfigDto` | shared frame-budget / threshold config |
| `set-perf-config` | `SetPerfConfigParams` | `PerfConfigDto` | set the frame budget + thresholds |
| `get-upscale` | `EmptyParams` | `GetUpscaleResult` | TAAU ratio, dynamic-resolution state, and input/display extents |
| `set-upscale` | `SetUpscaleParams` | `SetUpscaleResult` | set the TAAU input:display ratio + dynamic resolution |
| `drain-alarms` | `DrainAlarmsParams` | `DrainAlarmsResult` | drain perf-alarm events (seq cursor) |
| `list-active-alarms` | `EmptyParams` | `ActiveAlarmsDto` | active perf alarms |
| `set-aa` | `SetAaParams` | `SetAaResult` | set anti-aliasing mode |
| `get-taa-params` | `EmptyParams` | `GetTaaParamsResult` | current TAA blend/sharpen parameters |
| `set-taa-params` | `SetTaaParamsParams` | `SetTaaParamsResult` | tune TAA blend/sharpen parameters (partial update) |
| `set-view-mode` | `SetViewModeParams` | `SetViewModeResult` | set the debug render-output mode {lit\|wireframe\|albedo\|normal\|roughness\|metallic\|emissive} |
| `set-clustered` | `ToggleParams` | `SetClusteredResult` | toggle clustered lighting |
| `set-ibl` | `ToggleParams` | `SetIblResult` | toggle image-based lighting |
| `set-sky-occlusion` | `ToggleParams` | `SetSkyOcclusionResult` | occlude the reflected skybox with the Global SDF reflection-occlusion cone |
| `set-gdf` | `ToggleParams` | `SetGdfResult` | composite per-mesh SDFs into the Global Distance Field cascade clipmap |
| `set-render-quality` | `SetRenderQualityParams` | `RenderQualityResult` | set the render-quality tier (low/medium/high/ultra) — the SSGI/GTAO/contact knob |
| `get-render-quality` | `EmptyParams` | `RenderQualityResult` | the active render-quality tier + resolved per-effect state |
| `set-tonemap` | `SetTonemapParams` | `TonemapResult` | set the tonemap operator (reinhard/aces/agx/pbr-neutral) |
| `set-rt-shadows` | `ToggleParams` | `SetRtShadowsResult` | toggle ray-traced shadows |
| `set-restir` | `ToggleParams` | `SetRestirResult` | toggle ReSTIR |
| `set-ssr` | `ToggleParams` | `SetSsrResult` | toggle screen-space reflections |
| `set-rt-reflections` | `ToggleParams` | `SetRtReflectionsResult` | toggle ray-traced reflections |
| `set-gi` | `SetGiParams` | `SetGiResult` | set GI mode |
| `set-shadows` | `ToggleParams` | `SetShadowsResult` | toggle shadows |
| `set-skinning` | `ToggleParams` | `SetSkinningResult` | toggle GPU skinning |
| `set-displacement` | `ToggleParams` | `SetDisplacementResult` | toggle GPU displacement |
| `set-depth-prepass` | `ToggleParams` | `SetDepthPrepassResult` | toggle depth prepass |
| `viewport-native-info` | `EmptyParams` | `ViewportNativeInfoResult` | native viewport bridge status |
| `set-viewport-power-state` | `SetViewportPowerStateParams` | `ViewportPowerStateResult` | set the editor viewport visibility (focused/unfocused/occluded) for idle throttling |
| `set-viewport-size` | `SetViewportSizeParams` | `SetViewportSizeResult` | set the offscreen render size |
| `list-entities` | `EmptyParams` | `EntityList` | list all entities |
| `list-components` | `EmptyParams` | `ComponentList` | list registered component types |
| `create-entity` | `CreateEntityParams` | `EntityRef` | create-entity {name} |
| `destroy-entity` | `EntityParams` | `DestroyEntityResult` | destroy-entity {entity} |
| `set-parent` | `SetParentParams` | `EntityRef` | set-parent {entity, parent?} — reparent (absent/0 parent detaches to root) |
| `add-component` | `ComponentParams` | `AddComponentResult` | add-component {entity, component} |
| `remove-component` | `ComponentParams` | `RemoveComponentResult` | remove-component {entity, component} |
| `set-component` | `SetComponentParams` | `SetComponentResult` | set-component {entity, component, json} |
| `set-component-order` | `SetComponentOrderParams` | `SetComponentOrderResult` | set-component-order {entity, components} |
| `set-transform` | `SetTransformParams` | `EntityRef` | set-transform {entity, translation?, rotation?, scale?} |
| `set-light` | `SetLightParams` | `EntityRef` | set-light {entity?, direction?, color?, intensity?, ambient?} |
| `select` | `EntityParams` | `EntityRef` | select {entity} |
| `pick` | `PickParams` | `PickResult` | pick {u=0.5, v=0.5} |
| `query-surface-ray` | `QuerySurfaceRayParams` | `SurfaceRayResult` | nearest scene-surface hit |
| `spatial-cell` | `SpatialCellParams` | `SpatialCellResult` | canonical position ownership and ancestor-cell conversion |
| `spatial-providers` | `EmptyParams` | `SurfaceProvidersResult` | list live surface providers and capabilities |
| `spatial-sample` | `SpatialSampleParams` | `SpatialSampleResult` | sample one canonical provider field channel |
| `spatial-residency` | `EmptyParams` | `SpatialResidencyResult` | list residency sources and per-facet cell references |
| `inspect` | `EntityParams` | `InspectResult` | inspect {entity} |
| `focus` | `EntityParams` | `EntityRef` | focus {entity} |
| `get-world-transform` | `EntityParams` | `WorldTransformResult` | get-world-transform {entity} — the entity's composed world translation + scale |
| `get-environment` | `EmptyParams` | `EnvironmentDto` | get environment settings |
| `get-environment-defaults` | `EmptyParams` | `EnvironmentDto` | get the canonical environment defaults |
| `list-environment-profiles` | `EmptyParams` | `EnvironmentProfileListDto` | list built-in and project environment profiles |
| `save-environment-profile` | `SaveEnvironmentProfileParams` | `EnvironmentProfileSummaryDto` | save the active environment as a project profile |
| `update-environment-profile` | `UpdateEnvironmentProfileParams` | `EnvironmentProfileSummaryDto` | replace a project profile with the active environment |
| `apply-environment-profile` | `ApplyEnvironmentProfileParams` | `EnvironmentDto` | apply a complete environment profile |
| `set-environment` | `SetEnvironmentParams` | `EnvironmentDto` | set environment settings |
| `set-atmosphere` | `SetAtmosphereParams` | `EnvironmentDto` | set procedural-atmosphere settings |
| `set-fog` | `SetFogParams` | `EnvironmentDto` | set analytic height & distance fog settings |
| `set-clouds` | `SetCloudsParams` | `EnvironmentDto` | set volumetric cloud shape settings |
| `set-wind` | `SetWindParams` | `EnvironmentDto` | set shared global wind settings |
| `sample-wind` | `SampleWindParams` | `SampleWindResult` | the composed wind velocity at a world position {positionM, timeS?} |
| `emit-interaction-impulse` | `EmitInteractionImpulseParams` | `EmitInteractionImpulseResult` | push the world interaction field {positionM, radiusM, strength, direction?, depress?} |
| `set-time-of-day` | `SetTimeOfDayParams` | `EnvironmentDto` | set calendar-driven time-of-day settings |
| `get-selection` | `EmptyParams` | `SelectionResult` | get current selection |
| `deselect` | `EmptyParams` | `DeselectResult` | clear selection |
| `play` | `EmptyParams` | `PlayStateResult` | enter or resume play mode |
| `pause` | `EmptyParams` | `PlayStateResult` | pause the running scene |
| `step` | `StepParams` | `PlayStateResult` | step {frames=1} while paused |
| `stop` | `EmptyParams` | `PlayStateResult` | stop play and restore the authored scene |
| `get-play-state` | `EmptyParams` | `PlayStateResult` | current play state |
| `get-animation-state` | `AnimationStateParams` | `AnimationStateResult` | a rig's playhead, clip, wrap, and speed |
| `list-clips` | `ListClipsParams` | `ListClipsResult` | the animation clips in the project catalog |
| `play-animation` | `PlayAnimationParams` | `AnimationStateResult` | play a clip on a rig (previews in Edit too) |
| `set-animation-playing` | `SetAnimationPlayingParams` | `AnimationStateResult` | resume or pause without moving the playhead |
| `seek-animation` | `SeekAnimationParams` | `AnimationStateResult` | set the playhead (previews in Edit) |
| `set-animation-loop` | `SetAnimationLoopParams` | `AnimationStateResult` | set the wrap mode (once\|loop\|pingpong) |
| `stop-preview` | `AnimationStateParams` | `AnimationStateResult` | clear the Edit preview and stop (revert to rest) |
| `get-skeleton-overlay` | `EmptyParams` | `SkeletonOverlayResult` | the line-skeleton overlay toggle, axes, and joint size |
| `set-skeleton-overlay` | `SetSkeletonOverlayParams` | `SkeletonOverlayResult` | the selected rig's line-skeleton viewport overlay (show\|axes\|jointSize) |
| `get-debug-overlays` | `EmptyParams` | `DebugOverlaysResult` | the viewport debug-overlay toggles (bounds\|sceneAabb\|lightVolumes\|grid\|colliders) |
| `set-debug-overlays` | `DebugOverlaysParams` | `DebugOverlaysResult` | toggle viewport debug overlays {bounds?, sceneAabb?, lightVolumes?, grid?, colliders?, vegetationCells?, vegetationBounds?, vegetationRejections?, vegetationHeatmap?} |
| `set-skeleton-highlight` | `SetSkeletonHighlightParams` | `SkeletonOverlayResult` | tint a previewed model's joint by its get-asset-model node index (-1 clears) |
| `pick-skeleton-joint` | `PickSkeletonJointParams` | `PickSkeletonJointResult` | pick the previewed model's nearest joint to a viewport click (u,v) within radiusPx |
| `set-asset-preview-options` | `SetAssetPreviewOptionsParams` | `AssetPreviewOptionsResult` | set-asset-preview-options {floor?} — preview-scene settings (show floor) |
| `get-foot-ik` | `GetFootIkParams` | `FootIkResult` | a rig's foot-IK enable, ground height, and chain count |
| `set-foot-ik` | `SetFootIkParams` | `FootIkResult` | toggle a rig's kinematic foot IK (enabled\|groundHeight) |
| `set-morph-weights` | `SetMorphWeightsParams` | `MorphWeightsResult` | set a morph mesh's blend-shape weights (canonical 0..1) |
| `get-morph-weights` | `GetMorphWeightsParams` | `MorphWeightsResult` | a morph mesh's live blend-shape weights + target names |
| `list-clip-bindings` | `ListClipBindingsParams` | `ClipBindingsResult` | a clip's channels resolved against a live entity forest |
| `get-script-status` | `EmptyParams` | `ScriptStatusResult` | play state, live script instances, error high-water |
| `physics-state` | `EmptyParams` | `PhysicsStateResult` | live physics world summary (active, body + dynamic counts) |
| `physics-bodies` | `EmptyParams` | `PhysicsBodiesResult` | every live body's entity, motion, active state, and world position |
| `fit-collider` | `FitColliderParams` | `FitColliderResult` | re-fit a Collider's shape to the entity's mesh AABB |
| `apply-impulse` | `ApplyImpulseParams` | `ApplyImpulseResult` | push a Dynamic rigidbody (returns its new velocity) |
| `drain-contacts` | `DrainContactsParams` | `DrainContactsResult` | drain contact/trigger events (seq cursor) |
| `set-kinematic-bones` | `SetKinematicBonesParams` | `KinematicBonesResult` | toggle a rig's kinematic-bone physics |
| `move-character` | `MoveCharacterParams` | `MoveCharacterResult` | set a character controller's desired walk velocity |
| `raycast` | `RaycastParams` | `RaycastResult` | closest physics ray hit (entity/point/normal/distance) |
| `shapecast` | `ShapecastParams` | `RaycastResult` | closest sphere-sweep physics hit |
| `enable-ragdoll` | `EnableRagdollParams` | `RagdollResult` | go limp / restore animation on a rig's powered ragdoll |
| `set-ragdoll` | `SetRagdollParams` | `RagdollResult` | drive a rig's active-ragdoll blend (motors, body/bone weight) |
| `get-ragdoll` | `GetRagdollParams` | `RagdollResult` | a rig's ragdoll presence, active flag, and mean blend weight |
| `drain-script-errors` | `DrainScriptErrorsParams` | `DrainScriptErrorsResult` | drain script errors (seq cursor) |
| `drain-script-logs` | `DrainScriptLogsParams` | `DrainScriptLogsResult` | drain sa.log lines (seq cursor) |
| `get-script-schema` | `GetScriptSchemaParams` | `GetScriptSchemaResult` | a project script's declared fields |
| `set-script-override` | `SetScriptOverrideParams` | `SetScriptOverrideResult` | write one per-instance script field override |
| `add-entity` | `AddEntityParams` | `EntityRef` | add-entity {preset} |
| `copy-entity` | `EntityParams` | `EntityRef` | copy-entity {entity} |
| `rename-entity` | `RenameEntityParams` | `EntityRef` | rename-entity {entity, name} |
| `set-component-field` | `SetComponentFieldParams` | `SetComponentFieldResult` | set-component-field {entity, component, field, value} |
| `get-camera` | `EmptyParams` | `EditorCamera` | get camera |
| `set-camera` | `SetCameraParams` | `EditorCamera` | set camera |
| `get-gizmo` | `EmptyParams` | `GizmoState` | get gizmo |
| `set-gizmo` | `SetGizmoParams` | `GizmoState` | set gizmo |
| `gizmo-pointer` | `GizmoPointerParams` | `GizmoPointerResult` | drive gizmo pointer |
| `fly-input` | `FlyInputParams` | `FlyInputResult` | stream editor fly-cam input |
| `script-input` | `ScriptInputParams` | `ScriptInputResult` | set Lua gameplay key state |
| `set-probes` | `SetProbesParams` | `SetProbesResult` | toggle reflection-probe sampling |
| `recapture-probes` | `EmptyParams` | `RecaptureProbesResult` | mark reflection probes dirty |
| `list-probes` | `EmptyParams` | `ListProbesResult` | list captured reflection probes |
| `set-exposure` | `SetExposureParams` | `SetExposureResult` | set-exposure {ev} |
| `set-bloom` | `SetBloomParams` | `SetBloomResult` | set-bloom {enabled} {intensity} {scatter} {tint} {threshold} |
| `set-color-grading` | `SetColorGradingParams` | `SetColorGradingResult` | set-color-grading {temperature} {tint} {contrast} {pivot} {saturation} {slope} {offset} {power} |
| `bake-look` | `BakeLookParams` | `BakeLookResult` | bake-look [name] — fold grade + view transform + creative LUT into a 33³ .slut |
| `set-tessellation-quality` | `SetTessellationQualityParams` | `SetTessellationQualityResult` | set-tessellation-quality [factorCap] [minFactor] [edgeLengthTarget] |
| `vegetation-compile-biome` | `VegetationCompileBiomeParams` | `VegetationCompileBiomeResult` | compile a biome graph and inspect dependencies, halo, estimates, and hard caps |
| `vegetation-node-schema` | `VegetationNodeSchemaParams` | `VegetationNodeSchemaResult` | inspect typed biome-node pins, parameters, seed namespaces, and execution capability |
| `vegetation-preflight-region` | `VegetationPreflightRegionParams` | `VegetationEvaluationJobDto` | bound and retain one evaluation; report retained/generated inputs and both memory peaks |
| `vegetation-start-evaluation` | `VegetationEvaluationJobParams` | `VegetationEvaluationJobDto` | start the exact evaluator and inputs retained by a prepared job |
| `vegetation-evaluation-status` | `VegetationEvaluationJobParams` | `VegetationEvaluationStatusDto` | poll an asynchronous vegetation evaluation and its deterministic aggregate |
| `vegetation-cancel-evaluation` | `VegetationEvaluationJobParams` | `VegetationEvaluationStatusDto` | cancel an asynchronous vegetation evaluation without partial publication |
| `vegetation-explain-point` | `VegetationExplainPointParams` | `ProvenanceExplanationDto` | trace an accepted plant or rejected candidate through its provenance decision DAG |
| `vegetation-cook` | `VegetationCookParams` | `VegetationCookJobDto` | start a staged content-addressed map cook for all, bounds, or explicit cells |
| `vegetation-cook-status` | `VegetationCookJobParams` | `VegetationCookStatusDto` | poll cook progress, terminal statistics, manifest, or failure |
| `vegetation-cancel-cook` | `VegetationCookJobParams` | `VegetationCookStatusDto` | request cooperative cancellation of one vegetation cook |
| `vegetation-cell-inspect` | `VegetationCellInspectParams` | `VegetationCellInspectResult` | validate and inspect one immutable cell header and section directory |
| `vegetation-rejections` | `VegetationRejectionsParams` | `VegetationRejectionsResult` | one cooked cell's rejected candidates: position, reason, and ordinal (capped rows) |
| `vegetation-topology-diff` | `VegetationTopologyDiffParams` | `VegetationTopologyDiffResult` | diff two cooked manifests per cell: added/removed/moved plants + override conflicts |
| `vegetation-manifest` | `VegetationManifestParams` | `VegetationManifestResult` | inspect the current or an exact immutable generation manifest |
| `vegetation-runtime-status` | `EmptyParams` | `VegetationRuntimeStatusDto` | report the exact runtime vegetation generation, state, queues, residency, and budgets |
| `vegetation-runtime-cell` | `VegetationRuntimeCellParams` | `VegetationRuntimeCellResult` | inspect one immutable CPU-resident vegetation cell generation |
| `vegetation-runtime-query` | `VegetationRuntimeQueryParams` | `VegetationRuntimeQueryResult` | query CPU-resident macro vegetation by bounds, radius, ray, or nearest |
| `vegetation-runtime-inspect` | `VegetationRuntimePlantInspectParams` | `VegetationRuntimePlantInspectResult` | inspect one stable plant's effective row, persistent state, and resident provenance |
| `vegetation-state-export` | `EmptyParams` | `VegetationStateSnapshotDto` | export the canonical strict runtime vegetation state snapshot |
| `vegetation-state-import` | `VegetationStateImportParams` | `VegetationStateSnapshotDto` | verify and atomically import one exact runtime vegetation state snapshot |
| `plant-validate` | `PlantValidateParams` | `PlantValidationResult` | validate one retained plant source recipe without publication |
| `plant-recook` | `PlantRecookParams` | `PlantRecookResult` | compile and publish one validated plant-family artifact |
| `get-project` | `EmptyParams` | `ProjectInfoDto` | active project metadata |
| `project-status` | `EmptyParams` | `ProjectStatusDto` | project-load phase + progress |
| `cancel-load` | `EmptyParams` | `ProjectStatusDto` | abort the in-flight project load |
| `new-project` | `NewProjectParams` | `ProjectStatusDto` | new-project {name} |
| `create-script` | `CreateScriptParams` | `CreateScriptResult` | boilerplate .lua under the project src/ |
| `open-project` | `PathParams` | `ProjectStatusDto` | open-project {path} |
| `import-model` | `ImportModelParams` | `ImportModelResult` | import-model {path} — optional store attribution |
| `instantiate-model` | `InstantiateModelParams` | `EntityRef` | instantiate-model {asset} [name] |
| `asset-placement` | `AssetPlacementParams` | `AssetPlacementResult` | asset-placement {phase, asset?, u?, v?} — preview, commit, or clear a viewport model drop |
| `import-texture` | `ImportTextureParams` | `ImportTextureResult` | import-texture {path} [colorspace] |
| `import-lut` | `ImportLutParams` | `ImportLutResult` | import-lut {path} — import a creative .cube look as a LUT asset |
| `import-vegetation-asset` | `ImportVegetationAssetParams` | `ImportVegetationAssetResult` | import an authored plant, biome, or complete vegetation-map package |
| `list-assets` | `EmptyParams` | `AssetList` | list project asset catalog |
| `vegetation-map-layer-commit` | `VegetationMapLayerCommitParams` | `VegetationMapLayerCommitResult` | commit one optimistic authored-map layer transaction (upserts + removals) |
| `vegetation-map-chunk-commit` | `VegetationMapChunkCommitParams` | `VegetationMapChunkCommitResult` | commit one optimistic authored-map chunk transaction (a brush gesture's tiles + anchors) |
| `vegetation-map-chunk-read` | `VegetationMapChunkReadParams` | `VegetationMapChunkReadResult` | read authored map chunks by logical key (a brush gesture's read-modify-write baseline) |
| `vegetation-asset-summary` | `VegetationAssetSummaryParams` | `VegetationAssetSummaryResult` | inspect authored vegetation metadata, validation, dependencies, and cook statistics |
| `scan-assets` | `EmptyParams` | `ScanAssetsResult` | rescan assets/ and reconcile the catalog from disk |
| `extract-subasset` | `ExtractSubAssetParams` | `AssetRef` | extract-subasset {asset, subAsset} [dest] — slice an embedded sub-asset to a standalone file |
| `clear-extraction` | `ClearExtractionParams` | `AssetRef` | clear-extraction {asset, subAsset} — revert an extracted sub-asset to the embedded chunk |
| `reimport-model` | `ReimportModelParams` | `ReimportModelResult` | reimport-model {asset} — re-bake from source (skip if unchanged), preserving extractions |
| `model-info` | `ModelInfoParams` | `ModelInfoResult` | model-info {asset} — a container's sub-assets, source recipe, and byte footprint |
| `asset-references` | `AssetReferencesParams` | `AssetReferencesResult` | asset-references {asset} — what references this / what this references + footprint |
| `get-asset-model` | `GetAssetModelParams` | `AssetModelResult` | get-asset-model {asset} — a model's capabilities + bone tree + clips, from its .smodel container |
| `enter-asset-preview` | `EnterAssetPreviewParams` | `AssetPreviewResult` | enter-asset-preview {asset} — open any model in an isolated preview scene |
| `exit-asset-preview` | `EmptyParams` | `PlayStateResult` | exit-asset-preview — close the asset preview and restore the authored scene + camera |
| `set-active-view` | `SetActiveViewParams` | `SetActiveViewResult` | set-active-view {view} — switch the rendered view (scene \| assetPreview) |
| `clean-assets` | `CleanAssetsParams` | `CleanReport` | clean-assets [exclude] — categorized cleanup report (dry-run; never deletes) |
| `delete-unused` | `DeleteUnusedParams` | `DeleteUnusedResult` | delete-unused {ids} {confirm} — delete confirmed-unused assets, then rescan |
| `rename-asset` | `RenameAssetParams` | `AssetRef` | rename-asset {asset, name} |
| `create-asset-folder` | `CreateAssetFolderParams` | `AssetList` | create virtual asset folder |
| `rename-asset-folder` | `RenameAssetFolderParams` | `AssetList` | rename virtual asset folder |
| `delete-asset-folder` | `DeleteAssetFolderParams` | `AssetList` | delete virtual asset folder |
| `move-asset` | `MoveAssetParams` | `AssetRef` | move asset to virtual folder |
| `asset-usages` | `AssetUsagesParams` | `AssetUsagesResult` | list scene usages of an asset |
| `probe-asset` | `AssetMetadataParams` | `AssetMetadataDto` | probe asset metadata (size, vertices, created) |
| `delete-asset` | `DeleteAssetParams` | `DeleteAssetResult` | delete asset |
| `assign-asset` | `AssignAssetParams` | `AssignAssetResult` | assign asset to entity |
| `material-create` | `MaterialCreateParams` | `MaterialCreateResult` | material-create {name} [from-entity] |
| `material-assign` | `MaterialAssignParams` | `MaterialAssignResult` | material-assign {entity, material} |
| `material-import` | `MaterialImportParams` | `MaterialImportResultDto` | material-import {path} [name] |
| `material-list` | `EmptyParams` | `MaterialListResult` | material-list |
| `material-get` | `MaterialGetParams` | `MaterialGetResult` | material-get {id\|name} |
| `material-schema` | `MaterialSchemaParams` | `MaterialSchemaResult` | material-schema {id\|name} — the material's exposed override parameters |
| `material-update` | `MaterialUpdateParams` | `MaterialUpdateResult` | material-update {id} [fields] |
| `preview-render` | `PreviewRenderParams` | `PreviewRenderResult` | preview-render {material} [size] |
| `material-set-graph` | `MaterialSetGraphParams` | `MaterialSetGraphResult` | material-set-graph {material, graph} |
| `material-create-instance` | `MaterialCreateInstanceParams` | `MaterialCreateResult` | material-create-instance {parent} [name] |
| `material-set-override` | `MaterialSetOverrideParams` | `MaterialSetOverrideResult` | material-set-override {material, field, value} |
| `material-compile-graph` | `MaterialCompileParams` | `MaterialCompileResult` | material-compile-graph {material} |
| `material-cook` | `EmptyParams` | `MaterialCookResult` | material-cook |
| `save-scene` | `PathParams` | `PathResult` | save-scene {path} |
| `load-scene` | `PathParams` | `PathResult` | load-scene {path} |
| `save-project` | `OptionalPathParams` | `ProjectInfoDto` | save active project |
| `load-project` | `OptionalPathParams` | `ProjectStatusDto` | load-project {path} |
| `reload-project` | `EmptyParams` | `ProjectStatusDto` | reload the active project |
| `get-stores` | `EmptyParams` | `ProjectStoresDto` | get-stores — enabled asset-store connectors for the project |
| `set-stores` | `ProjectStoresDto` | `ProjectStoresDto` | set-stores {enabled} — set the project's enabled asset-store connectors |
| `screenshot` | `ScreenshotParams` | `ScreenshotResult` | capture screenshot |
| `get-thumbnail` | `ThumbnailParams` | `ThumbnailResult` | get asset thumbnail |
| `view-asset` | `ThumbnailParams` | `ThumbnailResult` | view asset thumbnail |
| `thumbnail-cache` | `ThumbnailCacheParams` | `ThumbnailCacheResult` | inspect or empty the thumbnail disk cache |
| `export-app` | `ExportAppParams` | `ExportAppResult` | export-app {outputDir, app} — cook the project into a standalone app folder |
| `quit` | `EmptyParams` | `QuitResult` | close the running app |

## In the code

| What | File | Symbols |
|---|---|---|
| Typed command inventory | `engine/crates/protocol/src/command.rs` | `COMMANDS`, `CommandSpec` |
| Generated schema | `schemas/control/openrpc.generated.json` | `methods`, `components.schemas` |
| Registry and dispatch | `engine/crates/control/src/registry.rs` | `register_builtin_commands`, `CommandRegistry::dispatch` |
| Registry completeness test | `engine/crates/control/src/registry.rs` | `registry_covers_the_protocol_manifest` |
| Host-owned script command | `engine/crates/host/src/layer.rs` | `register_script_schema_command` |
| CLI argument mapping | `engine/crates/sa/src/main.rs` | `build_params`, `coerce` |
| Socket envelope | `engine/crates/control-client/src/lib.rs` | `request_envelope`, `Client` |

## Related
- [Control plane](../../explanations/tooling-and-control/control-plane-architecture/) — socket framing, dispatch, and lifecycle
- [sa CLI](../../explanations/tooling-and-control/sa-cli-protocol/) — shell argument coercion and output modes
- [Shared types](../../explanations/tooling-and-control/shared-types/) — DTO generation and wire invariants
