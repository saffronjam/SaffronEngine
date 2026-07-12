# Phase 4 — Temporal reprojection, quality controls & per-light volumetric flags

**Status:** COMPLETED

Part of `plans/volumetric/` (volumetric + height fog as one froxel-grid subsystem). This phase makes the deliberately low-resolution froxel grid Phase 3 stood up look smooth in motion and hands artists per-light control over how each light feeds the fog. It ships one self-contained refinement slice: a persistent ping-pong history volume + reprojection fused into `fog_inject.slang`, a Halton sub-froxel jitter that reuses the TAA jitter the renderer already advances, optional neighbourhood + light clamping, two new per-light fields (`volumetric_scattering` / `cast_volumetric_shadow`) packed into the free `GpuLight`/`LightUbo` lanes and surfaced through the registry-driven Inspector, and a `quality` tier on `FogSettings` that selects the grid dimensions. It depends on Phase 3 (`fog_inject.slang`, `fog_integrate.slang`, the composite, `froxel_fog.rs`, `FogSettings.mode`) and Phase 2 (the 3D-transient infra + the `lighting.slang` shared module). It does **not** touch local `FogVolume` entities (Phase 5) or aerial perspective (Phase 6), and it introduces exactly one new render-graph capability: a **cross-frame-surviving** (persistent, ping-pong) 3D volume, where Phase 2 only added a per-frame transient one.

## Goal

A single point/spot light swept through the scene throws a stable, non-ghosting shaft, and an artist can dial its shaft brightness or switch its volumetric shadow off from the Inspector or `sa` with no per-component code. Concretely:

- `fog_inject.slang` gains a reprojection tail: it reads last frame's **linear** `scatterExtinction` at the reprojected froxel-centre world position and blends `historyBlend` (0.05 default) of the fresh sample, resetting to the fresh sample whole on first frame / camera cut / off-frustum.
- A Halton sub-froxel depth+XY jitter driving the froxel-centre reconstruction, reusing `Renderer::active_view_jitter` (the same NDC offset fed to `taa.slang` `Push.jitter`), so successive frames supersample distinct points inside each froxel and the history accumulation resolves them coherently.
- `volumetric_scattering: f32` (default `1.0`) and `cast_volumetric_shadow: bool` (default `true`) on `PointLight` / `SpotLight` / `DirectionalLight`, packed into the two free `GpuLight.spot_cos` lanes (punctual) and the reserved `LightUbo.extra_flags` lanes (directional); the inject shader reads them as a per-light in-scatter multiplier and a shadow-sampling gate.
- A `quality: FogQuality` (`low`/`medium`/`high` → grid Z `64`/`64`/`128`, XY `128×72`/`160×90`/`160×90`) on `FogSettings`, selecting the `froxel_fog.rs` grid dimensions; plus optional `neighborhoodClamp` + `lightClamp` firefly knobs. All ride the existing `set-fog` command (Phase 1) and the `EnvironmentPanel` Fog section; per-light fields ride generic `set-component-field`.

## Design stance (grounded in current engine practice)

The engine already runs exactly this temporal shape one level up: `taa.slang` reconstructs the input-extent current color into the display grid, reprojects history through the per-view previous camera (`view.prev_view_proj`, `view.prev_view_proj_valid` reset at `renderer.rs` ~l.3050), clips it to the neighbourhood statistics, and blends velocity-adaptively — all driven by the Halton jitter cycle `view.advance_jitter()` advances each TAA frame (`renderer.rs` ~l.6338) and read back through `active_view_jitter()` (~l.1330). The fog reprojection is the same technique in a 3D grid instead of a 2D image, and it reuses the *same* jitter offset so the two temporal filters stay phase-locked. The froxel volumes, the `FogGridParams` UBO, the inject/integrate/composite passes, the `import_image_3d` seam, and the compute-pass helper are all in place from Phases 2–3; the only genuinely new plumbing is promoting Phase 3's per-frame transient `scatterExtinction` to a persistent ping-pong pair so last frame's linear scatter survives for the reprojection read.

The reprojection is Hillaire-2015's energy-conserving temporal integration, not a naïve blend of the integrated result:

> **jittered froxel centre → fresh (L_scat, σ_t) → reproject through `prevViewProj` → lerp 5% fresh over the previous-frame LINEAR scatter (never the integrated transmittance) → optional 3×3×3 clamp → write `scatterExtinction`; the front-to-back integrate (Phase 3) still runs fresh each frame off the reprojected volume.** History carries the *linear* scatter+extinction only — the accumulated transmittance is non-linear in σ_t, so blending it desyncs energy; and the sub-froxel jitter is the supersampling mechanism that lets a coarse 64-slice grid resolve a crisp shaft edge over the jitter cycle.

Per-light control is packed into slots that already exist, not new GPU records. `GpuLight` is a byte-asserted 4×`vec4` std430 record (`gpu_types.rs`, `assert!(size_of::<GpuLight>() == 64)`) whose `spot_cos.zw` lanes are unused by both point and spot lights today; `LightUbo` is a byte-asserted std140 record (`lighting.rs`, `assert!(size_of::<LightUbo>() == 432)`) whose `extra_flags.zw` are documented `reserved`. The two per-light fields land there — growing either record is not offered as an alternative.

## NO-LEGACY checklist for this phase

- **One fog authoring surface.** The quality tier and temporal knobs ride the existing `set-fog` merge command (Phase 1) over `SceneEnvironment.fog` — there is no second command and no fog knob smuggled onto `set-environment` or `set-atmosphere`. `SetFogParams` (Phase 1) gains fields; it does not spawn a sibling DTO.
- **One per-light write path.** `volumetric_scattering` / `cast_volumetric_shadow` are plain `PointLight`/`SpotLight`/`DirectionalLight` fields edited by the *existing* generic `set-component-field` / `set-component` / `inspect` — no `set-light-volumetrics` command, no per-component handler. The Inspector renders them from two `FIELD_HINTS` rows per component, no `InspectorPanel` branch.
- **One packing slot per lane, no record growth.** `volumetric_scattering → spot_cos.z`, `cast_volumetric_shadow → spot_cos.w` for punctual; the directional pair folds into `extra_flags.zw`. The `size_of` asserts on `GpuLight` (64) and `LightUbo` (432) stay unchanged and enforce it.
- **History is one volume pair.** Phase 3's transient `scatterExtinction` current volume is *replaced* by a persistent ping-pong pair in `froxel_fog.rs` in the same change — the transient version is deleted, not left beside the persistent one. The `integration` volume stays transient (recomputed each frame).
- **The grid dims live in one place.** `FROXEL_GRID_{X,Y,Z}` become a per-tier lookup keyed by `FogQuality` in `froxel_fog.rs`; the `froxel_grid_matches_shader` CPU-mirror unit test (Phase 2) is updated to cover all three tiers in the same change so the CPU↔GPU depth mapping stays locked per tier.
- **Every wire change hits all tripwires together.** The extended `SetFogParams` + `FogSettingsDto` schema + the hand-authored `FogSettingsDto`/`environment_dto.ts` interface, and the per-component `component_block.ts` interface additions, all land in one change; `xtask gen-protocol` regenerates `sa-types.ts` — never hand-edited.

## 1 — History volume + reprojection (the new persistent-3D capability)

**File `engine/crates/rendering/src/froxel_fog.rs`.**

Promote the Phase-3 `scatterExtinction` current volume from a per-frame `TransientImage3D` to a **persistent ping-pong pair** of `Image3D` (`resources.rs` `Image3D`, `R16G16B16A16_SFLOAT`, `STORAGE | SAMPLED`) held on the `FroxelFog` module and swapped each frame — the same persistent, cross-frame pattern the Global-SDF cascades use (`global_sdf.rs`, `Image3D::new` ~l.361), read back through `import_image_3d` (`render_graph.rs` ~l.494) with cross-frame layout carried by `alloc_external_layout` / `external_layout` (`render_graph.rs` ~l.450/457). This is the one new render-graph capability the phase adds: Phase 2 gave the pool a per-frame transient 3D image; a reprojection history must **survive to the next frame**, so it is owned persistently and its `GENERAL ↔ SHADER_READ_ONLY` transitions are tracked across the frame boundary the way GDF cascades already are. The `integration` volume stays transient.

```rust
struct FroxelFog {
    /// Ping-pong linear (L_scat, sigma_t) volume: index `write` is this frame's inject target,
    /// `write ^ 1` is last frame's history read. Persistent so history survives the frame.
    scatter: [Image3D; 2],
    write: usize,               // toggles each frame
    history_valid: bool,        // false on first frame / grid-dims change / camera cut
    // ... FogGridParams UBO, descriptor sets, jitter state (§2), quality dims (§6) ...
}
```

**File `engine/assets/shaders/fog_inject.slang`.**

After Phase 3's `(L_scat, sigma_t)` accumulation for the jittered froxel-centre world position (§2), reproject that world position through `prevViewProj` (a new `FogGridParams` field, populated from `view.prev_view_proj`) to the previous-frame froxel `uvw`, sample the history volume (linear filter), and blend. The write is the **linear** scatter+extinction; the integrate pass (Phase 3, unchanged) still marches this fresh each frame.

```hlsl
// FogGridParams (set 1, b0) gains, beside the Phase-3 grid fields:
//   float4x4 prevViewProj;    // last frame's un-jittered world->clip (view.prev_view_proj)
//   float2   subFroxelJitter; // this frame's NDC jitter (== taa.slang Push.jitter)  [§2]
//   float    historyBlend;    // fresh-sample weight, 0.05 default
//   uint     historyValid;    // 0 = first frame / camera cut -> take fresh whole
[[vk::binding(?, 1)]] Texture3D<float4>   scatterHistory;   // last frame, linear sampler
[[vk::binding(?, 1)]] RWTexture3D<float4> scatterExtinction; // this frame (rgb=L_scat, a=sigma_t)

float4 fresh = float4(L_scat, sigma_t);
float4 clip  = mul(params.prevViewProj, float4(worldPos, 1.0));
float3 hUvw  = froxelUvwFromClip(clip, params);   // NDC->[0,1]^2 + log-Z slice (froxel<->cluster mirror)
bool   reuse = params.historyValid != 0 && all(hUvw >= 0.0) && all(hUvw <= 1.0);
float4 hist  = scatterHistory.SampleLevel(linearSampler, hUvw, 0.0);
float4 outv  = reuse ? lerp(hist, fresh, params.historyBlend) : fresh;   // LINEAR only
scatterExtinction[froxel] = outv;
```

- `historyValid` is driven from `view.prev_view_proj_valid` (reset at `renderer.rs` ~l.3050 on a camera cut / first frame) **and** cleared whenever the grid dimensions change (a `quality` switch, §6) so a resized grid never reprojects against a differently-shaped history.
- `froxelUvwFromClip` is the same exponential-Z froxel↔slice mapping the Phase-2 `froxel_grid_matches_shader` test locks (`w = log(viewZ/near)/log(far/near)`), reused for the reprojected read.

> Decision to record when building: **reproject the LINEAR `(L_scat, σ_t)`, never the integrated `(accum, transmittance)`.** Hillaire's energy note — transmittance is `exp(-∫σ)`, non-linear in σ_t, so a temporal lerp of the integrated result accumulates the wrong energy at density discontinuities. Blending the linear scatter and re-integrating fresh each frame is the only correct path; the naïve "blend the final volume" is not offered.

> Decision to record when building: **the history is a persistent ping-pong pair, the integration volume stays transient.** History must outlive the frame, so it is owned like a GDF cascade; the integration volume is a pure per-frame consumer, so it stays in the transient pool. Making the whole set persistent would waste ~44 MB of frustum scratch that never needs to survive.

## 2 — Halton sub-froxel jitter (reuse the TAA jitter)

**File `engine/crates/rendering/src/froxel_fog.rs` + `engine/assets/shaders/fog_inject.slang`.**

Do not roll a second sequence. `Renderer::active_view_jitter()` (~l.1330) already returns this frame's Halton(2,3) NDC offset — the same value fed to `taa.slang` `Push.jitter` (~l.7984) and advanced by `view.advance_jitter()` (~l.6338). Thread it into `FogGridParams.subFroxelJitter`; in `fog_inject` apply it as a sub-froxel shift on the froxel-centre reconstruction — both the XY tile centre and the exponential-Z slice depth — so each frame samples a distinct point inside the froxel and the §1 history accumulation supersamples the coarse grid coherently.

```hlsl
// froxel centre, jittered: the XY tile centre nudged by the shared NDC jitter, and the Z slice
// centre nudged a fraction of a slice so successive frames walk the froxel interior.
float3 froxelCenterJittered(uint3 f, FogGridParams p) {
    float2 uv = (float2(f.xy) + 0.5 + p.subFroxelJitter * 0.5) / p.gridSize.xy;
    float  w  = (float(f.z)  + 0.5 + haltonZ(f, p.frameIndex)) / p.gridSize.z;   // R2/Halton on Z
    return worldFromFroxelUvw(float3(uv, w), p);
}
```

> Decision to record when building: **one jitter drives TAA and fog.** Reusing `active_view_jitter()` keeps the two temporal filters phase-locked; an independent fog sequence would beat against TAA's cycle and reintroduce the shimmer both are trying to remove. When TAA is off the jitter is zero (`renderer.rs` ~l.6335 gate), so the fog degenerates to un-jittered froxel centres — correct, matching the TAA-off path.

## 3 — Neighbourhood clamp + light clamp (optional firefly control)

**File `engine/assets/shaders/fog_inject.slang` + `froxel_fog.rs`.**

Two opt-in `FogGridParams` knobs, both off by default, mirroring the `taa.slang` clip and the Blender/EEVEE light-clamp:

- `neighborhoodClamp` (bool → `uint`): before the §1 lerp, clamp the reprojected history sample to the min/max of the fresh `scatterExtinction` in the 3×3×3 neighbourhood (in linear scatter, the `clip_aabb` idea from `taa.slang` ~l.152), suppressing ghosting when a light moves fast. Requires reading the fresh neighbourhood, so it runs as a second read of this frame's write — kept behind the flag because it costs 27 taps.
- `lightClamp` (`f32`, `0` = off): cap each light's per-froxel in-scatter contribution before accumulation, killing the single-froxel firefly a bright light grazing a shadow edge injects.

> Decision to record when building: **both clamps default off.** The Karis-style history blend + sub-froxel jitter already stabilise a steady scene; the clamps are the escape hatch for pathological fast-moving lights only. Turning them on unconditionally would over-smooth static shafts — they are labelled quality knobs, not the recommended baseline.

## 4 — Per-light volumetric fields (components + serde)

**File `engine/crates/scene/src/component.rs`.**

Add two fields to `PointLight` (~l.715), `SpotLight` (~l.739), and `DirectionalLight` (~l.690), beside the existing `color`/`intensity`, with defaults in each `impl Default`:

```rust
pub volumetric_scattering: f32,     // per-light fog in-scatter multiplier, default 1.0
pub cast_volumetric_shadow: bool,   // gate this light's shadow sampling in fog, default true
```

**File `engine/crates/scene/src/serde.rs`.**

Extend each light's `SceneSerialize` (`DirectionalLight` ~l.339, `PointLight` ~l.358, `SpotLight` ~l.375) with a `("volumetricScattering", f32_value(...))` / `json_f32_or(..., 1.0)` key and a `("castVolumetricShadow", Value::Bool(...))` / `json_bool_or(..., true)` key — exactly the `f32_value`/`json_f32_or` + `Value::Bool`/`json_bool_or` pair `ReflectionProbe.box_projection` already uses (~l.403/411). The project round-trips through the existing entity serialization with no `document.rs` change.

> Decision to record when building: **`cast_volumetric_shadow` defaults `true`.** God-rays from shadowed in-scatter are the marquee visual; a light should throw a shaft out of the box. It gates sampling of the *already-rendered* shadow map / cube (cheap), so `true` costs nothing extra beyond the fog pass itself; the expensive inline-RT shaft is a separate quality opt-in (Out of scope).

## 5 — GpuLight / LightUbo packing + upload

**File `engine/crates/rendering/src/gpu_types.rs`.**

`GpuLight.spot_cos` carries `x = cos(inner)`, `y = cos(outer)`, and `z`/`w` are unused by both point and spot today. Repurpose the free lanes; the `assert!(size_of::<GpuLight>() == 64)` stays and enforces no growth. Document the lanes in the field doc-comment:

```rust
/// `x` cos(inner angle), `y` cos(outer angle), `z` volumetric-scattering multiplier,
/// `w` cast-volumetric-shadow gate (0/1).
pub spot_cos: Vec4,
```

**File `engine/crates/rendering/src/lighting.rs`.**

The directional light is not a `GpuLight`; it rides `LightUbo`. Its two fields fold into the documented-reserved `extra_flags.zw` lanes (~l., `extra_flags`: `x` SSR, `y` RT-refl, `zw` reserved). Add `directional_volumetric: f32` + `directional_cast_volumetric_shadow: bool` to `SceneLighting` (~l.207) and write them into `extra_flags.z` / `extra_flags.w` where the renderer derives `LightUbo` from `SceneLighting`. The `assert!(size_of::<LightUbo>() == 432)` stays.

**File `engine/crates/assets/src/render_scene.rs`.**

In `gather_punctual_lights` (~l.783), write the packed lanes for both the point arm (~l.801) and the spot arm (~l.821):

```rust
spot_cos: Vec4::new(inner_cos, outer_cos,           // point arm: inner/outer = 0
    light.volumetric_scattering,
    if light.cast_volumetric_shadow { 1.0 } else { 0.0 }),
```

In `gather_directional_light` (~l.750), return the two new fields and thread them into `SceneLighting`. The mock `SceneRenderer` test stub (~l.1494 `set_scene_lighting`) needs no new method — `SceneLighting` simply gains fields.

**File `engine/assets/shaders/fog_inject.slang`** — read the lanes in the per-light accumulation loop (Phase 3): multiply each light's in-scatter by `spot_cos.z` and skip the shadow sample (treat the light as unshadowed in fog, or drop it if intensity·multiplier is zero) when `spot_cos.w < 0.5`; read `extra_flags.zw` for the directional light.

## 6 — Quality tiers on the froxel grid

**File `engine/crates/rendering/src/froxel_fog.rs`.**

Replace the Phase-2 `const FROXEL_GRID_{X,Y,Z}` with a per-tier lookup keyed by `FogQuality`:

```rust
pub enum FogQuality { Low, Medium, High }

impl FogQuality {
    /// (X, Y, Z) froxel-grid dimensions per tier. Z is the expensive axis (per-slice light eval).
    fn grid_dims(self) -> (u32, u32, u32) {
        match self {
            FogQuality::Low    => (128, 72, 64),
            FogQuality::Medium => (160, 90, 64),
            FogQuality::High   => (160, 90, 128),
        }
    }
}
```

The volume allocations, the `FogGridParams.gridSize`, the descriptor writes, and the 3D dispatch group counts all read the selected dims; a tier switch reallocates the ping-pong history + transient integration volumes and clears `history_valid` (§1). Extend the `froxel_grid_matches_shader` CPU-mirror unit test (Phase 2) to assert the CPU↔GPU exponential-Z mapping for all three tiers.

**File `engine/crates/scene/src/environment.rs`.** Add `quality: FogQuality` to `FogSettings` (Phase 1) with a `Default` of `Medium`, plus the temporal knobs `history_blend: f32` (0.05), `neighborhood_clamp: bool` (false), `light_clamp: f32` (0.0). `FogQuality` is a small `enum` mirroring the existing `SkyMode` shape in this file.

> Decision to record when building: **Z is the tier axis, XY barely moves.** Per-slice light evaluation dominates the cost, so `low`/`medium` share Z=64 and only `high` doubles to Z=128; XY steps `128×72 → 160×90` for tile density. This matches UE's `r.VolumetricFog.GridSizeZ` being the primary cost lever, not a naïve uniform scale.

## 7 — Protocol: SetFogParams extension + per-light wire shapes

**File `engine/crates/protocol/src/dto.rs`.** Extend `SetFogParams` (Phase 1) with `Option`al `quality` (a `FogQuality` DTO enum, camelCase `low`/`medium`/`high`), `history_blend`, `neighborhood_clamp`, `light_clamp` — same `Option<T>` merge-partial shape as the Phase-1 fog fields. No new params/result struct; this is one command's payload growing.

**File `engine/crates/protocol/src/schema.rs`.** Add the new properties to the hand-authored `FogSettingsDto` schema (Phase 1) and, if `FogQuality` needs its own `$ref` enum object, add it beside `FogSettingsDto`. The `Environment` schema already `$ref`s `FogSettingsDto`.

**File `engine/xtask/src/protocol/environment_dto.ts` + `component_block.ts`.** Add the four fields to the hand-authored `FogSettingsDto` interface; add `volumetricScattering: number` + `castVolumetricShadow: boolean` to the hand-authored `PointLight`, `SpotLight`, and `DirectionalLight` interfaces in `component_block.ts` (these are the registered components' wire shapes — the fields must appear here or the `luau.rs` reachability test cannot see them).

**File `engine/crates/protocol/src/{codegen.rs,command.rs}` + `tests/{inventory.rs,schema_fragments.rs}`.** `SetFogParams` is already registered across the tripwires (Phase 1); adding fields does not add a struct, so only the committed `schema_fragments` / openrpc artifacts change — regenerated in §9. No new `COMMANDS` row, no new `DTO_TYPE_NAMES` entry.

**Regenerate:** `cargo run -p xtask -- gen-protocol` rewrites `editor/src/protocol/sa-types.ts`, `schemas/control/*.generated.json`, and `sa.generated.luau`. Never hand-edit `sa-types.ts`.

## 8 — Control seam

**File `engine/crates/control/src/commands_scene.rs`.** The `set-fog` handler (Phase 1) already merges a partial `SetFogParams` onto `environment_to_json(...)["fog"]` and re-parses via `environment_from_json`; the new fields flow through the merge with **no** handler change beyond validating ranges (`history_blend ∈ [0,1]`, `light_clamp ≥ 0`), returning `Err(Error::command(...))` on bad input. Per-light fields need **no** control code at all — `set-component-field` / `set-component` / `inspect` are registry-driven and already round-trip any `PointLight`/`SpotLight`/`DirectionalLight` field. The `sa` CLI needs no per-command code for either.

## 9 — Persistence

**File `engine/crates/scene/src/serde.rs`.** Fog is `SceneEnvironment` state, not `RenderSettings` — so persistence is the environment block, not `render_settings.rs`. Extend `fog_to_json` / `fog_from_json` (Phase 1) with the `quality` (string), `historyBlend`, `neighborhoodClamp`, `lightClamp` keys; the project round-trips automatically through `environment_to_json` / `environment_from_json` (already wired, `document.rs` unchanged). Per-light fields round-trip through the §4 component `SceneSerialize`. There is **no** touch to `engine/crates/rendering/src/render_settings.rs`.

## 10 — Editor and codegen

- **File `editor/src/panels/EnvironmentPanel.tsx`** — in the Fog section (Phase 1, cloned from the Atmosphere block), add a `Select` for `quality` (`low`/`medium`/`high`), a `NumberDrag` for `historyBlend`, a `Switch` for `neighborhoodClamp`, and a `NumberDrag` for `lightClamp`, each routed through the existing `patchFog` / `fogCoalescers` / `recordFogEdit` trio and `client.setFog(...)` — the same coalesced optimistic write + one `pushEdit(..., "scene")` per gesture the atmosphere rows use.
- **File `editor/src/components/fieldRenderer.tsx`** — add six `FIELD_HINTS` rows, following the light/probe entries (~l.117–137):

```ts
"PointLight.volumetricScattering":        { kind: "slider", min: 0, max: 4, step: 0.01 },
"PointLight.castVolumetricShadow":        { kind: "bool" },
"SpotLight.volumetricScattering":         { kind: "slider", min: 0, max: 4, step: 0.01 },
"SpotLight.castVolumetricShadow":         { kind: "bool" },
"DirectionalLight.volumetricScattering":  { kind: "slider", min: 0, max: 4, step: 0.01 },
"DirectionalLight.castVolumetricShadow":  { kind: "bool" },
```

The `InspectorPanel` renders these generically — no per-component code.
- **File `tools/check-control-schema/check.ts`** — extend the `paramsForFixture()` `"fog-*"` case (Phase 1) so the new `SetFogParams` fields are exercised by the live-vs-schema contract test.

## Out of scope (later phases)

- **Local `FogVolume` entities (Phase 5).** This phase's per-light packing and reprojection are density-agnostic; local volumes inject into the same grid during Phase 3's density step and are reprojected/integrated by these same stages with no new path. This phase does not add the `FogVolume` component or its overlay.
- **Aerial perspective (Phase 6).** Long-range planetary scattering is a separate injection volume sharing this infra; this phase leaves the composite ledger clean but does not build it.
- **Inline-RT volumetric shadows (Future).** `cast_volumetric_shadow` gates the *shadow-map / cube* sample in fog. A per-froxel ray-query into the bound TLAS for crisp, leak-free shafts (the `rayQueryShadow` helper the Phase-2 shared module exposes) is a Future quality opt-in gated behind a per-light or per-tier flag — this phase leaves the seam (the gate lane, the shared helper) in place but keeps cascade-PCF / point-cube as the default for cost.

## Known interactions (note, don't over-engineer)

- **TAA off.** When TAA is disabled the shared jitter is zero, so fog degenerates to un-jittered froxel centres and the history still reprojects (the reprojection does not require jitter). This is correct — note it, do not add a separate fog-only jitter path for the TAA-off case.
- **Camera cut / teleport.** `view.prev_view_proj_valid` already resets on a cut (`renderer.rs` ~l.3050); routing it into `historyValid` is the whole reset. Do not add a second bespoke cut detector.
- **Grid resize vs. history shape.** A `quality` switch changes the grid dims, so the reprojected `uvw` would read a differently-shaped history — clearing `history_valid` on any dims change (§1/§6) is sufficient; do not attempt to resample the old history into the new grid.
- **Directional lane budget.** Only the reserved `extra_flags.zw` are consumed; if a later phase needs more directional flags it grows `LightUbo` deliberately, not by stealing these. Note the two lanes are now spoken for.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises; the extended `froxel_grid_matches_shader` unit test must pass for all three tiers. Boot a lit fixture with a shadow-casting occluder and a point light animated across the scene headless (`just run-engine-headless`) on the NVIDIA GPU with a validation-clean log, and confirm the shaft is **stable and non-ghosting** in motion (the reprojection + jitter working) rather than the swimming a raw low-res grid shows; switch `sa set-fog quality low` → `high` and confirm the frame time / grid cost changes. Check per-light control: `sa set-component-field <light> PointLight castVolumetricShadow false` removes its shaft and `sa set-component-field <light> PointLight volumetricScattering 2.0` brightens it. Because wire types changed (extended `SetFogParams`, new light fields), run `bun run check` in `editor/` (regenerates `@saffron/protocol` — never hand-edit `sa-types.ts`) and `just e2e`, adding a `tests/e2e` case that round-trips `volumetricScattering` / `castVolumetricShadow` on a light over the control plane and drives `set-fog quality`, asserting the echo plus a validation-clean log. Update the `docs/content/` Volumetric fog page with a temporal-reprojection + per-light-controls section (the reprojection energy note, the shared-jitter supersampling, `volumetric_scattering` / `cast_volumetric_shadow`, the quality tiers, and the `What | File | Symbols` table: `fog_inject.slang`, `froxel_fog.rs`, `GpuLight.spot_cos`, `SetFogParams`) and its hub `_index.md` row in the same change.
