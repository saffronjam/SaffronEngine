# Phase 4 — Volumetric cloud shape: noise fields, weather map, and cloud-type profiles

**Status:** COMPLETED

Part of `plans/sky-and-volume/` — the dynamic-sky / time-of-day / volumetric-cloud planset. This is the
**greenfield root of the cloud track** and the one phase that touches no lighting: it authors cloud
*shape* — the tiling 3D noise fields, the weather map, and the analytic density-vs-height profiles — and
ships a debug view that renders the raw density so the shaping is verifiable **before any lit raymarch
exists**. It depends on nothing (`dependsOn: —`) and runs in parallel with the dynamic-sky track (Phases
1–3); it deliberately does **not** light, march-with-scattering, temporally reconstruct, or composite the
clouds onto the atmosphere. The energy-conserving lit raymarch is Phase 5; folding `T_cloud` onto the
existing `height_fog.slang` transmittance ledger, cloud shadows, god-rays, and the wind field is Phase 6.

Crucially, this phase **reuses** the volumetric subsystem that already shipped (the `plans/volumetric/`
planset, `COMPLETED`) rather than rebuilding any of it. The `Image3D` type + the one-shot device-local
noise bake (`froxel_fog.rs::bake_noise_volume`), the `import_image_3d` render-graph path, the 3D compute
dispatch (`add_compute_pass` `groups_z`), and the `SHADER_READ_ONLY_OPTIMAL`-resting persistent-volume
pattern (the aerial-perspective `AP_GRID` volume) are all consumed as-is. The Hillaire sky LUT chain and
the 32³ aerial-perspective froxel are **not** re-planned and **not** touched here — the cloud density
field is a pure shape function with no atmosphere coupling yet, so there is no transmittance ledger to
double-count against in this phase (that reconciliation is Phase 6, and §4 states exactly why the debug
view cannot introduce it early).

## Goal

Author a physically-motivated cloud density field and prove it renders correctly in isolation. The
industry-correct shape model (Guerrilla's Nubis lineage, ported by every modern engine) is a low-frequency
**Perlin-Worley base** dilated into billows, **eroded** at the edges by higher-frequency Worley and warped
by a **curl** field, revealed only *inside* an analytic **density-vs-height envelope** that a **weather
map** selects per world-XZ location — so noise decides *where within the envelope* mass appears, never the
overall silhouette. Concretely:

- **`CloudSettings` on `SceneEnvironment`** — a new nested scene-state block mirroring `FogSettings`
  end-to-end (coverage, cloud type, precipitation, layer altitude/height, base/detail/curl scale, and
  weather-map params), round-tripped through `environment_to_json`/`environment_from_json`, driven by one
  `set-clouds` merge command registered across *every* protocol tripwire, and surfaced in the Environment
  panel. It is scene state, not `RenderSettings` — authored per-scene like the atmosphere and fog.
- **The canonical tiling noise volumes** — a `128³` RGBA base (`R` = Perlin-Worley, `GBA` = Worley at
  rising frequencies), a `32³` Worley erosion volume, and a `128²` curl field, baked once at init into
  persistent `Image3D`s following the `bake_noise_volume` precedent, sampled through a linear-**repeat**
  sampler so any world scale tiles seamlessly.
- **A weather map** — a `128²` (or larger) 2D image whose channels drive coverage / precipitation / cloud
  type, filled procedurally from world-XZ Perlin coverage with a painted-override seam left clean.
- **The density sampler** — a shared shader function `sampleCloudDensity(worldPos, …)` that builds a
  **dimensional profile** by blending stratus / cumulus / cumulonimbus height gradients by cloud type ×
  coverage, then reveals mass with `cloud_density = saturate(noise − (1 − dimensional_profile))` — the
  load-bearing **value-erosion**, never a multiply (a multiply would gut the dense core). Both the debug
  view and the Phase-5 lit raymarch import this one function.
- **A cloud-density debug `ViewMode`** — a `ViewMode::CloudDensity` variant that renders accumulated
  coverage/density as grayscale over the existing `set-view-mode` path, so shaping is inspectable with no
  lighting, no temporal reconstruction, and no composite.

## Design stance (grounded in current engine practice)

The volumetric subsystem already built every primitive this phase needs. Persistent 3D density volumes,
device-local one-shot bakes, `import_image_3d`, 3D compute dispatch, and the `set-fog`/`FogSettings`
vertical slice are all in-tree and load-bearing. So this phase is *shape authoring + a new scene-state
block*, riding proven infra — not novel plumbing — with one genuinely new asset class (multi-channel
tiling cloud noise) and one new shared shader function (the density sampler).

| What (reused, not rebuilt) | File | Symbols |
|---|---|---|
| One-shot device-local noise bake + persistent `Image3D` | `engine/crates/rendering/src/froxel_fog.rs` | `bake_noise_volume`, `Image3D`, `create_compute_layout`, `init_transition_volumes` |
| The 3D image type + `TYPE_3D` view | `engine/crates/rendering/src/resources.rs` | `Image3D::new`, `Image3D::handle`, `Image3D::view`, `Image3D::layout` |
| Render-graph 3D import + external layout writeback | `engine/crates/rendering/src/render_graph.rs` | `import_image_3d`, `import_image`, `alloc_external_layout`, `external_layout`, `RgUsage::SampledReadCompute`, `RgUsage::StorageImageRwCompute` |
| Persistent-volume resting-layout pattern (the AP volume) | `engine/crates/rendering/src/froxel_fog.rs` | `AerialPerspective` (fixed `AP_GRID` `Image3D`, rests `SHADER_READ_ONLY_OPTIMAL`, set written once) |
| Transient-3D acquire path (Phase-5's reduced-res buffer, **not** needed here) | `engine/crates/rendering/src/transient.rs` | `acquire_image_3d`, `TransientImage3D`, `FROXEL_VOLUME_KEYS` |

**Shape, not lighting — the Nubis density model with value-erosion.** The modern-correct destination is a
raymarched procedural volume whose *shape* is an analytic height envelope selected by a weather map and
eroded by tiling noise (Schneider & Vos, *Horizon Zero Dawn*, SIGGRAPH 2015; Schneider, *Nubis Evolved*,
SIGGRAPH 2022) — **not** stacked fBm × a single height ramp (the ShaderToy shape, which has no governing
form and no billows), and **not** noises multiplied together (which destroys density at the core). Noise
is combined into the profile by `remap()` / value-erosion so high-frequency detail only *subtracts at the
edges*, preserving a dense interior — the single most-copied Nubis lesson. This phase ships that shape
model and stops there; the lit march is Phase 5.

**Author noise as tiling channel-packed 3D textures baked once, not per frame.** The base shape is one
`128³` RGBA volume (`R` = Perlin-Worley, `GBA` = Worley at rising frequencies), erosion is a `32³` Worley
volume, and turbulence is a `128²` curl field — the exact HZD-2015 channel pack (Hillaire's open
`TileableVolumeNoise` is the reference generator). These are static tiling assets, so they are baked once
at init into persistent `Image3D`s exactly like the fog erosion noise, and sampled through a linear-repeat
sampler. This is a *new* asset (multi-channel cloud noise), distinct from the fog's single-channel `R8`
erosion volume — the two coexist because they serve different consumers, not a NO-LEGACY duplicate.

**The debug view is an isolated, unlit density integration — it introduces no transmittance ledger.**
Because this phase has no lighting and no atmosphere coupling, the `CloudDensity` view is a debug-only
coarse fixed-step accumulation of the *raw density* (Beer-Lambert opacity, `coverage = 1 − exp(−Σ σ dt)`,
no scattering, no phase function, no light march) written as grayscale over the scene, stopping the march
at opaque depth. It is explicitly **not** the Phase-5 lit raymarch and it does **not** touch
`height_fog.slang`, the aerial-perspective volume, or the `(inscatter, transmittance)` composite — there
is nothing to double-count because nothing is composited. The debug pass and the Phase-5 raymarch call the
*same* `sampleCloudDensity` function, so the shape they show is byte-identical; only the integration
differs (unlit opacity here, energy-conserving scatter in Phase 5).

> **weather map (world-XZ coverage/precip/type) → dimensional profile (height gradients blended by type ×
> coverage) → value-erosion by the `128³`/`32³` noise + `128²` curl warp → `sampleCloudDensity(worldPos)`;
> the `CloudDensity` debug pass integrates that density unlit into grayscale over `color`.** No lighting,
> no march-with-scattering, no composite onto the atmosphere ledger — those are Phases 5 and 6, and the
> density-sampler seam is left clean for both (and for the Nubis-3 sparse-voxel end-state).

## NO-LEGACY checklist for this phase

- `set-clouds` is **the** command for scene-wide cloud state — one `SetCloudsParams` DTO, one merge
  handler, one `CloudSettings` block. No cloud knob is smuggled onto `set-environment`/`set-atmosphere`,
  no second cloud command, and the reply reuses the opaque `EnvironmentDto` (never a bespoke result DTO),
  exactly as `set-fog`/`set-atmosphere` do.
- The density sampler lives in **exactly one** shader function (`sampleCloudDensity` in a shared
  `clouds.slang` include). The Phase-4 debug pass and the Phase-5 lit raymarch both `import clouds` and
  call it — there is no second density evaluation copied into the raymarch (the drift the shared-`lighting`
  module extraction removed in the fog planset is not re-introduced here).
- The weather map is sampled through **one** fetch: the procedural fill writes the weather-map image, and a
  painted override (a `weatherTexture` asset) is imported by blitting into that *same* image, so the
  sampler never branches on source. One weather-map resource, one sample path.
- Cloud state is `SceneEnvironment` state round-tripped in the project `environment` block via
  `environment_to_json`/`from_json` — **not** a `RenderSettings` field and **not** a `ControlRenderer`
  setter — mirroring the atmosphere and fog.
- `SetCloudsParams` and its `CloudSettingsDto` schema are registered in *all* protocol tripwire lists
  (`dto.rs`, `command.rs` `COMMANDS`/`COMMAND_FIXTURES`/`DTO_TYPE_NAMES`/`scene_domain`, `codegen.rs`
  `ts_decls`/`struct_fragments`, `schema.rs` `CloudSettingsDto` + `Environment.cloud` + `required`,
  `tests/inventory.rs`, `tests/schema_fragments.rs`, and the regenerated `schemas/control/*.generated.json`)
  in one change — the build and the schema-fragment byte-equivalence test refuse to compile otherwise.
- `ViewMode::CloudDensity` is added to the `ViewMode` enum, the `ViewModeDto` enum, and *both* mapping arms
  in `commands_render.rs` in the same change — no half-registered view mode.
- The Phase-4 debug pass is a lasting debug visualizer (like `ViewMode::Fog`), **not** a throwaway
  superseded by Phase 5; Phase 5 adds the production lit cloud pass beside it and both share
  `sampleCloudDensity`, so no code path is retired or duplicated.

## 0 — Foundation: `CloudSettings` scene state + the `set-clouds` vertical slice

Build the authoring surface first — it is the spine every later section writes through, and it mirrors the
`FogSettings`/`set-fog` slice exactly.

**File `engine/crates/scene/src/environment.rs`.** Add a `CloudSettings` struct with a hand-written
`Default` and a `pub cloud: CloudSettings` field on `SceneEnvironment` (+ its `Default`). Cloud *type* is a
continuous `f32` in `[0, 1]` (stratus → cumulus → cumulonimbus), not an enum — Nubis blends profiles by a
continuous type value, so a continuous field is both modern-correct and one fewer wire enum.

```rust
/// Scene-wide volumetric cloud shape (no lighting — Phase 4 authors the density field; the lit
/// raymarch is Phase 5, the atmosphere-ledger fold Phase 6). One layer, procedurally shaped by a
/// weather map and eroded by tiling noise; `cloud_type` blends stratus→cumulus→cumulonimbus profiles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudSettings {
    /// Whether the cloud density field is evaluated (the debug view + later raymarch).
    pub enabled: bool,
    /// Global coverage bias multiplied onto the weather map's coverage channel (0 = clear, 1 = overcast).
    pub coverage: f32,
    /// Cloud-type bias: stratus (0) → cumulus (0.5) → cumulonimbus (1); selects the height envelope.
    pub cloud_type: f32,
    /// Precipitation bias (weather-map precip channel) — reserved for Phase-5/6 lighting/density; 0 here.
    pub precipitation: f32,
    /// Cumulonimbus anvil spread near the layer top (0 = none).
    pub anvil_bias: f32,
    /// Cloud-layer bottom altitude (world-up metres).
    pub layer_altitude: f32,
    /// Cloud-layer thickness (metres); the profile spans `[altitude, altitude + height]`.
    pub layer_height: f32,
    /// World→base Perlin-Worley frequency (smaller = larger cloud forms).
    pub base_scale: f32,
    /// World→detail Worley frequency (edge erosion granularity).
    pub detail_scale: f32,
    /// Detail-erosion strength (0 = off, ~0.35 default).
    pub detail_strength: f32,
    /// Curl-warp displacement applied to the detail lookup (world metres).
    pub curl_strength: f32,
    /// World-XZ→weather-map frequency.
    pub weather_scale: f32,
    /// Weather-map scroll offset (xz used; y ignored) — the clean seam Phase-6 wind advances.
    pub weather_offset: Vec3,
    /// Painted weather-map override asset (0 = procedural fill).
    pub weather_texture: Uuid,
}
```

Reasonable defaults: `enabled: false`, `coverage: 0.5`, `cloud_type: 0.4`, `precipitation: 0.0`,
`anvil_bias: 0.0`, `layer_altitude: 1500.0`, `layer_height: 2500.0`, `base_scale: 8e-5`,
`detail_scale: 1e-3`, `detail_strength: 0.35`, `curl_strength: 120.0`, `weather_scale: 2e-5`,
`weather_offset: Vec3::ZERO`, `weather_texture: Uuid(0)`.

**File `engine/crates/scene/src/serde.rs`.** Add `cloud_to_json`/`cloud_from_json` (a per-field camelCase
round-trip beside `atmosphere_*`/`fog_*`, each `from` half supplying the struct default when absent), and
add `("cloud", cloud_to_json(&env.cloud))` to `environment_to_json` and
`if let Some(v) = field(j, "cloud") { env.cloud = cloud_from_json(v); }` to `environment_from_json`. The
document save/load picks it up automatically through those two functions.

**File `engine/crates/protocol/src/dto.rs`.** Add `SetCloudsParams` — the `set-fog` shape: a leading
`json: Option<Value>` escape hatch, then one `Option<T>` per `CloudSettings` field
(`weather_texture: Option<Uuid>`, `weather_offset: Option<Vec3>`, `enabled` via `coerce::opt_boolean`), with
the full `#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]` +
`#[serde(rename_all = "camelCase")]` + `#[ts(export)]` stack. The reply is the existing `EnvironmentDto` —
no new result DTO, no new enum (cloud type is `f32`).

**File `engine/crates/control/src/commands_scene.rs`.** Register `set-clouds` in `register_scene_commands`
with `reg.register::<SetCloudsParams, EnvironmentDto>("set-clouds", …, |ctx, params| …)`, following the
`set-fog` handler verbatim: `environment_to_json(current)` → merge `params.json` first → overwrite each
`Some` field into the `cloud` sub-object → `environment_from_json` → assign `active_scene().environment` →
bump `scene_version` → return `environment_dto(ctx)`. Validate ranges the way `set-fog` validates
`historyBlend`/`lightClamp` (e.g. `coverage`/`cloud_type`/`precipitation`/`anvil_bias` ∈ `[0, 1]`,
`layer_height > 0`) with `Error::command(...)`. The `sa` CLI needs no per-command code —
`sa set-clouds --enabled true --coverage 0.6` flows through the generated manifest.

## 1 — The tiling cloud-noise volumes

**File `engine/crates/rendering/src/clouds.rs` (new).** Add a `Clouds` resource that owns the persistent
noise + weather assets, its host-mapped `CloudParams` UBO, the samplers, and the debug-pass descriptor set
— mirroring `AerialPerspective`'s shape (fixed-size persistent `Image3D`s, set written once, volumes rest
in `SHADER_READ_ONLY_OPTIMAL`). Bake three tiling noise fields once at init:

| Volume | Extent / format | Channels | Purpose |
|---|---|---|---|
| Base shape | `128³` `R8G8B8A8_UNORM` `Image3D` | `R` = Perlin-Worley, `GBA` = Worley rising freq | low-freq billowed base |
| Detail erosion | `32³` `R8G8B8A8_UNORM` `Image3D` (RGB used) | Worley at three rising freqs | edge erosion into wisps |
| Curl | `128²` `R8G8B8A8_UNORM` `Image` (2D) | `RG` = curl vector field | turbulent warp of the detail lookup |

The modern-correct bake is a **GPU compute** dispatch (Perlin-Worley and Worley are cheap, tileable, and
`128³×4` channels is too much to fill on the CPU at startup), run once through a one-shot command buffer +
`device.wait_idle()` and rested in `SHADER_READ_ONLY_OPTIMAL` — the exact lifecycle of
`bake_noise_volume`, generalized from a single-channel `R8` staged CPU upload to a multi-channel storage
`Image3D` compute fill. Add three tiny bake shaders (`cloud_noise_base.slang`, `cloud_noise_detail.slang`,
`cloud_curl.slang`), each `computeMain` writing an `RWTexture3D`/`RWTexture2D` storage image, dispatched
`groups_z` for the 3D fills; the periodic lattice must divide the volume edge so the fields tile seamlessly
under the linear-repeat sampler (the `tiling_fbm` wrap the fog noise already guarantees, ported to the GPU
Perlin/Worley basis). Reuse `create_compute_layout`, `init_transition_volumes`, and the linear-repeat
sampler pattern from `froxel_fog.rs`.

The `remap` primitive and the Perlin-Worley combine are the shared shaping backbone (Schneider & Vos,
HZD 2015; Hillaire, *TileableVolumeNoise*):

```hlsl
// clouds.slang (shared include) — the shaping primitives
float remap(float v, float o0, float o1, float n0, float n1) {
    return n0 + saturate((v - o0) / (o1 - o0)) * (n1 - n0);
}
// bake: R channel of the 128^3 base = Perlin dilated by inverted Worley into billows
float perlinWorley = remap(perlin, 1.0 - worleyLow, 1.0, 0.0, 1.0);
```

> Decision to record when building: **the cloud noise is a new persistent `Image3D` set baked once at
> init, not a per-frame transient and not a reuse of the fog `bake_noise_volume` `R8` volume.** The fog
> erosion noise (single-channel `R8`, `64³`) and the cloud shape noise (`128³` RGBA + `32³` + `128²` curl)
> are distinct fields for distinct consumers; they coexist. The transient-3D path
> (`acquire_image_3d`/`TransientImage3D`) is *not* used this phase — it is Phase-5's reduced-res march
> buffer, and this phase leaves it untouched.

## 2 — The weather map

**Files `engine/crates/rendering/src/clouds.rs` + `engine/assets/shaders/cloud_weather.slang` (new).** Hold
one `128²` (or `256²`) 2D weather-map `Image` (`R8G8B8A8_UNORM`, `R` = coverage, `G` = precipitation,
`B` = cloud type) that the density sampler reads by world XZ. Fill it procedurally with a world-XZ Perlin
coverage field (multi-octave, biased by `CloudSettings::coverage`/`cloud_type`/`precipitation`), refilling
only when the weather params change (a cheap 2D compute dispatch, gated by a dirty flag — the same
cost-asymmetric refresh the atmosphere bake uses). The painted-override seam: when
`CloudSettings::weather_texture != 0`, blit the painted asset into this *same* weather-map image at load, so
the density sampler always samples one resource through one fetch (no source branch — the NO-LEGACY
one-path rule).

| What | File | Symbols |
|---|---|---|
| Weather-map image + fill/override | `engine/crates/rendering/src/clouds.rs` | `Clouds` (`weather_map: Image`, `weather_dirty`), `refill_weather`, `blit_painted_override` |
| Procedural coverage/precip/type fill | `engine/assets/shaders/cloud_weather.slang` | `computeMain` (world-XZ Perlin → `RGB`) |

The channel semantics follow Nubis-2017 / Unity HDRP's cloud map: `R` biases how much the noise is revealed
(coverage), `G` carries precipitation (drives density/darkening in Phases 5/6), `B` selects the height
envelope horizontally. Phase 4 exercises coverage and type; precipitation is authored and round-tripped but
only reserved here.

## 3 — The density sampler

**File `engine/assets/shaders/clouds.slang` (shared include, new).** The heart of the phase: one function
`sampleCloudDensity(float3 worldPos, CloudParams p, …)` that both the debug pass and the Phase-5 raymarch
import. It (1) rejects positions outside the layer slab `[layer_altitude, layer_altitude + layer_height]`,
(2) samples the weather map by world XZ for `(coverage, precip, type)`, (3) builds the **dimensional
profile** — an analytic density-vs-height envelope blended from stratus/cumulus/cumulonimbus gradients by
`cloud_type × coverage`, (4) samples the `128³` base for the billowed shape, warps the `32³` detail lookup
by the `128²` curl field, and (5) reveals mass by **value-erosion**.

The height gradients are two-`remap` presets selected by type, the anvil biases coverage near the layer top,
and the reveal is the load-bearing subtract-not-multiply (Nubis 2017 remap shaping; Nubis Evolved 2022
value-erosion):

```hlsl
// clouds.slang — sampleCloudDensity (shape only; Phase 5 adds lighting on top of this same result)
float heightGradient(float h, float type) {
    // h in [0,1] over the layer; blend stratus (flat/low), cumulus (billowed/mid), cumulonimbus (tall)
    float stratus     = remap(h, 0.0, 0.10, 0, 1) * remap(h, 0.20, 0.30, 1, 0);
    float cumulus      = remap(h, 0.00, 0.25, 0, 1) * remap(h, 0.60, 0.95, 1, 0);
    float cumulonimbus = remap(h, 0.00, 0.10, 0, 1) * remap(h, 0.80, 1.00, 1, 0);
    float a = saturate(type * 2.0);          // stratus → cumulus
    float b = saturate(type * 2.0 - 1.0);    // cumulus → cumulonimbus
    return lerp(lerp(stratus, cumulus, a), cumulonimbus, b);
}

float sampleCloudDensity(float3 worldPos, CloudParams p) {
    float h = (worldPos.y - p.layerAltitude) / p.layerHeight;    // [0,1] within the slab
    if (h < 0.0 || h > 1.0) return 0.0;

    float3 wm = weatherMap.SampleLevel(linearRepeat, worldPos.xz * p.weatherScale + p.weatherOffset.xz, 0).rgb;
    float coverage = saturate(wm.r * p.coverage);
    float type     = saturate(wm.b + p.cloudType);

    // dimensional profile: the height envelope scaled by coverage (anvil widens the top for cb)
    float profile = heightGradient(h, type) * coverage;
    profile = pow(profile, remap(h, 0.7, 0.8, 1.0, lerp(1.0, 0.5, p.anvilBias)));   // anvil

    // billowed base shape (Perlin-Worley R dilated by Worley GBA), combined by remap not multiply
    float4 base = baseNoise.SampleLevel(linearRepeat, worldPos * p.baseScale, 0);
    float shape = remap(base.r, dot(base.gba, float3(0.625, 0.25, 0.125)) - 1.0, 1.0, 0.0, 1.0);

    // reveal mass only inside the profile: value-erosion (subtract), the dense core is preserved
    float clouds = saturate(shape - (1.0 - profile));

    // curl-warped high-frequency edge erosion
    float2 curl = curlNoise.SampleLevel(linearRepeat, worldPos.xz * p.baseScale, 0).rg * 2.0 - 1.0;
    float3 dp   = worldPos + float3(curl.x, 0.0, curl.y) * p.curlStrength;
    float3 det  = detailNoise.SampleLevel(linearRepeat, dp * p.detailScale, 0).rgb;
    float  erosion = dot(det, float3(0.625, 0.25, 0.125)) * p.detailStrength;
    clouds = saturate(remap(clouds, erosion * (1.0 - clouds), 1.0, 0.0, 1.0));      // erode edges only

    return clouds;
}
```

> Decision to record when building: **mass is revealed by value-erosion (`saturate(shape − (1 − profile))`),
> never by multiplying the profile into the noise.** A multiply darkens the dense core to nothing as
> coverage rises; the subtract inflates clouds toward a solid interior as `coverage → 1` and only carves at
> the edges. This is the single most-copied Nubis lesson and the reason the profile is authored as an
> envelope the noise fills rather than a mask the noise multiplies.

> Decision to record when building: **`sampleCloudDensity` is the one density code path.** The debug pass
> (§4) and the Phase-5 lit raymarch both call it; the raymarch adds lighting *around* the returned density,
> it does not re-derive shape. The `CloudParams` UBO the sampler reads is filled from `CloudSettings` via
> the `submit_clouds` seam (§4), so scene state drives shape with no second source.

## 4 — Cloud-density debug ViewMode + graph wiring

**File `engine/crates/rendering/src/renderer.rs`.** Add a `ViewMode::CloudDensity` variant alongside
`ViewMode::Fog` (its `debug_channel()` returns `0` — it is produced by a dedicated pass, like `Fog` and
`MotionVectors`, not the mesh debug channel). Add a `Clouds` field to the renderer, an
`add_cloud_debug_pass` gated on `self.view_mode == ViewMode::CloudDensity`, and a `submit_clouds` push.

The debug pass is a fullscreen compute pass inserted in the post window (after the scene resolve, before
bloom/tonemap — the same slot `add_fog_pass` occupies), reading scene depth (to stop the march at opaque
geometry) + the three noise volumes + the weather map + the `CloudParams` UBO, and writing grayscale
coverage into `color`. It marches the view ray through the layer slab at a **coarse fixed step**,
accumulating opacity from the raw density — *no* lighting, *no* phase function, *no* light march, *no*
temporal reconstruction, *no* atmosphere composite:

```hlsl
// cloud_density_debug.slang — unlit density preview (NOT the Phase-5 raymarch)
float coverage = 0.0, T = 1.0;
for (int i = 0; i < DEBUG_STEPS; ++i) {            // ~48 fixed steps through the slab
    float3 wp = rayOrigin + rayDir * (tNear + (i + 0.5) * dt);
    float  sigma = sampleCloudDensity(wp, cloudParams);
    T *= exp(-sigma * dt);                          // Beer-Lambert opacity of the raw density
}
coverage = 1.0 - T;
outColor = float3(coverage);                        // grayscale over the scene
```

Import the persistent noise/weather images with `import_image_3d` (3D) and `import_image` (2D) declaring
`RgUsage::SampledReadCompute`, and `color` as `StorageImageRwCompute`; the graph derives every barrier.
Wire `submit_clouds` on the `SceneRenderer` trait (`engine/crates/assets/src/render_scene.rs`), folded from
`scene.environment.cloud` in `render_scene` beside `submit_sky`/`submit_fog`, forwarding a
`CloudRenderSettings` to the renderer (which updates `CloudParams` and marks the weather map dirty on param
change); the mock `SceneRenderer` in the test harness gains the same method. Phase 5 extends this same seam
with march/temporal settings — it does not add a second.

**File `engine/crates/protocol/src/dto.rs` + `engine/crates/control/src/commands_render.rs`.** Add
`CloudDensity` to the `ViewModeDto` enum and *both* mapping arms (`view_mode_to_dto` /
`view_mode_from_dto`), so `sa set-view-mode --mode cloud-density` round-trips through the existing
`set-view-mode` command (no new command).

> Decision to record when building: **the debug view introduces no transmittance ledger and cannot
> double-count the atmosphere.** Phase 4 composites nothing onto `height_fog.slang`, the aerial-perspective
> volume, or the `(inscatter, transmittance)` convention — the debug pass overwrites `color` with a
> grayscale coverage preview and is only active in `ViewMode::CloudDensity`. The Phase-6 ledger fold
> (`T_total = T_fog · T_aerial · T_cloud`, premultiplied, front-to-back, camera→cloud atmosphere applied
> exactly once) has no early footprint here; the seam is left clean.

## 5 — Protocol tripwires and the editor panel

The full checklist for the new `SetCloudsParams` DTO + `CloudSettingsDto` schema (the build refuses to
compile if any is missed):

| Tripwire | File | Edit |
|---|---|---|
| DTO struct | `engine/crates/protocol/src/dto.rs` | `SetCloudsParams` (derives + `#[ts(export)]`, per-field `Option`) |
| Command row | `engine/crates/protocol/src/command.rs` `COMMANDS` | `set-clouds` in the frozen scene-domain order (params `SetCloudsParams`, result `EnvironmentDto`) |
| Fixture | `engine/crates/protocol/src/command.rs` `COMMAND_FIXTURES` | `("set-clouds", "clouds-disabled")` |
| Type-name list | `engine/crates/protocol/src/command.rs` `DTO_TYPE_NAMES` | add `"SetCloudsParams"` |
| Domain-order test | `engine/crates/protocol/src/command.rs` `scene_domain()` | add `"set-clouds"` in position |
| TS decls + fragments | `engine/crates/protocol/src/codegen.rs` | `decl_entry!(SetCloudsParams)` + `frag_entry!(SetCloudsParams)` |
| Hand-authored schema | `engine/crates/protocol/src/schema.rs` | `CloudSettingsDto` block + its `required[]`; `Environment.cloud` `$ref` + `Environment.required` add `"cloud"` |
| Inventory | `engine/crates/protocol/tests/inventory.rs` | add `SetCloudsParams` |
| Schema fragment | `engine/crates/protocol/tests/schema_fragments.rs` | `check!(SetCloudsParams, "SetCloudsParams")` |
| e2e fixture params | `tools/check-control-schema/check.ts` | `paramsForFixture()` case `clouds-disabled` → `{ enabled: false }` |
| Generated artifacts | `schemas/control/openrpc.generated.json`, `command-manifest.generated.json` | regenerate via `cargo run -p xtask -- gen-protocol` |
| `ViewModeDto` variant | `engine/crates/protocol/src/dto.rs`, `engine/crates/protocol/tests/wire.rs` | add `CloudDensity`; extend the `ViewModeDto` wire round-trip if it enumerates variants |

**File `editor/src/control/client.ts`.** Add `setClouds(cloud: Partial<Environment["cloud"]>)` beside
`setFog`/`setAtmosphere` (a thin wrapper over the generic control passthrough). **File
`editor/src/panels/EnvironmentPanel.tsx`.** Add a **Clouds** `<Row>` section mirroring the Fog section:
`patchClouds`/`recordCloudEdit`/`cloudCoalescerFor` cloned from `patchFog`/`recordFogEdit`/`fogCoalescerFor`,
an enabled `Switch`, `NumberDrag`/`SliderField` rows for coverage/cloud-type/precipitation/anvil, altitude
+ height, base/detail/curl/weather scales, and an `AssetPicker` for the painted `weatherTexture`.
`@saffron/protocol` regenerates the `SetCloudsParams`/`Environment` TS types from `bun run check` — never
hand-edit `sa-types.ts`.

## Out of scope (later phases and unscheduled)

- **Lighting the density field (Phase 5).** No energy-conserving multi-octave scatter, no dual-lobe /
  HG-Draine phase, no cone-sampled sun transmittance, no ambient from SH sky, no reduced-res buffer, no
  temporal reconstruction, no premultiplied HDR composite. The debug view is unlit opacity only, and
  `sampleCloudDensity` returns pure density — Phase 5 lights *around* it via the shared function.
- **Atmosphere-ledger integration, cloud shadows, god-rays, wind (Phase 6).** No sampling of the
  Transmittance/sky-view LUTs at the cloud hit distance, no `T_cloud` fold onto `height_fog.slang`, no
  top-down cloud shadow map, no `fog_inject` god-rays, no curl-advected wind field. `weather_offset` is the
  clean seam Phase-6 wind advances; the density-sampler signature is stable so Phase 6 adds animation by
  scrolling inputs, not by reshaping.
- **Nubis-3 sparse voxel authoring (unscheduled, the modern end-state).** Bespoke, fly-through,
  terrain-shadowed clouds want a low-res dimensional-profile NVDF (BC6) + BC1 SDF sphere-trace up-rez with
  the noise composite, authored in Houdini "Atlas" — the destination for hero clouds the procedural
  weather-map path cannot express (Schneider, *Nubis Cubed*, SIGGRAPH 2023). This phase ships the
  procedural weather-map path and leaves `sampleCloudDensity` as the clean seam an NVDF up-rez slots behind
  (swap the weather-map/height-envelope source for a voxel fetch; the erosion + value-erosion tail is
  unchanged). The voxel authoring + import pipeline is deliberately not built.

## Known interactions (note, don't over-engineer)

- **Cloud noise vs. fog erosion noise.** `Clouds` bakes a new `128³` RGBA + `32³` + `128²` set; the fog's
  `bake_noise_volume` `64³` `R8` volume is a separate resource for a separate consumer. Do not merge or
  reuse — they tile different bases at different frequencies.
- **Weather-map refill cost.** The procedural weather fill is a cheap 2D dispatch; refill only on a
  weather-param dirty flag (coverage/type/scale/offset/precip changed), not per frame. The painted-override
  blit happens once at load.
- **Debug step count.** The unlit preview march uses a small fixed step count for interactivity; it is a
  visualization, not a quality target. Do not thread a quality tier through it — Phase 5's production march
  owns step-count/quality.
- **Layer altitude vs. scene scale.** The default `1500 m` layer sits far above a typical test scene, so
  the debug view needs a camera that can see the sky dome (or a lowered `layer_altitude`) to show anything —
  note this in the docs page so the first `set-clouds` probe is visible.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this
change raises. Boot headless on the NVIDIA GPU (`just run-engine-headless`) with a validation-clean log and
confirm the density field renders: `sa set-clouds --enabled true --coverage 0.7 --cloudType 0.4`
followed by `sa set-view-mode --mode cloud-density` must change the frame (a grayscale cloudscape over
the sky), and varying `--coverage` / `--cloudType` / `--anvilBias` must visibly reshape it (more coverage
→ denser cores, higher type → taller anvils). Because a wire type changed (`SetCloudsParams` +
`CloudSettings`, and the `ViewModeDto::CloudDensity` variant), run `bun run check` in `editor/`
(regenerates `@saffron/protocol` — the `Environment` type gains `cloud`, `set-clouds` appears in the
manifest; never hand-edit `sa-types.ts`) and `just e2e`, adding a `tests/e2e` case that drives `set-clouds`
with `{ enabled: true, coverage: 0.7 }` over the control plane and asserts the `EnvironmentDto` echo carries
the cloud block plus a validation-clean log. Add a `docs/content/explanations/image-based-lighting/`
concept page — **Volumetric cloud shape** (the noise fields, the weather map, the dimensional profile,
value-erosion vs. multiply, the debug view, and Nubis-3 as the noted future direction) — sibling to the
Phase-1/2/3 sky pages, and update its hub `image-based-lighting/_index.md` `## Pages` row, using the slim
`What | File | Symbols` table:
`cloud_noise_base.slang`/`cloud_noise_detail.slang`/`cloud_curl.slang`/`cloud_weather.slang`/`clouds.slang`,
`engine/crates/rendering/src/clouds.rs` (`Clouds`, `sampleCloudDensity`),
`engine/crates/rendering/src/renderer.rs` (`ViewMode::CloudDensity`, `add_cloud_debug_pass`),
`engine/crates/scene/src/environment.rs` (`CloudSettings`), and `SetCloudsParams` — in the same change.

## References

Cloud-shape noise + authoring:

- Schneider & Vos — *The Real-Time Volumetric Cloudscapes of Horizon: Zero Dawn* (SIGGRAPH 2015; the
  `128³` RGBA Perlin-Worley base + `32³` Worley erosion + `128²` curl channel pack, `remap` shaping):
  https://advances.realtimerendering.com/s2015/The%20Real-time%20Volumetric%20Cloudscapes%20of%20Horizon%20-%20Zero%20Dawn%20-%20ARTR.pdf
- Schneider & Vos — *Nubis: Authoring Real-Time Volumetric Cloudscapes with the Decima Engine*
  (SIGGRAPH 2017; the weather-map RGB = coverage/precip/type + the height-gradient presets and anvil bias):
  https://advances.realtimerendering.com/s2017/Nubis%20-%20Authoring%20Realtime%20Volumetric%20Cloudscapes%20with%20the%20Decima%20Engine%20-%20Final%20.pdf
- Schneider — *Nubis, Evolved* (SIGGRAPH 2022; the dimensional profile + `cloud_density = saturate(noise −
  (1 − dimensional_profile))` value-erosion): https://www.guerrilla-games.com/read/nubis-evolved
- Schneider — *Nubis, Cubed* (SIGGRAPH 2023; sparse voxel authoring, the noted future end-state):
  https://advances.realtimerendering.com/s2023/Nubis%20Cubed%20(Advances%202023).pdf
- Sébastien Hillaire (Frostbite) — *TileableVolumeNoise* (the open Perlin-Worley/Worley 3D generator):
  https://github.com/sebh/TileableVolumeNoise

Engine references (shape / weather-map / height gradients):

- Unreal Engine — *Volumetric Cloud Material* (Conservative Density gate, layer bounds, height gradient from
  cloud sample altitude): https://dev.epicgames.com/documentation/en-us/unreal-engine/volumetric-cloud-material-in-unreal-engine
- Unity HDRP — *Volumetric Clouds Volume Override* (cloud-map channels R=coverage/G=rain/B=type + LUT
  height profile, shaping vs. erosion): https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@17.3/manual/volumetric-clouds-volume-override-reference.html
