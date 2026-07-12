# Phase 1 — Analytic exponential height & distance fog: the always-on base layer

**Status:** COMPLETED

Part of `plans/volumetric/` (volumetric & height fog: a Wronski/Hillaire froxel pipeline unified with a closed-form analytic base). This phase ships a complete, self-contained cheap fog end-to-end: a nested `FogSettings` block on `SceneEnvironment` (mirroring `AtmosphereSettings`), a `set-fog` merge command that drives it from `sa`, a new `height_fog.slang` compute pass that evaluates the closed-form exponential height/distance integral over scene depth with a directional sun-inscatter lobe and sky-view-LUT-tinted in-scatter, and the `Fog` section that authors it in `EnvironmentPanel.tsx`. It composites into the scene-linear HDR `ViewTarget::offscreen` after the sky pass and *before* bloom/tonemap. It adds **no** new render-graph primitive — it reuses the existing 2D `add_compute_pass`, `SampledReadCompute` on `ViewTarget::depth`, and an in-place `StorageImageRwCompute` blend into `ViewTarget::offscreen`. It does **not** touch the froxel grid, 3D transient images, or the light-cull SSBOs; those are Phase 2/3. It is the base density every later phase injects and the far-field fallback beyond the froxel range, so it never double-counts against the volumetric path that supersedes it.

## Goal

Fog that reads as depth-correct atmospheric haze, authored per-scene like the sky, on for free with zero raymarching. Concretely:

- A new `height_fog.slang` compute shader: the UE `HeightFogCommon.ush` / iquilezles closed-form line integral of `exp2(-falloff·height)` along the view ray, with the Taylor branch that keeps near-horizontal rays finite, a `maxOpacity` clamp, a `startDistance`, a second summed analytic layer, a directional (sun) in-scatter lobe, and in-scatter tinted by the existing Hillaire sky-view LUT.
- The renderer seam: `FogRenderSettings` (a plain mirror of `SkyRenderSettings`), `Renderer::set_fog`, a `FogParams` UBO, and one fog `RgPass::compute` inserted after the sky pass and before bloom — `SampledReadCompute` on `ViewTarget::depth` (DEPTH aspect) and the sky-view LUT, `StorageImageRwCompute` blend into `ViewTarget::offscreen`.
- `FogSettings` on `SceneEnvironment` with `Default`, serialized by hand-authored `fog_to_json`/`fog_from_json` wired into `environment_to_json`/`environment_from_json` so the project's `environment` block round-trips it with no `document.rs` change.
- `set-fog` as the one control command (a `SetFogParams` DTO, result `EnvironmentDto`), reachable from `sa` with no per-command CLI code, merging a partial onto `environment.fog` and bumping `scene_version` — plus the `Fog` section in `EnvironmentPanel.tsx` and a `client.setFog` wrapper.

## Design stance (grounded in current engine practice)

The engine already resolves `scene.environment` into GPU settings each frame and folds them in through a `SceneRenderer` trait method: `submit_sky(&SkyRenderSettings)` at (`engine/crates/assets/src/render_scene.rs`, `render_scene` ~l.727-743) paints the background, and `AtmosphereSettings` (`engine/crates/scene/src/environment.rs`, `AtmosphereSettings`) drives the Hillaire sky-view / transmittance / multiscatter LUT chain in `ibl.rs`. This deliverable is the same shape one seam later: a nested settings block on the environment, a `submit_fog` frame push, and one compute pass. The PSO cache (`pipelines.rs`, `build_compute`/`build_compute_multi`), the render-graph compute-pass + barrier derivation (`render_graph.rs`, `RgPass::compute`, `SampledReadCompute`, `StorageImageRwCompute`), and the scene targets (`view_target.rs`, `ViewTarget::offscreen` rgba16f STORAGE, `ViewTarget::depth` D32 SAMPLED with DEPTH aspect) are all in place and are the exact attach points. This is the modern, technically-correct base — a physically-motivated closed-form exponential-height medium (Lagarde/de Rousiers, iquilezles, Zero-Radiance), not a per-vertex or fixed-function fog factor. The cheaper legacy shape (a screen-tint LERP on a linear distance ramp) is **not** offered as a parallel path.

**Closed-form, not marched.** The optical depth of an exponential-density medium along a straight ray has a closed form: with density falling as `exp2(-falloff·y)`, the path integral collapses to `originDensity · lineInt · rayLength`, where `lineInt = (1 - exp2(-F))/F` for `F = falloff·(y_receiver - y_camera)`. As `F → 0` (a near-horizontal ray) that ratio is `0/0`, so the one numerically critical detail is the Taylor branch `lineInt = ln2 - ½·ln2²·F`. This is the UE `CalculateLineIntegralShared`/`LineIntegralTaylor` port (Ubpa/ExponentialHeightFog) and the Zero-Radiance analytic-media result. There is no raymarch, no 3D volume, no temporal history — the whole thing is one fullscreen compute pass over reconstructed world position. The marched froxel path is Phase 3 and is a strict superset of this term, never a sibling of it.

**In-scatter that agrees with the sky for free.** The in-scatter color defaults to a flat `albedo`, but is tinted by sampling the sky-view LUT `ibl.rs` already builds (the Hillaire atmosphere chain, `EnvSource::Atmosphere`) in the view direction, so fog reddens toward the sun at sunset and matches the horizon hue with no extra authoring. A directional (sun) lobe — `dirColor · pow(saturate(dot(viewDir, sunDir)·0.5 + 0.5), dirExponent) · (1 - T)` — adds believable sun-through-haze on top. When the atmosphere is off the LUT sample degrades to the flat albedo through the *same* code path — not a second shader variant.

> **depth → reconstruct worldPos → { broad layer, ground layer } closed-form optical depth → T = max(exp2(-τ), 1 - maxOpacity); inscatter = albedo · skyViewTint(viewDir) + sunLobe → offscreen = scene·T + inscatter·(1-T) + sunLobe.** All scene-linear rgba16f, in place, before bloom. The two analytic layers sum into one τ; nothing branches into a separate target.

## NO-LEGACY checklist for this phase

- `set-fog` is **the** way to drive scene-wide fog — no second command, no fog knob smuggled onto `set-environment` or `set-atmosphere`. One `SetFogParams` DTO, one handler in `commands_scene.rs`, one merge onto `environment.fog`.
- Fog is **scene state**, so it lives in exactly one place per layer: the `FogSettings` field on `SceneEnvironment` (`environment.rs`), the hand-authored `fog_to_json`/`fog_from_json` in the `environment` block (`serde.rs`), and the per-frame `FogRenderSettings` push from `render_scene`. It is **not** a `RenderSettings` field and adds **no** `ControlRenderer` setter — the render-panel/`renderSettings` path is deliberately not used (that is the bloom/tonemap path; fog authors per-scene like the sky and must round-trip in the project's `environment` block).
- The `SceneRenderer` trait gains `submit_fog` in the same change across *every* impl: the real host renderer and the mock stub in the `render_scene` test harness (`render_scene.rs` ~l.1489-1530) — no stub left un-migrated.
- Every new DTO is registered in *all* tripwire lists in one change — `codegen.rs` `ts_decls`/`struct_fragments`, `tests/inventory.rs`, `tests/schema_fragments.rs`, the `command.rs` sibling arrays (`COMMANDS`/`COMMAND_FIXTURES`/`DTO_TYPE_NAMES`/`scene_domain()`), the hand-authored `schema.rs` `FogSettingsDto` + `Environment` schema `fog` property, and the hand-authored `component_block.ts`/`environment_dto.ts` — the build refuses to compile or the byte-equivalence tests fail otherwise, by design.

## 1 — `FogSettings` on `SceneEnvironment`

**File `engine/crates/scene/src/environment.rs`.**

Mirror `AtmosphereSettings` (~l.36) exactly: a `#[derive(Clone, Copy, Debug, PartialEq)]` struct with a `Default` impl, and a `pub fog: FogSettings` field added to `SceneEnvironment` (~l.84) plus its `Default` (~l.109). Fog is not an entity component — it is global frame state on the `Scene`, so it belongs here, next to `atmosphere`. Phase 1 carries only the analytic-relevant fields; later phases *add* fields (`mode`, `range`/`fogFar` in Phase 3; `quality` in Phase 4; `aerial_perspective`/intensity in Phase 6) additively — a new field defaulting sensibly is not a parallel path.

```rust
/// Scene-wide analytic height & distance fog (exponential-density closed form).
///
/// Composites into scene-linear HDR before bloom. The broad layer plus an optional
/// ground layer sum into one optical depth; `directional_*` adds a sun-through-haze lobe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogSettings {
    pub enabled: bool,
    pub density: f32,               // broad-layer sigma at `height`
    pub albedo: Vec3,               // in-scatter tint (multiplied by the sky-view LUT)
    pub height: f32,                // world-up reference height of the broad layer
    pub height_falloff: f32,        // exponential density falloff with world-up distance
    pub start_distance: f32,        // fog begins this far from the eye
    pub max_opacity: f32,           // clamps 1 - transmittance
    pub emissive: Vec3,             // constant in-medium emission
    pub directional_color: Vec3,    // sun-through-haze lobe color
    pub directional_exponent: f32,  // lobe sharpness (~4..64)
    pub layer2_density: f32,        // ground-haze layer; 0 disables
    pub layer2_falloff: f32,
    pub layer2_height: f32,
}
```

```rust
impl Default for FogSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            density: 0.02,
            albedo: Vec3::new(0.5, 0.6, 0.7),
            height: 0.0,
            height_falloff: 0.2,
            start_distance: 0.0,
            max_opacity: 1.0,
            emissive: Vec3::ZERO,
            directional_color: Vec3::new(1.0, 0.9, 0.7),
            directional_exponent: 8.0,
            layer2_density: 0.0,
            layer2_falloff: 0.5,
            layer2_height: 0.0,
        }
    }
}
```

> Decision to record when building: **the world up axis is +Y.** Height is `worldPos.y`, matching the directional-light default `(-0.5, -1, -0.3)` (light travels −Y) and the translate/rotate/scale gizmo. A configurable up-axis is deliberately not offered — there is one world orientation.

## 2 — Project round-trip: `fog_to_json` / `fog_from_json`

**File `engine/crates/scene/src/serde.rs`.**

Mirror `atmosphere_to_json` (~l.786) and `atmosphere_from_json` (~l.804): two free functions that serialize/parse the `FogSettings` block, defaulting per field, using `f32_value`/`vec3_to_json`/`json_f32_or`/`json_bool_or`/`vec3_from_json`/`field` exactly as atmosphere does. Wire a `("fog", fog_to_json(&env.fog))` row into `environment_to_json` (~l.833, beside the `atmosphere` row) and an `if let Some(v) = field(j, "fog") { env.fog = fog_from_json(v); }` into `environment_from_json` (~l.855, beside the atmosphere read). Because fog lives inside `environment_to_json`/`from_json`, `document.rs` picks it up for project save/load with **no** change there.

```rust
fn fog_to_json(f: &FogSettings) -> Value {
    object([
        ("enabled", Value::Bool(f.enabled)),
        ("density", f32_value(f.density)),
        ("albedo", vec3_to_json(f.albedo)),
        ("height", f32_value(f.height)),
        ("heightFalloff", f32_value(f.height_falloff)),
        ("startDistance", f32_value(f.start_distance)),
        ("maxOpacity", f32_value(f.max_opacity)),
        ("emissive", vec3_to_json(f.emissive)),
        ("directionalColor", vec3_to_json(f.directional_color)),
        ("directionalExponent", f32_value(f.directional_exponent)),
        ("layer2Density", f32_value(f.layer2_density)),
        ("layer2Falloff", f32_value(f.layer2_falloff)),
        ("layer2Height", f32_value(f.layer2_height)),
    ])
}

fn fog_from_json(j: &Value) -> FogSettings {
    let mut f = FogSettings::default();
    if !j.is_object() { return f; }
    f.enabled = json_bool_or(j, "enabled", false);
    f.density = json_f32_or(j, "density", 0.02);
    if let Some(v) = field(j, "albedo") { f.albedo = vec3_from_json(v); }
    /* height, heightFalloff, startDistance, maxOpacity, emissive,
       directionalColor, directionalExponent, layer2* — same shape */
    f
}
```

> Decision to record when building: **hand-authored free functions, not a `SceneSerialize` impl.** Fog is on the `Scene`, not an entity component, so it follows the `atmosphere_*` free-function precedent; the keys are frozen and read byte-identically on a defaulted load (the `environment` round-trip unit test extends to cover `fog`).

## 3 — `height_fog.slang`: the closed-form optical-depth pass

**File `engine/assets/shaders/height_fog.slang` (new).**

One invocation per screen pixel (`[numthreads(8, 8, 1)]`, `computeMain`), compiled by `cargo run -p xtask -- shaders`. Reconstruct world position from `ViewTarget::depth` via `invViewProj`, form the view ray, accumulate the closed-form optical depth over both analytic layers with the Taylor fallback, build in-scatter from the flat `albedo` times a sky-view-LUT tint plus the directional lobe, and blend into `ViewTarget::offscreen` in place.

```hlsl
struct FogLayer {
    float density;        // sigma at `height`; 0 disables this layer
    float heightFalloff;  // exp2 falloff with world-up distance
    float height;         // world-up reference height
    float _pad;
};

struct FogParams {
    float4x4 invViewProj; // clip -> world for depth reconstruction
    float3 cameraPos; float maxOpacity;
    float3 albedo;    float startDistance;
    float3 emissive;  float dirExponent;
    float3 sunDir;    float useSkyLut;   // 1 when the atmosphere LUT is live, else 0
    float3 dirColor;  float _pad0;
    FogLayer layer0;  // broad layer
    FogLayer layer1;  // ground haze
};

// Closed-form line integral of exp2(-falloff*y) along the ray. The `abs(f) <= 1e-4`
// Taylor branch is the numerically critical detail for near-horizontal rays.
float layerOpticalDepth(FogLayer l, float camY, float recvY, float rayLen, float startDist) {
    const float LN2 = 0.6931472;
    float originDensity = l.density * exp2(-l.heightFalloff * (camY - l.height));
    float f = l.heightFalloff * (recvY - camY);
    float lineInt = abs(f) > 1e-4 ? (1.0 - exp2(-f)) / f
                                  : (LN2 - 0.5 * LN2 * LN2 * f);
    return originDensity * lineInt * max(rayLen - startDist, 0.0);
}

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID) {
    // worldPos = reconstruct(depth[tid.xy], invViewProj);
    // toRecv = worldPos - cameraPos; rayLen = length(toRecv); viewDir = toRecv / rayLen;
    // tau  = layerOpticalDepth(p.layer0, cameraPos.y, worldPos.y, rayLen, p.startDistance)
    //      + layerOpticalDepth(p.layer1, cameraPos.y, worldPos.y, rayLen, p.startDistance);
    // T = max(exp2(-tau), 1.0 - p.maxOpacity);
    // tint = lerp(1.0, skyView.SampleLevel(viewDir), p.useSkyLut);
    // inscatter = p.albedo * tint + p.emissive;
    // sun = p.dirColor * pow(saturate(dot(viewDir, p.sunDir) * 0.5 + 0.5), p.dirExponent);
    // out = scene * T + inscatter * (1.0 - T) + sun * (1.0 - T);
    /* ... */
}
```

> Decision to record when building: **`exp2`/base-2 throughout with the `LN2` Taylor fallback**, matching the UE `HeightFogCommon.ush` port so densities read like the reference. The `(1 - exp2(-F))/F` singularity at horizontal rays is handled by the Taylor branch, not by an epsilon-nudged division (which bands). A per-layer `density == 0` short-circuits the ground layer to zero cost. The sky-view tint degrades to `1.0` when `useSkyLut == 0` through one `lerp`, not a second shader variant — the flat-color-only fog is a subset of this pass, never a parallel one.

## 4 — Renderer seam: `FogRenderSettings`, `set_fog`, the `FogParams` UBO and the composite pass

**File `engine/crates/rendering/src/ibl.rs`.** Add a getter that exposes the sky-view LUT image + sampler `ibl.rs` already builds for `EnvSource::Atmosphere` (the target of `atmos_skyview.slang`), and a flag for whether it is live this frame, so the fog pass can bind it (`SampledReadCompute`) and set `useSkyLut`. `SkyRenderSettings` (`ibl.rs` ~l.150) is the sibling settings struct.

**File `engine/crates/rendering/src/renderer.rs`.** Define a plain `FogRenderSettings` mirroring `SkyRenderSettings` (the authored fog fields), a `Renderer::set_fog(&mut self, settings: &FogRenderSettings)` that stashes it in a per-frame field, a `#[repr(C)] FogParams` bytemuck `Pod`/`Zeroable` UBO matching the shader struct, and the fog `RgPass::compute`. Build it exactly like the existing in-place compute blends — the bloom copy pass at ~l.6105 is the template: `.access(color, RgUsage::SampledReadCompute).access(copy.prev_color, RgUsage::StorageImageRwCompute)`. The fog pass declares `SampledReadCompute` on the imported `ViewTarget::depth` (its DEPTH aspect is already set on import) and the sky-view LUT, `StorageImageRwCompute` on `ViewTarget::offscreen`, and dispatches with the 2D `add_compute_pass` (`groups_z = 1` is correct — no 3D). The per-view `invViewProj`/`cameraPos` come from the active camera the way other passes fill them; `sunDir` is the resolved directional-light direction (negated to point toward the sun) — the same one the lighting UBO uses. Insert the pass after the sky pass and before the bloom pyramid.

```rust
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FogParams {
    inv_view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 3], max_opacity: f32,
    albedo: [f32; 3],     start_distance: f32,
    emissive: [f32; 3],   dir_exponent: f32,
    sun_dir: [f32; 3],    use_sky_lut: f32,
    dir_color: [f32; 3],  _pad0: f32,
    layer0: [f32; 4],     // density, heightFalloff, height, pad
    layer1: [f32; 4],
}
```

> Decision to record when building: **fog composites strictly before bloom, in place.** Distant bright emitters must be attenuated first so the energy-conserving bloom pyramid reads already-fogged HDR and blooms them less; applying bloom first would let far highlights bloom at full strength and then get covered, punching physically wrong halos through the fog. The blend is a single `StorageImageRwCompute` read-modify-write into `offscreen` (no separate fog target, no ping-pong) — the same in-place shape TAA/GDF use; the analytic term needs no history.

## 5 — `SceneRenderer::submit_fog` and the render-scene push

**File `engine/crates/assets/src/render_scene.rs`.** Add `fn submit_fog(&mut self, settings: &FogRenderSettings)` to the `SceneRenderer` trait (~l.51, beside `submit_sky` ~l.123). In `render_scene` (~l.727), resolve `scene.environment.fog` into a `FogRenderSettings` right where the sky block already resolves `env` into `SkyRenderSettings` (~l.728-743), and call `renderer.submit_fog(&fog)`. Add the matching stub to the mock `SceneRenderer` in the test harness (~l.1489-1530) so the render-scene tests compile and exercise the call — no impl left behind.

```rust
// in render_scene, next to the sky resolve:
let fog = FogRenderSettings::from_env(&env.fog);
renderer.submit_fog(&fog);
```

> Decision to record when building: **one frame push from `scene.environment.fog`, no `ControlRenderer` setter.** Fog reaches the GPU the same way the sky does — resolved from environment state each frame — so there is no live render-settings toggle and nothing to persist in `renderSettings`. The mock stub is a no-op that records the last settings for assertions, matching the `submit_sky` stub.

## 6 — Protocol DTO and command table

**File `engine/crates/protocol/src/dto.rs`.** Add `SetFogParams` cloning `SetAtmosphereParams` (~l.2503): the `json: Option<Value>` escape hatch, `enabled: Option<bool>` with `coerce::opt_boolean`, and one `Option<T>` per authored field (`density`, `albedo: Option<Vec3>`, `height`, `height_falloff`, `start_distance`, `max_opacity`, `emissive: Option<Vec3>`, `directional_color: Option<Vec3>`, `directional_exponent`, `layer2_density`, `layer2_falloff`, `layer2_height`) — full derive stack `#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]`, `#[serde(rename_all = "camelCase")]`, `#[ts(export)]`. The result reuses the opaque `EnvironmentDto` (~l.2460).

**File `engine/crates/protocol/src/command.rs`** — the four sibling edits placed beside `set-atmosphere`: the `COMMANDS` row (~l.367), the `COMMAND_FIXTURES` tuple (`("set-fog", "fog-disabled")`, ~l.1163 beside `atmosphere-disabled`), `"SetFogParams"` in `DTO_TYPE_NAMES` (~l.1646), and `"set-fog"` in the `scene_domain()` test list (~l.1930). Position sets the frozen wire order.

**File `engine/crates/protocol/src/codegen.rs`** — `decl_entry!(SetFogParams)` in `ts_decls()` (~l.262) and `frag_entry!(SetFogParams)` in `struct_fragments()` (~l.560), beside the atmosphere entries.

**File `engine/crates/protocol/src/schema.rs`** — a hand-authored `FogSettingsDto` schema object cloning `AtmosphereSettingsDto` (~l.575), a `"fog": { "$ref": "#/components/schemas/FogSettingsDto" }` property in the `Environment` schema (~l.622) with `"fog"` added to its `required` array (`EnvironmentDto` resolves to this hand-authored `Environment` schema because it is `#[serde(transparent)]` and opaque).

**File `engine/crates/protocol/tests/inventory.rs`** — add `SetFogParams` to the catalog list (~l.242). **File `engine/crates/protocol/tests/schema_fragments.rs`** — add `check!(SetFogParams, "SetFogParams")` (~l.244); this asserts byte-equivalence against the committed `openrpc.generated.json`, so it fails until the artifact is regenerated.

## 7 — Control seam: the `set-fog` handler

**File `engine/crates/control/src/commands_scene.rs`.** Register `set-fog` cloning the `set-atmosphere` handler verbatim (~l.834): `reg.register::<SetFogParams, EnvironmentDto>("set-fog", "...", |ctx, params| { ... })`. Read `environment_to_json(&ctx.scene_edit.active_scene().environment)`, pull the `"fog"` sub-object (`unwrap_or_else(|| json!({}))`), apply the `params.json` object merge first, then each `Some` field (`fog["density"] = json!(v)`, `fog["albedo"] = vec3_json(v)`, …), write `body["fog"] = fog`, reassign `environment = environment_from_json(&body)`, bump `ctx.scene_edit.scene_version`, and return `environment_dto(ctx)` (~l.83). The `sa` CLI needs **no** per-command code — it is clap-driven from the manifest, so `sa set-fog --enabled true --density 0.05 --directional-exponent 16` works once the command is registered.

> Decision to record when building: **partial-merge onto the `environment` JSON, one command.** Reusing `environment_to_json`/`from_json` as the merge substrate (rather than a bespoke `FogSettings` patcher) keeps the single authoring surface and guarantees the round-trip and the wire agree. There is no `set-fog-layer2` or per-field command — one `SetFogParams` covers the whole block.

## 8 — Editor and codegen

- **Regenerate:** run `cargo run -p xtask -- gen-protocol`, then `bun run check` in `editor/`. **Never hand-edit** `editor/src/protocol/sa-types.ts` — `CommandName` gains `set-fog` and `SetFogParams` appears automatically.
- **File `engine/xtask/src/protocol/component_block.ts`** — add a `FogSettingsDto` interface cloning `AtmosphereSettingsDto` (~l.176). **File `engine/xtask/src/protocol/environment_dto.ts`** — add `fog: FogSettingsDto;` to the `EnvironmentDto` interface (~l.12, beside `atmosphere`), because `EnvironmentDto` is opaque `{ value: Value }` so its TS shape is hand-authored here.
- **File `tools/check-control-schema/check.ts`** — add a `paramsForFixture()` case for `"fog-disabled"` (an `enabled: false` payload).
- **File `editor/src/control/client.ts`** — a hand-authored typed wrapper `setFog(patch)` cloning `setAtmosphere` (~l.788): `call("set-fog", patch)`.
- **File `editor/src/panels/EnvironmentPanel.tsx`** — a new `Fog` section cloning the self-contained `Atmosphere` block (`Separator` + a `Switch` enabled gate, then `Row`-wrapped `NumberDrag` for density/height-falloff/start-distance/max-opacity/directional-exponent, `ColorField` for albedo/emissive/directional-color, a second-layer foldout), routed through its own `fogCoalescers`/`patchFog`/`recordFogEdit` trio distinct from `set-atmosphere`, over `environment.fog`. It reads live state from the `environment` slice (re-synced by the reconcile poll on a `scene_version` bump), folds the echoed `EnvironmentDto` optimistically, brackets scrubs with `onDragStart`/`onDragEnd` to gate the poll, and records one `pushEdit(inverse, "scene")` per gesture — the exact write path `patchAtmos`/`recordAtmosEdit` already use.

## Out of scope (later phases)

- **No 3D transient image and no 3D dispatch.** This phase adds no new render-graph primitive; the froxel volume (a `TYPE_3D` transient rgba16f image) and the `groups_z` 3D compute dispatch are explicitly Phase 2's job. The fog pass here is a 2D fullscreen compute over screen depth.
- **No `mode` field, no froxel path.** `FogSettings.mode` (analytic|volumetric), `range`/`fogFar`, `phase_g`, and the inject/integrate/composite passes land in Phase 3, where the analytic height density becomes the *base medium* the froxel grid injects (so the two never double-count). This phase leaves that seam clean — the closed-form term is a strict subset of the froxel path.
- **No per-light or quality controls.** Per-light `volumetricScattering`/`castVolumetricShadow` and the low/medium/high grid tiers are Phase 4. **No local `FogVolume` entities** — Phase 5. **No aerial perspective** — Phase 6, which finalizes the sky-view-LUT inscatter tint stubbed here.
- **No `RenderSettings`/`ControlRenderer` surface.** Fog is scene state; it is deliberately not a render-panel toggle.

## Known interactions (note, don't over-engineer)

- **Transparents.** Forward-blended surfaces are not in the depth prepass, so they do not receive the composite-pass term. Note it; the closed-form term applied in their forward shader is a small follow-up (it can reuse the same `FogParams` UBO). Phase 1 correctly fogs opaque geometry via the depth read; leave transparents for the same shared UBO once the forward pass wants it.
- **Sky pixels.** At the far plane the reconstructed `rayLen` is large and the fog saturates toward `1 - maxOpacity`; the sky pass has already painted the background, so the composite naturally fades distant sky into the fog color. Do not special-case the far plane — the `startDistance` and `maxOpacity` clamps are sufficient.
- **Sky-view LUT availability.** When the atmosphere is off, `useSkyLut = 0` and the tint is `1.0` — the flat `albedo` carries the in-scatter. Do not build a gradient fallback LUT; the flat path is correct and is the same code.
- **Exposure.** The fog composites in scene-linear HDR before the tonemap/grade compute pass, so authored densities are exposure-independent; the tonemap stays last and reads the fogged+bloomed HDR. No interaction to build.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Boot a fixture scene with real depth range headless (`just run-engine-headless`) on the NVIDIA GPU with a validation-clean log and confirm depth-correct haze that thickens with distance and reddens toward the sun; check that `sa set-fog --enabled true --density 0.05 --height-falloff 0.2 --directional-exponent 16` visibly changes the frame and that the reply echoes the updated `EnvironmentDto` `fog` block. Because a wire type changed, run `bun run check` in `editor/` (regenerates `@saffron/protocol` — `CommandName` must gain `set-fog`; never hand-edit `sa-types.ts`) and `just e2e`, adding a `tests/e2e` case that drives `set-fog` over the control plane and asserts the `EnvironmentDto` echo plus a validation-clean log. Add the `docs/content/` rendering hub `_index.md` row and a new `Fog` concept page — the analytic height/distance model, the closed-form line integral with the Taylor branch, the directional in-scatter lobe, and the composite-before-bloom ordering, with the slim `What | File | Symbols` table (`engine/assets/shaders/height_fog.slang`; `engine/crates/scene/src/environment.rs`, `FogSettings`; `engine/crates/rendering/src/renderer.rs`, `Renderer::set_fog`, `FogParams`; `engine/crates/protocol/src/dto.rs`, `SetFogParams`) — in the same change.
