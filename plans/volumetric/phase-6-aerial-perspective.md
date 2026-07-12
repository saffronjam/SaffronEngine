# Phase 6 — Aerial perspective: Hillaire-2020 atmosphere coherence

**Status:** COMPLETED

Part of `plans/volumetric/` (volumetric & height fog on a single shared transmittance ledger). This phase closes the loop with the existing Hillaire sky: it stands up a small `32×32×32` aerial-perspective froxel volume filled by ray-marching the atmosphere transmittance + multiscatter LUTs that `ibl.rs` already bakes, then folds that volume into the Phase-3 fog composite as an independent multiplied medium on the *same* transmittance ledger, so near fog, mid haze, and far planetary scattering read as one continuous atmosphere. It depends on Phase 2 (the 3D transient image + `import_image_3d` + 3D dispatch) and Phase 3 (the fog composite + shared ledger + `submit_fog` seam); it reuses that infra unchanged rather than building a parallel one. It does **not** touch the fog inject/integrate stages, the clustered light cull, the temporal reprojection (Phase 4), or the local `FogVolume` component (Phase 5) — the aerial-perspective volume carries only the atmosphere medium (no shadowed local lights), so it is the fog volume with a different, cheaper injection.

## Goal

Distant scene geometry picks up the same blue-shift and desaturation-with-distance that the sky already shows, so a far ridge and the sky behind it agree in hue instead of the ridge reading as untinted geometry pasted over a scattering sky. Concretely:

- A new `aerial_perspective.slang` compute shader fills a `32³` `rgba16f` transient volume — `rgb` = in-scattered radiance, `a` = mean transmittance — by ray-marching `ibl.rs`'s `transmittance_lut` + `multi_scatter_lut` along froxel centers reconstructed with the shared exponential-Z mapping, lifting `sampleTransmittance`/`sampleMultiScatter`/`rayleighPhase`/`hgPhase` from `atmos_skyview.slang`.
- The volume is a separate `acquire_image_3d` transient (Phase 2), filled by one `add_compute_pass` dispatch (Phase 2's `groups_z`), scheduled after `fog_integrate` and before the fog composite blend in `renderer.rs`.
- The Phase-3 fog composite (`height_fog.slang`) gains one extra sample and one extra ledger term — no second composite pass. The shared ledger becomes `T_total = T_fog·T_aerial`, `L_out = scene·T_total + inScatter_fog + T_fog·inScatter_aerial`, with the analytic height fog carrying anything beyond both.
- The Phase-1 analytic height-fog inscatter is re-tinted from the `sky_view_lut` (stubbed in Phase 1, finalized here) so fog and sky agree in hue at the horizon.
- Two fields — `aerial_perspective: bool` + `aerial_intensity: f32` — join `FogSettings`, driven by the existing `set-fog` command (no new command), persisted in the project `environment` block, reachable from `sa` and the `EnvironmentPanel` Fog section.

## Design stance (grounded in current engine practice)

Aerial perspective is built as Hillaire-2020's own design — a small froxel volume fed by the atmosphere LUTs — reusing the froxel infra this planset already landed, not as a screen-space post-effect and not as a second bespoke atmosphere path bolted onto the sky pass.

The engine already bakes the full Hillaire LUT chain each time the environment changes: `ibl.rs` records transmittance → multiscatter → sky-view (`transmittance_lut`, `multi_scatter_lut`, `sky_view_lut`, all `IblImage`) under `EnvSource::Atmosphere`, and `atmos_skyview.slang` already ray-marches those LUTs with `sampleTransmittance(r, mu)` + `sampleMultiScatter(r, sunMu)` and applies `rayleighPhase` + `hgPhase` in a single-scatter accumulation loop (`radiance += transmittance * sInt; transmittance *= stepT;`). The one thing that pipeline does *not* do is tint scene geometry — the sky-view LUT is a background-only lookup with no depth. Aerial perspective is the same march, but stopped at each froxel center's distance instead of the atmosphere edge, written into a `32³` volume the composite samples by scene depth. That is the industry-correct destination (Hillaire 2020, Frostbite): a `32×32×32` aerial-perspective volume out to the horizon, not the GPU-Gems-3 screen-space light-scattering post-process, which cannot tint occluded geometry and is deliberately not offered as a parallel path.

The volume is a **separate injection** from the Phase-3 fog grid, not a merge, and that separation is the correctness mechanism, not a shortcut. Fog handles near/mid participating media out to `fogFar` (~128 m); aerial perspective handles long-range Rayleigh/Mie planetary scattering out to the horizon (~32 km). If the atmosphere medium were injected into the fog grid it would be double-counted against the fog's own density; kept separate and composed as two independent multiplied media on one ledger (`T_total = T_fog·T_aerial`), the far scene is attenuated by each exactly once and neither darkens the other's in-scatter. Because the storage convention (`inScatter.rgb`, mean-`transmittance.a`) and the apply operator (`color·T + inScatter`) are identical to the fog integration volume's, the aerial-perspective volume is literally the fog volume with only the atmosphere medium injected — the natural unification point, extended from the existing infra.

> **camera → exponential-Z froxel centers → { fog inject·integrate → integration volume ; atmosphere LUT march → AP volume } → one shared-ledger composite → offscreen.** The two volumes stay independent media (never merged) and compose on a single transmittance product so there is no double darkening; the AP volume is a no-op identity write (`inScatter=0`, `T=1`) whenever the atmosphere source is off, so the ledger degrades cleanly to fog-only.

## NO-LEGACY checklist for this phase

- **`set-fog` is the one way to drive aerial perspective.** `aerial_perspective` + `aerial_intensity` live on `FogSettings` and reach the wire only through `SetFogParams`. There is no second toggle smuggled onto `SetAtmosphereParams`, and no `set-aerial-perspective` command — the atmosphere's own `set-atmosphere enabled` is the *gate* (no LUTs → identity AP write), never a duplicate owner of the AP state.
- **One volume for the atmosphere medium.** The AP volume is its own `acquire_image_3d` transient; the atmosphere medium is never also injected into the Phase-3 fog grid. One medium, one volume, one ledger term.
- **One ledger, extended in place.** The composite math lives only in `height_fog.slang`'s composite path (Phase 3). Phase 6 adds one AP sample and folds `T_aerial`/`inScatter_aerial` into the existing `T_total`/`L_out` — it does not add a second composite pass or a second apply operator.
- **All `SceneRenderer` impls migrated together.** `submit_fog` (Phase 1/3) grows the two AP fields on `FogRenderSettings`; the host impl and the `render_scene` test mock gain them in the same change — no stub left un-migrated.
- **Every wire change hits all tripwires in one change.** The two new `SetFogParams` fields, the hand-authored `FogSettingsDto` schema (`schema.rs`), the `component_block.ts`/`environment_dto.ts` interfaces, and the regenerated `openrpc.generated.json` byte-fixture (`tests/schema_fragments.rs`) move together — the build refuses to compile otherwise, by design.

## 1 — The aerial-perspective volume and its depth mapping

**File `engine/crates/rendering/src/froxel_fog.rs`.**

The froxel-fog module from Phase 2 already owns the exponential-Z slice↔view-Z helper (the CPU mirror locked by `froxel_grid_matches_shader`) and the `acquire_image_3d` path. Add the aerial-perspective volume next to the fog volumes: a single `32³` `rgba16f` transient (no ping-pong — the LUT march is stable frame to frame, so no temporal history is needed), plus the AP-specific far plane. Reuse the exponential-Z mapping *function* unchanged, instantiated with `AP_FAR` instead of `FROXEL_FAR`, so there is one mapping implementation with two instantiations rather than a second copy.

```rust
/// Aerial-perspective grid — Hillaire-2020's 32³ atmosphere volume.
pub const AP_GRID: u32 = 32;
/// AP far plane — the atmosphere horizon (km-scale), covering the far field
/// beyond the fog's `FROXEL_FAR`. Near reuses the fog near plane.
pub const AP_FAR_M: f32 = 32_000.0;

struct AerialPerspective {
    volume: TransientImage3D,        // acquire_image_3d, AP_GRID³, rgba16f
    params: AerialParamsUbo,         // sun dir + atmosphere physical params + AP_NEAR/AP_FAR + intensity
    set: vk::DescriptorSet,          // LUTs (transmittance, multiscatter) + params + storage volume
}

/// View-Z of an AP slice center — the same exponential mapping as the fog grid,
/// with the AP far plane. `slice_to_view_z(k, AP_GRID, near, AP_FAR_M)` is a
/// direct call into froxel_fog's shared helper, not a re-derivation.
fn ap_slice_view_z(k: u32, near: f32) -> f32 { /* shared helper, AP far */ }
```

> Decision to record when building: **share the exponential-Z mapping function, not the far plane.** The fog grid stops at ~128 m; the AP volume must reach the horizon or it is redundant with fog. Parameterizing the one `slice ↔ viewZ` helper by `(near, far)` keeps a single mapping code path (still covered by `froxel_grid_matches_shader`, extended with an AP-far case) while covering the correct range. A second hand-written mapping for AP is *not* offered — that is exactly the duplicate the shared helper exists to prevent.

## 2 — `aerial_perspective.slang`: ray-march the atmosphere LUTs

**File `engine/assets/shaders/aerial_perspective.slang` (new).**

Mirror the single-scatter loop in `atmos_skyview.slang` (`sampleTransmittance`, `sampleMultiScatter`, `rayleighPhase`, `hgPhase`, and the `radiance += transmittance * sInt; transmittance *= stepT;` accumulation), but bound the march at each froxel center's distance instead of the atmosphere edge. One thread per froxel: reconstruct the froxel-center world position from the inverse view-proj and the AP exponential-Z slice, march from the camera to that point sampling the LUTs, and write `(inScatter, meanTransmittance)` into the `32³` volume via a `RWTexture3D` (`StorageImageRwCompute`). No cluster light list, no shadow maps, no HG local-light term — only the sun through the atmosphere LUTs. This is `atmos_skyview.slang` minus the background framing plus a depth-bounded stop.

```hlsl
// aerial_perspective.slang — one thread per froxel center
[[vk::binding(0, 0)]] Sampler2D<float4> transmittanceLut;   // reuse ibl.rs sampler + transmittance view
[[vk::binding(1, 0)]] Sampler2D<float4> multiScatterLut;    //                    + multiscatter view
[[vk::binding(2, 0)]] RWTexture3D<float4> apVolume;         // rgb = in-scatter, a = mean transmittance

[numthreads(4, 4, 4)]
void computeMain(uint3 id : SV_DispatchThreadID) {
    float viewZ  = apSliceViewZ(id.z);                 // shared exponential-Z, AP far
    float3 wpos  = froxelCenterWorld(id.xy, viewZ);    // inverse view-proj
    // march camera -> wpos: rayleigh/mie sInt via sampleMultiScatter + sun sampleTransmittance,
    // phaseR = rayleighPhase(cosSun), phaseM = hgPhase(cosSun, mieAnisotropy);
    // radiance += transmittance * sInt;  transmittance *= stepT;   // lifted verbatim in shape
    apVolume[id] = float4(radiance * pc.aerialIntensity, dot(transmittance, float3(1,1,1)) / 3.0);
}
```

> Decision to record when building: **a dedicated march, not a re-parameterized sky-view sample.** The sky-view LUT is background-only (no distance); sampling it for geometry would give every far pixel the horizon color regardless of range and cannot desaturate correctly with distance. Marching the transmittance + multiscatter LUTs to the froxel distance is the Hillaire-2020 AP construction and the only one that yields the correct range-dependent blue-shift. The sky-view LUT is still used — but for the horizon-hue *tint* of the analytic fog (section 5), not for the AP volume.

## 3 — Expose the atmosphere LUTs to the AP pass

**File `engine/crates/rendering/src/ibl.rs`.**

The transmittance and multiscatter LUTs are private `IblImage` fields today; only `env_cube_view()`, `sampler()`, and `set()` are public. Add read accessors mirroring `env_cube_view()` (~l.629) so the AP descriptor set can bind the LUT views with the existing IBL sampler — no new sampler, no new bake step (the LUTs are already recorded by the atmosphere chain at ~l.977–1004).

```rust
impl Ibl {
    pub fn transmittance_view(&self) -> vk::ImageView { self.transmittance_lut.view }
    pub fn multi_scatter_view(&self) -> vk::ImageView { self.multi_scatter_lut.view }
    pub fn sky_view_view(&self)      -> vk::ImageView { self.sky_view_lut.view }   // used by §5 tint
    // sampler() already exposes the shared clamp sampler.
}
```

> Decision to record when building: **read-only accessors, not a new descriptor set on `Ibl`.** The AP pass owns its own set (bound in `froxel_fog.rs`); `Ibl` just hands out the views + the existing `sampler()`. This keeps the LUT lifetime with `Ibl` (freed in its `Drop`) and avoids a second sampler for images that already have one.

## 4 — The shared transmittance ledger

**File `engine/assets/shaders/height_fog.slang`.**

Extend the Phase-3 composite (which already reads `ViewTarget.depth` via `SampledReadCompute` and blends into `ViewTarget.offscreen`) with one AP sample and one ledger term. Sample the AP volume at `(screenUV, w = log(viewZ / apNear) / log(apFar / apNear))` trilinear, then fold it into the existing `T_total`/`L_out` using the shared `(inScatter.rgb, transmittance.a)` convention.

```hlsl
// height_fog.slang composite — shared ledger
float4 fogI = integrationVolume.SampleLevel(fogUV3, 0);   // Phase 3: rgb = inScatter_fog, a = T_fog
float4 apI  = apVolume.SampleLevel(apUV3, 0);             // Phase 6: rgb = inScatter_ap,  a = T_ap
float  T    = fogI.a * apI.a;                             // one transmittance ledger
float3 L    = scene * T + fogI.rgb + fogI.a * apI.rgb;    // scene·T_total + inScatter_fog + T_fog·inScatter_aerial
// analytic height fog carries anything beyond both: T *= T_height; L += (1 - T_height) * heightInscatter;
offscreen[px] = float4(L, 1.0);
```

> Decision to record when building: **`inScatter_aerial` is pre-attenuated by `T_fog`, not added raw.** Aerial perspective sits *behind* the near/mid fog along the view ray, so its in-scatter must be dimmed by the fog transmittance in front of it (`T_fog·inScatter_aerial`); adding it raw would let far planetary scattering punch through dense near fog. This ordering (far medium behind near medium on one ledger) is the same UE/Frostbite composition order and is why the two volumes must stay separate rather than summed.

## 5 — Horizon-hue coherence (finalize the Phase-1 stub)

**File `engine/assets/shaders/height_fog.slang`.**

Phase 1 left the analytic height-fog inscatter as a flat tint with a stub hook for the sky-view LUT. Finalize it here: sample `sky_view_lut` in the view direction (the same LUT the sky background uses) and tint the analytic inscatter with it, gated by `fog.enabled` — no new field. This makes near fog redden toward the sun at sunset exactly as the sky does, so near fog, mid haze, and far aerial perspective read as one continuous medium.

> Decision to record when building: **the tint is a shader behavior, not a control setting.** It has no artistic knob of its own — it exists to keep fog and sky in agreement, so exposing a toggle would only let them disagree. It is driven entirely by `fog.enabled` + the live atmosphere state, keeping the write surface at exactly `set-fog`.

## 6 — Pass ordering

**File `engine/crates/rendering/src/renderer.rs`.**

Insert the AP fill between `fog_integrate` and the fog composite, following the `global_sdf.rs` precedent for a 3D storage-image compute pass driven through the graph (`Image3D` + `STORAGE_IMAGE` GENERAL write + `import_image_3d` + `cmd_dispatch(gx, gy, gz)`). Use Phase 2's `add_compute_pass` with `groups_z` (the `32³` grid dispatches `8×8×8` groups at `[numthreads(4,4,4)]`). The AP fill only reads the atmosphere LUTs and writes its own volume, so it has no ordering dependency on the fog inject/integrate — it may run concurrently — but it must complete before the composite reads it.

```
light-cull → fog_inject → fog_integrate ─┐
                       aerial_perspective ┴→ fog composite (shared ledger) → offscreen
                                            (all before motion / TAA / bloom / tonemap)
```

> Decision to record when building: **AP stays strictly pre-bloom, like the rest of the fog stage.** Aerial perspective attenuates and blue-shifts distant bright emitters *before* the energy-conserving bloom pyramid reads the HDR, so far highlights bloom through the already-scattered atmosphere rather than at full strength — the same load-bearing ordering the planset fixes for fog.

## 7 — Scene state and the coherence flag

**File `engine/crates/scene/src/environment.rs`.**

Add two fields to `FogSettings` (the struct Phase 1 introduced next to `AtmosphereSettings` on `SceneEnvironment`): `aerial_perspective: bool` (default `false`) and `aerial_intensity: f32` (default `1.0`). No change to `AtmosphereSettings` — the atmosphere's `enabled` already gates whether the LUTs exist, which is the natural precondition for AP, not a second owner of the toggle.

```rust
pub struct FogSettings {
    // … Phase 1/3/4 fields …
    /// Tint distant geometry with Hillaire-2020 aerial perspective (needs an active atmosphere).
    pub aerial_perspective: bool,
    /// Aerial-perspective strength multiplier.
    pub aerial_intensity: f32,
}
```

## 8 — Serde and persistence

**File `engine/crates/scene/src/serde.rs`.**

Add the two keys to `fog_to_json`/`fog_from_json` (the pair Phase 1 wired into `environment_to_json`/`environment_from_json`), so the project `environment` block round-trips them with no change to `document.rs`. Update the frozen-key round-trip unit test in the same change.

## 9 — Protocol DTO and command table

**File `engine/crates/protocol/src/dto.rs`.** Add `aerial_perspective: Option<bool>` and `aerial_intensity: Option<f32>` to `SetFogParams` (the Phase-1 DTO that mirrors `SetAtmosphereParams`), keeping the `json` escape hatch + per-field `Option` shape. The result stays `EnvironmentDto`.
**File `engine/crates/protocol/src/command.rs`** — no new `COMMANDS` row (`set-fog` already exists from Phase 1); the `COMMAND_FIXTURES` fog fixture may gain the two fields so the golden manifest exercises them. No `DTO_TYPE_NAMES` or `scene_domain()` change (`SetFogParams` is already registered).
**File `engine/crates/protocol/src/schema.rs`** — extend the hand-authored `FogSettingsDto` schema (mirroring the `AtmosphereSettingsDto` object at ~l.576) with `aerialPerspective` + `aerialIntensity` properties.
**Files `engine/xtask/src/protocol/environment_dto.ts` + `component_block.ts`** — add the two fields to the hand-authored `FogSettingsDto` interface (the Rust DTO is opaque `{value: Value}`, so the TS shape is hand-written here).
**File `engine/crates/protocol/tests/schema_fragments.rs`** — the byte-equivalence check against `openrpc.generated.json` fails until `cargo run -p xtask -- gen-protocol` regenerates the committed artifacts; regenerate in the same change.

> Decision to record when building: **extend `SetFogParams`, do not add a command.** Aerial perspective is fog-ledger state; a `set-aerial-perspective` command would split the fog write surface across two commands. Two `Option` fields on the existing DTO keep one command, one handler, one write path.

## 10 — Control seam

**File `engine/crates/control/src/commands_scene.rs`.**

The `set-fog` handler (Phase 1, cloned from `set-atmosphere` at ~l.834) already merges a partial `SetFogParams` onto the environment fog block, bumps `scene_version`, and returns `environment_dto(ctx)`. It picks up the two new fields for free — the only work is validating `aerial_intensity >= 0.0` and returning `Err(Error::command(...))` on a bad value. The `sa` CLI needs no per-command code; `sa set-fog --aerial-perspective true --aerial-intensity 1.2` flows through the generated manifest.

## 11 — Renderer drive and editor

**File `engine/crates/assets/src/render_scene.rs`.** Extend `FogRenderSettings` (the struct `submit_fog` already takes) with `aerial_perspective: bool` + `aerial_intensity: f32`, populate them from `scene.environment.fog`, and read `scene.environment.atmosphere.enabled` to decide whether the AP fill runs at all (identity write when off). Add the two fields to the mock `SceneRenderer` stub in the test harness in the same change.

- **Regenerate:** `cargo run -p xtask -- gen-protocol`. **Never hand-edit** `editor/src/protocol/sa-types.ts`.
- **File `tools/check-control-schema/check.ts`** — extend the `set-fog` `paramsForFixture()` case with the two fields.
- **File `editor/src/control/client.ts`** — no change; `client.setFog(patch)` (Phase 1) already accepts `Partial<Environment['fog']>`, and the regenerated `SetFogParams` gains the fields automatically.
- **File `editor/src/panels/EnvironmentPanel.tsx`** — add two rows to the Fog section (cloned from the Atmosphere block): a `Switch` for `aerialPerspective` and a `NumberDrag` for `aerialIntensity`, routed through the existing `fogCoalescers`/`patchFog`/`recordFogEdit` trio with one `pushEdit(..., "scene")` per gesture.

## Out of scope (later phases)

- **The AP grid resolution is fixed at `32³`.** A quality-scaled AP volume (matching the fog `quality` tier) is deferred — `32×32×32` is Hillaire's production size and is not the bottleneck; wiring it to the tier is a trivial later refinement, not this phase's concern.
- **No AP temporal reprojection.** The LUT march is frame-stable, so the AP volume is a plain per-frame transient with no history — the Phase-4 reprojection machinery is deliberately *not* extended to it. If ground-truth multiple-scattering (rather than the isotropic multiscatter LUT) is ever wanted, that is a Future atmosphere upgrade, not an AP-volume change.
- **Transparents.** Transparent surfaces sample the fog integration volume in their forward pass (Phase 3); folding the AP volume into that forward path is left for the same future pass that unifies transparent volumetric sampling — this phase leaves the forward seam untouched.

## Known interactions (note, don't over-engineer)

- **Atmosphere disabled.** When `scene.environment.atmosphere.enabled` is false there are no baked LUTs; the AP fill writes identity (`inScatter=0`, `T=1`) so the ledger collapses to fog-only. Gate the *dispatch* on the flag to skip the cost entirely, but keep the composite branch cheap and always-correct rather than special-casing the shader.
- **AP without fog.** `aerial_perspective` on with `fog.enabled` off is valid — the composite runs with `T_fog=1`, `inScatter_fog=0`, so `L_out = scene·T_aerial + inScatter_aerial`. The horizon-hue tint (§5) simply does not apply (it is a fog behavior). Do not couple the two flags.
- **Double-darkening probe.** The single-ledger design (`T_total = T_fog·T_aerial`) is the guard against double darkening; verify it explicitly (section in the milestone gate) rather than trusting the math — a region inside both fog and AP must attenuate by the product exactly once.
- **Intensity is a coherence knob, not an exposure.** `aerial_intensity` scales only the AP in-scatter; leave global exposure to the tonemap pass. A large intensity is a stylistic push, not a fix for a dim scene.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Boot a fixture with an active atmosphere and distant geometry — a horizon vista under `EnvSource::Atmosphere` — headless (`just run-engine-headless`, NVIDIA GPU) with a validation-clean log, and confirm distant geometry blue-shifts and desaturates with distance coherently with the sky behind it, and that a region where near fog and aerial perspective overlap shows no double darkening (attenuation matches `T_fog·T_aerial`). Check `sa set-fog --aerial-perspective true` and `sa set-fog --aerial-intensity 1.5` visibly change the distant tint, and `sa set-atmosphere --enabled false` collapses AP to fog-only. Because a wire type changed, run `bun run check` in `editor/` (regenerates `@saffron/protocol`; `SetFogParams` must gain `aerialPerspective`/`aerialIntensity`; never hand-edit `sa-types.ts`) and `just e2e`, adding a `tests/e2e` case that drives `set-fog` with the two fields and asserts the `EnvironmentDto` echo plus a validation-clean log. Add the new `docs/content/` **Aerial perspective** concept page (the Hillaire-2020 AP volume, the shared transmittance ledger, why it is a separate injection from the fog grid, and the `What | File | Symbols` table: `engine/assets/shaders/aerial_perspective.slang`, `ibl.rs` `transmittance_view`/`multi_scatter_view`, `froxel_fog.rs` AP volume, `height_fog.slang` composite, `SetFogParams`), add its hub `_index.md` row, and cross-link it from the existing atmosphere page — all in the same change.
