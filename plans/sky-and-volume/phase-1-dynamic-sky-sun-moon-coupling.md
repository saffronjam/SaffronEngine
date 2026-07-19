# Phase 1 — Dynamic atmosphere re-bake and physically-coupled sun and moon key lights

**Status:** COMPLETED

Part of `plans/sky-and-volume/` (dynamic sky, time-of-day, and volumetric clouds). This is the
**enabler phase**: it turns the frozen Hillaire chain into an amortized dynamic one and couples the
scene's key lights to it, so the sky animates and sunsets are physically correct with *no clouds
required*. It is a standalone win and has no dependency (`dependsOn: —`).

Today the Hillaire-2020 atmosphere is physically based but static. Every sun move arms a full
synchronous re-bake of the whole chain — LUTs → env cube → mips → irradiance → prefilter → BRDF —
behind `device.wait_idle` (`Ibl::request_env_bake` → `should_rebake` → `fire_rebake` → `bake(false)`
in `engine/crates/rendering/src/ibl.rs`, which calls `device.wait_idle()` at line 746). A
time-of-day cycle would stall the GPU on every sun step, so the sky, key light, and sky-light IBL
cannot animate at all. Worse, the "Sun" `DirectionalLight` carries an authored color decoupled from
the atmosphere it is drawn against — `drive_env_bake` sets `sun_dir = -light_dir` but never derives
the light *color* from the transmittance the same LUTs bake (`engine/crates/assets/src/render_scene.rs`)
— so at sunset the sky disc and the lit scene redden and dim by different amounts (the tell-tale
decoupled-hack look), and there is no moon key light at all.

This phase does three things, all reusing the existing chain and **never rebuilding it**:

1. **Split the bake by cost.** The sun-*independent* Transmittance (256×64) and Multiple-Scattering
   (32×32) LUTs bake once behind an atmosphere-dirty gate; only the sun-*dependent* Sky-View LUT and
   `atmos_skygen` env-cube fill (+ its convolutions) re-derive per sun-move, off the `device.wait_idle`
   path, double-buffered and temporally blended so a moving sun never pops.
2. **Read the key lights out of the atmosphere.** The "Sun" `DirectionalLight` color and intensity, and
   its physically based sun disc, are one radiance — `E_TOA × T(height, sun_zenith)` from the
   transmittance — so light, sky, and disc redden in lockstep with no authored sunset curve.
3. **Add the Moon** as a second atmosphere-coupled `DirectionalLight` (role index 1): transmittance-tinted,
   a Hapke/Lommel-Seeliger disc with opposition surge (deliberately *not* limb-darkened like the sun),
   phase from the sun direction, ~0.26 lux at full, single-scatter only — plus a per-pixel-transmittance
   flag on `AtmosphereSettings` for planetary shadowing.

> **Explicitly NOT rebuilt here.** The four Hillaire LUTs, the `atmos_*.slang` chain, the visible-sky
> pass, and the shipped 32³ aerial-perspective froxel (`AerialPerspective`, `aerial_perspective.slang`)
> already exist and stay as they are — this phase reuses them. The sun-independent LUTs (transmittance,
> multiscatter) are the *frozen* base a dynamic sun rides; the aerial-perspective pass already reads
> `baked_atmosphere()`/`baked_sun()` and keeps working unchanged (it re-derives per frame from the
> frozen LUTs). No second sky model, no second env source, no parallel bake path.

## Goal

- **Dynamic sun with no stall.** Moving the "Sun" `DirectionalLight` animates the sky and IBL without
  `device.wait_idle`: transmittance + multiscatter stay frozen, only sky-view + env-cube + convolutions
  refresh past a sun-angle threshold, double-buffered and EMA-blended into the live IBL set.
- **One radiance for sun light, sky, and disc.** `set_scene_lighting` derives the Sun color/intensity
  from `E_TOA × T` toward the sun, replacing the authored color for atmosphere scenes; the `atmos_skygen`
  disc uses the same `E_TOA·T/Ω` with Hestroffer–Magnan limb darkening. Sunsets redden automatically.
- **A physical Moon key light.** A second atmosphere-coupled `DirectionalLight` (role Moon) lights the
  scene at ~0.26 lux full, transmittance-tinted; its `atmos_skygen` disc uses a Hapke/Lommel-Seeliger
  BRDF with opposition surge + earthshine + in-shader phase from the sun direction, single-scatter.
- **Per-pixel atmosphere transmittance** as an `AtmosphereSettings` flag so the planet can shadow the
  moon disc (per-direction transmittance instead of one global value).
- **One write path.** `set-atmosphere` carries the new sun/moon/per-pixel knobs; the moon *direction*
  is an `atmosphereRole` on `DirectionalLight` edited through the existing component path. No second sky
  command, no knob smuggled onto `set-environment`, no duplicate pass.

## Design stance (grounded in current engine practice + the cited technique)

**Cost-asymmetric LUT refresh — freeze the sun-independent LUTs, refresh only the sun-dependent tail
off `wait_idle`.** Hillaire's four-LUT decomposition is what makes a dynamic sun affordable: the
Transmittance LUT (`atmos_transmittance.slang`, 256×64) and the Multiple-Scattering LUT
(`atmos_multiscatter.slang`, 32×32, which samples only the transmittance LUT and a sun-cos-zenith axis)
depend *only* on atmosphere composition, not the sun's current direction. The Sky-View LUT
(`atmos_skyview.slang`), the `atmos_skygen` env-cube fill (which places the sun disc from `pc.sunDir`),
and the convolution chain (`ibl_irradiance`/`ibl_prefilter`) are the sun-dependent tail. So a sun move
re-runs only the tail, and the frozen LUTs are reused in place (Hillaire 2020,
https://sebh.github.io/publications/egsr2020.pdf; per-LUT costs, CESCG 2023). The synchronous
`device.wait_idle` full re-bake is **replaced**, not kept beside the new path — the refresh runs
double-buffered and fence-guarded so the in-flight frame keeps sampling the previous IBL set, and the
new set is EMA-blended in so nothing pops (UE SkyLight Real-Time Capture TimeSlice,
https://dev.epicgames.com/documentation/unreal-engine/sky-lights-in-unreal-engine?lang=en-US).

**One transmittance source of truth for the sun light, sky, and disc.** The scene key light is a
read-out of the same atmosphere the sky uses: `L_sun = E_TOA × T(height, sun_zenith)`, with
`E_TOA ≈ 120000` lux equivalent at zenith, `T` the Rayleigh+Mie+ozone transmittance the LUT bakes.
As the sun lowers, the long slant path removes blue first, so `T` reddens and dims and the directional
light, the sky, and the sun disc all track it — no authored sunset color curve (UE Atmosphere Sun Light
/ `SkyAtmosphereLightIlluminance`,
https://dev.epicgames.com/documentation/unreal-engine/sky-atmosphere-component-in-unreal-engine). The
disc shares the same radiance: `L_disc = E_TOA·T / Ω_disc`, `Ω_disc = 2π(1 − cos(θ/2))`, with
Hestroffer–Magnan limb darkening `I(μ)/I(1) = 1 − u(1 − μ)`, `u ≈ 0.6`, `μ = cos` of the angle from
disc centre (https://en.wikipedia.org/wiki/Limb_darkening). The disc is ~0.53° across
(`sun_disk_angular_radius = 0.00465` rad today; UE uses 0.545°).

**The Moon is a second atmosphere-coupled directional light, not a billboard.** Following UE's
"Atmosphere Sun Light Index 1" (same doc as the sun), the moon gets a transmittance-tinted key light
and a sun-disc representation, but is lit by reflected sunlight: a low-albedo, back-scattering surface
whose disc uses a Hapke / Lommel-Seeliger BRDF with an *opposition surge* (the full moon is nearly
uniformly bright to the limb and surges at full phase — the opposite of the sun's limb darkening;
Kuzminykh 2021, https://elib.dlr.de/203152/1/Bachelorarbeit_Alexander_Kuzminykh_20210818.pdf). Phase is
computed in-shader from the sun direction — illuminated fraction `(1 − cos(elong))/2`,
`elong = angle(sunDir, moonDir)` — so the terminator tracks the sun with no baked phase texture; the
dark limb gets a faint bluish earthshine (~0.15–0.19 W/m² at the moon; Jensen et al. Night Rendering,
http://graphics.ucsd.edu/~henrik/papers/night/night.pdf). The full moon delivers ~0.26 lux to the
ground (UE default), so exposure/tonemap can read night as night. Only the *first* atmosphere light
(the sun) drives the multiscatter LUT; the moon is **single-scatter only** — do not re-bake a second
sky-view for it in this phase.

**Per-pixel atmosphere transmittance for correct planetary shadowing.** A single global (top-of-planet)
transmittance is wrong once one celestial body should shadow another. A `per_pixel_transmittance` flag on
`AtmosphereSettings` makes `atmos_skygen` evaluate transmittance per disc direction from the
Transmittance LUT (bound as a second input) instead of a single value, so the planet can occlude the
moon disc (UE Per Pixel Atmosphere Transmittance, same Sky Atmosphere doc).

**Modern-correct, NO-LEGACY.** The synchronous full re-bake is retired in the change that adds the
amortized path — no static path kept alive next to the dynamic one. The authored sun color is replaced
by the transmittance read for atmosphere scenes; procedural/equirect scenes keep the authored color
(they have no transmittance to read). There is exactly one `E_TOA × T` expression, shared by the light
and the disc.

## NO-LEGACY checklist for this phase

- The `device.wait_idle` full re-bake in `Ibl::bake(_, false)` is **removed** and replaced by the
  split refresh (frozen static LUTs + double-buffered async sun-move refresh). There is no "static
  re-bake" fallback kept beside it.
- `should_rebake` is **widened in place** to a sun-angle epsilon threshold + an atmosphere-dirty flag —
  the pure unit-tested gate is extended, not duplicated by a second dirty boolean.
- For an atmosphere scene the Sun `DirectionalLight` color/intensity is derived from the transmittance
  in `set_scene_lighting`; the authored `color`/`intensity` no longer feed the sun UBO in that mode.
  There is no second "sunset color" curve anywhere.
- The sun disc and the sun key light read **one** `E_TOA × T` radiance; the moon disc and moon key light
  read one moon radiance. The disc is not drawn from an independently authored intensity.
- `AtmosPush` and the copy-pasted `AtmosParams` struct in **all four** `atmos_*.slang` files gain the moon
  + per-pixel fields together in one change (the struct is shared verbatim; the transmittance/multiscatter/
  skyview passes ignore the new lanes, only `atmos_skygen` reads them) — the build breaks otherwise.
- The new `AtmosphereSettings` fields (moon intensity, moon angular radius, per-pixel transmittance) and
  the `DirectionalLight.atmosphereRole` are registered across their tripwire lists (§4) in one change;
  the schema-fragment byte-equivalence test and `bun run check` refuse to compile a partial change.

## 0 — Split the bake: cost-asymmetric refresh, double-buffer, temporal blend, threshold gate

**File `engine/crates/rendering/src/ibl.rs`.** Restructure `Ibl::bake` / `fire_rebake` into two paths
gated by *what changed*, and remove the `device.wait_idle` stall.

- **Static LUTs behind an atmosphere-dirty gate.** `record_atmosphere` today records the whole chain
  (transmittance → multiscatter → sky-view). Split it: transmittance + multiscatter bake **only** when
  the atmosphere params changed (`AtmosphereParams != baked`), tracked by a new `atmosphere_dirty` flag
  set in `request_env_bake`. They are the frozen base; a sun move never touches them. The **BRDF LUT**
  (`ibl_brdf.slang`) is fully static — bake it once at startup and never again (drop it from the
  per-refresh chain entirely).
- **Sun-move refresh, off `wait_idle`.** Sky-view LUT → `atmos_skygen` env-cube fill → env mips →
  irradiance → prefilter re-derive per sun-move. Record them on a one-shot command buffer submitted to
  the graphics queue **without** `device.wait_idle`, guarded by a per-refresh fence checked at the next
  `begin_frame_graph`. Double-buffer the three IBL cubes so the in-flight frame samples the committed
  ("front") set while the refresh writes the "back" set; `write_mesh_set` re-points set 3 (bindings 0-2)
  at the back set only after the fence signals. This is the double-buffer + fence-guard that lets the
  refresh drop `wait_idle`.
- **Temporal blend so a sun move never pops.** Add a small `ibl_cube_blend.slang` compute pass that
  EMA-blends the freshly baked env cube into the committed one (`front = lerp(front, back, α)`,
  `α ≈ 0.2`) over a handful of frames after each refresh, then reconvolves the blended cube — so the
  visible sky + IBL cross-fade to the new sun instead of hard-swapping. (Phase 2 relocates this blend
  onto the SH diffuse + prefiltered mips to make it per-frame-cheap; this phase blends the env cube,
  which is literally "the new env cube into the previous one.")
- **Widen `should_rebake` to a threshold.** Replace the exact `params.sun_dir != baked.sun_dir` with a
  sun-angle epsilon: arm the sun-move refresh when `sun_dir.dot(baked.sun_dir) < cos(ε)`, `ε ≈ 0.25°`
  (matching UE's time-slice cadence), and arm the static-LUT rebake on the atmosphere `!=`. Keep the
  panorama/source-change arms. Continuous sun motion then triggers a bounded number of refreshes, not
  one per frame. Extend the existing `should_rebake` unit tests (ibl.rs ~2717) for the threshold.

| What | File | Symbols |
|---|---|---|
| Split bake + refresh gate | `engine/crates/rendering/src/ibl.rs` | `Ibl::bake`, `Ibl::fire_rebake`, `Ibl::record_atmosphere`, `Ibl::request_env_bake`, `should_rebake`, `rebake_pending`, `write_mesh_set` |
| Frozen LUT images (reused in place) | `engine/crates/rendering/src/ibl.rs` | `transmittance_lut`, `multi_scatter_lut`, `brdf_lut`, `sky_view_lut`, `ATMOS_TRANSMITTANCE_W/H`, `ATMOS_MULTI_SCATTER_SIZE` |
| Double-buffered IBL cubes | `engine/crates/rendering/src/ibl.rs` | `IblCube`, `env_cube`, `irradiance_cube`, `prefiltered_cube`, `generate_cube_mips`, `BakeScratch` |
| New env-cube blend pass | `engine/assets/shaders/ibl_cube_blend.slang` (new) | `computeMain` (EMA `lerp(front, back, α)`) |
| Refresh trigger | `engine/crates/rendering/src/renderer.rs` | the `rebake_pending` fire block (~4916), `Renderer::request_env_bake` |

> Decision to record when building: **the sun-move refresh is fence-guarded + double-buffered, not a
> `wait_idle`.** The in-flight frame samples the committed cube set; the refresh writes the shadow set
> and is committed only after its fence signals. This is the single mechanism that removes the stall —
> not a shorter `wait_idle`, not a per-frame full re-bake.

## 1 — The Sun key light from the Transmittance LUT

**File `engine/crates/rendering/src/renderer.rs` (`Renderer::set_scene_lighting`, ~2003) + a CPU
transmittance evaluator in `engine/crates/rendering/src/ibl.rs`.**

Add a pure CPU function `sun_transmittance(atmos: &AtmosphereParams, dir_to_sun: Vec3) -> Vec3` that
mirrors `atmos_transmittance.slang` (`densities` + `rayTopDistance` + the 40-step midpoint integral)
for a ground observer (`h = 0.5` km, matching the sky-view bake altitude), returning the per-channel
transmittance toward the sun. It is a ~40-step integral, cheap enough to evaluate every frame and
deterministic (no GPU readback), so the sun light stays perfectly smooth even while the env cube
amortizes.

In `Renderer::set_scene_lighting`, when `self.scene_ibl().atmosphere_live()`, override the sun
before handing the `SceneLighting` to `Lighting::set_scene_lighting`:

```
let T = sun_transmittance(&self.scene_ibl().baked_atmosphere(), (-self.sun_direction));
let L = E_TOA * T;                       // E_TOA ≈ 120000 lux-equivalent at zenith (a named const)
scene.color = normalize_chroma(L);       // chromaticity from T (reddens at sunset)
scene.intensity = luminance(L) * scene.intensity;  // authored intensity becomes a trim
```

so `color × intensity == E_TOA × T`, the same radiance the disc uses (§2). For procedural/equirect
scenes (`!atmosphere_live()`) the authored color/intensity flow through unchanged — those sources have
no transmittance to read, so there is nothing to override and no second path. `E_TOA` is a named
constant (documented ≈120000 lux at zenith, solar constant 1361 W/m² ≈ 128000 lux TOA) scaled into the
engine's linear light units.

| What | File | Symbols |
|---|---|---|
| CPU transmittance evaluator | `engine/crates/rendering/src/ibl.rs` | `sun_transmittance` (new, mirrors `atmos_transmittance.slang::densities`/`rayTopDistance`), `E_TOA` const, `baked_atmosphere`, `baked_sun` |
| Sun-light override | `engine/crates/rendering/src/renderer.rs` | `Renderer::set_scene_lighting` (~2003), `sun_direction`, `scene_ibl().atmosphere_live()` |
| Downstream UBO write (unchanged shape) | `engine/crates/rendering/src/lighting.rs` | `Lighting::set_scene_lighting` (~517), `LightUbo::color_intensity`/`direction_ambient`, `SceneLighting` |

> Decision to record when building: **derive the sun light on the CPU from the analytic transmittance
> integral, not a GPU LUT readback.** The integral is the exact math `atmos_transmittance.slang` bakes;
> evaluating it CPU-side every frame keeps the key light smooth independent of the env-cube refresh
> cadence and avoids a stall-inducing image readback. The result equals a Transmittance-LUT sample.

## 2 — Physically based sun disc + limb darkening in `atmos_skygen`

**File `engine/assets/shaders/atmos_skygen.slang` (`computeMain`, the disc term at lines 66-72).**

Today the disc is `disk * pc.params1.y (sunDiskIntensity) * pc.sunDir.w (sunIntensity)` added onto the
sampled sky radiance — a flat white cap. Replace the radiance with the physical form so the disc and the
key light share one source:

- Disc radiance `L_disc = E_TOA · T(sun) / Ω_disc`, `Ω_disc = 2π(1 − cos(θ/2))`, `θ = 2·sun_disk_angular_radius`.
  `T(sun)` comes from the Transmittance LUT (bound into `atmos_skygen`, §3's per-pixel path, or the
  sky-view value already sampled at the sun direction).
- Multiply by Hestroffer–Magnan limb darkening `I(μ)/I(1) = 1 − u(1 − μ)`, `u ≈ 0.6`, where
  `μ = sqrt(1 − (d/θ_r)²)` and `d` is the angle from disc centre, `θ_r = sun_disk_angular_radius`. The
  disc is ~40% dimmer at its edge.
- Keep `sun_disk_intensity` as an artist trim multiplier over the physical baseline (not a second radiance
  source) — a scene can push the disc up without decoupling it from the light.

Because the sky radiance the disc sits on is the baked (blended) env cube, and the key light is
`E_TOA × T` (§1), the disc, sky, and lit scene now redden together with no authored ramp.

| What | File | Symbols |
|---|---|---|
| Physical disc + limb law | `engine/assets/shaders/atmos_skygen.slang` | `computeMain` (disc term), `dirToSkyViewUv`, `pc.params1.x` (angular radius), `pc.params1.y` (intensity trim), `pc.sunDir` |
| Shared push | `engine/crates/rendering/src/ibl.rs` | `AtmosPush`, `AtmosPush::new` |

## 3 — The Moon: atmosphere-light role, moon key light, and the Hapke disc

**Component role — `engine/crates/scene/src/component.rs` (`DirectionalLight`).** Add an
`atmosphere_role: AtmosphereRole` field (`enum AtmosphereRole { Sun, Moon }`, default `Sun`). The Sun-role
light is the key light + drives the sun disc; a Moon-role light supplies the moon *direction* and the moon
key light. This is the explicit UE-style "Atmosphere Sun Light Index 0/1" — no reliance on
DirectionalLight creation order. Round-trip it in the `DirectionalLight` `SceneSerialize` impl
(`serde.rs`, `to_json`/`load_json` at ~379) as an `"atmosphereRole": "sun"|"moon"` string, add the
enum to the hand-authored `DirectionalLight` component schema (`schema.rs` ~360, properties + `required`),
and add a `FIELD_HINTS` enum entry `"DirectionalLight.atmosphereRole"` in `editor/src/components/fieldRenderer.tsx`.

**Gather both — `engine/crates/assets/src/render_scene.rs`.** `gather_directional_light` (~839) returns
the first `DirectionalLight`; extend it to resolve the first Sun-role and the first Moon-role lights
separately (a `DirectionalResolved` for each). The sun feeds `set_scene_lighting` + `drive_env_bake` as
today; the moon feeds a new moon slot on `SceneLighting`.

**Moon key light — `engine/crates/rendering/src/lighting.rs` + `renderer.rs`.** Extend `SceneLighting`
with a moon `direction`/`color`/`intensity` triple, and `LightUbo` with a `moon_direction_intensity` +
`moon_color` pair (updating the std140 byte-layout offsets test at lighting.rs ~1071). In
`Renderer::set_scene_lighting`, derive the moon color from `moon_albedo · E_TOA_moon · phase · T(moon_zenith)`
scaled so full moon ≈ 0.26 lux, transmittance-tinted via the §1 CPU evaluator toward the moon. The mesh
directional term (`engine/assets/shaders/lighting.slang`) evaluates the moon as a second, single-scatter,
transmittance-tinted directional light (no cascade shadow this phase — the moon is a low-intensity fill;
moon shadows are a later refinement).

**Moon disc — `atmos_skygen.slang` + `AtmosPush`.** Extend `AtmosPush` (and the copy-pasted `AtmosParams`
struct in **all four** `atmos_*.slang` files) with two new `float4`s: `moon` (`xyz` dir to moon, `w`
intensity) and `moon_disk` (`x` angular radius, `y` disc intensity, `z` earthshine, `w`
per-pixel-transmittance flag). Only `atmos_skygen` reads them; transmittance/multiscatter/skyview ignore
the new lanes. In `atmos_skygen::computeMain`, add a second disc at the moon direction using a
Hapke/Lommel-Seeliger BRDF with opposition surge — **not** limb-darkened — with:

- phase from the sun: `illum = (1 − cos(elong))/2`, `elong = acos(dot(sunDir, moonDir))`, the terminator
  tracking the sun;
- earthshine lifting the dark limb (a faint bluish term scaled by `moon_disk.z`, strongest near new moon);
- transmittance applied per the per-pixel flag (§3 per-pixel path).

**Per-pixel transmittance — `AtmosphereSettings` + `atmos_skygen`.** Add `per_pixel_transmittance: bool`
to `AtmosphereSettings`; when set, `atmos_skygen` samples the Transmittance LUT (bound as a **second
input**, binding 2 alongside the existing sky-view LUT at binding 0) at each disc direction so the planet
can shadow the moon, instead of one global value.

| What | File | Symbols |
|---|---|---|
| Atmosphere-light role | `engine/crates/scene/src/component.rs` | `DirectionalLight`, new `AtmosphereRole { Sun, Moon }` + `atmosphere_role`, `DEFAULT_DIRECTION` |
| Role serde + schema + hint | `engine/crates/scene/src/serde.rs`, `engine/crates/protocol/src/schema.rs`, `editor/src/components/fieldRenderer.tsx` | `DirectionalLight` `to_json`/`load_json` (~379), `"DirectionalLight"` schema (~360), `FIELD_HINTS` |
| Gather sun + moon | `engine/crates/assets/src/render_scene.rs` | `gather_directional_light` (~839), `DirectionalResolved`, `drive_env_bake` (~1215) |
| Moon key light | `engine/crates/rendering/src/lighting.rs`, `renderer.rs`, `engine/assets/shaders/lighting.slang` | `SceneLighting`, `LightUbo`, `Renderer::set_scene_lighting`, `sun_transmittance` |
| Moon + per-pixel disc | `engine/assets/shaders/atmos_skygen.slang`, `atmos_transmittance/multiscatter/skyview.slang`, `engine/crates/rendering/src/ibl.rs` | `AtmosPush`, `AtmosParams` (5→7 `float4`), `computeMain`, `sky_view_lut`/`transmittance_lut` bindings |

> Decision to record when building: **the moon is single-scatter and re-uses the sun's sky-view bake —
> it does not bake a second sky-view LUT.** Following UE (multiscatter is computed only for the first
> atmosphere light), the moon adds a disc in `atmos_skygen` + a directional key light; full moonlit-sky
> multiple scattering is out of scope. The moon shares the frozen transmittance/multiscatter LUTs.

## 4 — `AtmosphereSettings` fields + `SetAtmosphereParams` across the tripwires

The moon disc appearance + per-pixel flag are scene state on `AtmosphereSettings` (the moon *direction*
is the role'd `DirectionalLight`, §3). These are **added fields on an existing settings block + DTO**, so
the narrower tripwire set applies (no new DTO type):

- **`engine/crates/scene/src/environment.rs`** — add to `AtmosphereSettings` + its `Default`:
  `moon_disk_angular_radius` (default ~0.00496 rad ≈ 0.568° diameter), `moon_disk_intensity`,
  `moon_earthshine`, `per_pixel_transmittance: bool` (default false). (The Moon key-light *intensity* is
  the Moon-role `DirectionalLight.intensity`; `moon_disk_intensity` trims the disc.)
- **`engine/crates/scene/src/serde.rs`** — add each field to `atmosphere_to_json` (~909) **and**
  `atmosphere_from_json` (~927) with its default, so it round-trips through `environment_to_json`/`from_json`.
- **`engine/crates/protocol/src/dto.rs`** — add matching `Option<>` fields to `SetAtmosphereParams` (~2526):
  `moon_disk_angular_radius`, `moon_disk_intensity`, `moon_earthshine`, and
  `per_pixel_transmittance: Option<bool>` (with `coerce::opt_boolean`). No new DTO type, so
  `DTO_TYPE_NAMES`/`codegen.rs`/`inventory.rs`/`schema_fragments.rs` are unchanged.
- **`engine/crates/protocol/src/schema.rs`** — extend the hand-authored `AtmosphereSettingsDto` block
  (~604) properties + `required` array with the four new fields.
- **`engine/crates/control/src/commands_scene.rs`** — extend the `set-atmosphere` handler (~834) merge
  with one `if let Some(v) = params.<field>` branch per new field, and update the help string.
- **`engine/crates/scene/src/component.rs` / schema.rs** (from §3) — the `atmosphereRole` component field
  is a component-schema enum string, not a protocol DTO type.
- **Editor** — add rows in `editor/src/panels/EnvironmentPanel.tsx` under the Atmosphere block via
  `patchAtmos` (a Moon subsection: disc radius, disc intensity, earthshine; a Per-Pixel Transmittance
  switch), reusing the existing `atmosCoalescerFor`/`recordAtmosEdit` gesture plumbing. The typed
  `client.setAtmosphere` wrapper is generic over `Partial<Environment["atmosphere"]>`, so it needs no
  edit; `bun run check` regenerates `@saffron/protocol` so the new fields appear in the TS type.

The **`sa` CLI needs no per-command code** — `sa set-atmosphere --moonDiskIntensity … --perPixelTransmittance`
flows through the generated manifest. Regenerate the committed wire artifacts with
`cargo run -p xtask -- gen-protocol` (`schemas/control/{openrpc,command-manifest}.generated.json`).

| What | File | Symbols |
|---|---|---|
| Scene state + defaults | `engine/crates/scene/src/environment.rs` | `AtmosphereSettings`, `AtmosphereSettings::default` |
| Serde round-trip | `engine/crates/scene/src/serde.rs` | `atmosphere_to_json` (~909), `atmosphere_from_json` (~927) |
| Wire DTO | `engine/crates/protocol/src/dto.rs` | `SetAtmosphereParams` (~2526) |
| Hand-authored schema | `engine/crates/protocol/src/schema.rs` | `AtmosphereSettingsDto` (~604) |
| Control handler | `engine/crates/control/src/commands_scene.rs` | `set-atmosphere` handler (~834), `environment_dto` |
| Editor rows | `editor/src/panels/EnvironmentPanel.tsx`, `editor/src/control/client.ts` | `patchAtmos`, `atmosCoalescerFor`, `recordAtmosEdit`, `setAtmosphere` |

## 5 — Docs + `sa`

Update `docs/content/explanations/image-based-lighting/procedural-atmosphere.md` (do not add a new page):
add a **Dynamic re-bake** section (the frozen transmittance/multiscatter LUTs vs. the sun-move sky-view
+ env-cube refresh, the double-buffer + EMA blend, the `should_rebake` threshold — replacing the "editor-time,
not per-frame" framing) and a **Sun and moon coupling** section (the `E_TOA × T` key-light + disc radiance,
limb darkening, the moon as a second role'd atmosphere light with a Hapke disc + earthshine + per-pixel
transmittance). Extend the "In the code" table with `sun_transmittance`, `AtmosphereRole`, the moon fields,
and `Renderer::set_scene_lighting`. Update the `_index.md` hub row for `procedural-atmosphere` to mention
the dynamic re-bake + sun/moon coupling. Run the docs skill's build + link-check + style-check loop.

## Out of scope (later phases)

- **Real-time sky-light IBL (SH diffuse + amortized specular) + GI retint — Phase 2.** This phase
  double-buffers + EMA-blends the *env cube* and keeps the existing irradiance-cube + prefilter
  convolution; Phase 2 replaces the irradiance cube with SH diffuse (per-frame) and time-slices the
  specular prefilter, and retints DDGI/voxel-GI ambient (`ddgi_sky`) from the live sky. The blend seam
  here is left clean for that relocation.
- **Time-of-day driver + night sky — Phase 3.** No date/latitude/ephemeris here: the sun and moon
  directions are authored `DirectionalLight`s (role'd), moved by hand or `set-light`. The
  NREL-SPA ephemeris that *drives* those directions, artist TOD curves, stars/Milky Way, and the
  scotopic tonemap are Phase 3 — which feeds the Phase-1 re-bake gate and moon disc.
- **Clouds — Phases 4-6.** Nothing cloud here.
- **Moon shadows + full moonlit-sky multiple scattering.** The moon is single-scatter and unshadowed
  this phase (a low-intensity fill); moon cascade shadows and a moon multiscatter pass are deferred.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning
this change raises. Boot an atmosphere fixture headless on the NVIDIA GPU (`just run-engine-headless`)
with a validation-clean log and confirm the dynamic path works:

- `sa set-atmosphere --enabled true` then `sa set-light --direction {...}` to lower the sun
  **must change the frame** (the sky disc, the lit scene, and the horizon all redden together) **without**
  a `device.wait_idle` stall in the log — a moving sun animates, it does not re-bake synchronously.
- `sa set-atmosphere --perPixelTransmittance true` plus a Moon-role directional light must show a
  transmittance-tinted moon disc distinct from the sun disc (Hapke, not limb-darkened), with the terminator
  tracking the sun direction.

Because wire types changed (`SetAtmosphereParams` gains fields, `DirectionalLight` gains `atmosphereRole`),
run `bun run check` in `editor/` (regenerates `@saffron/protocol` — never hand-edit `sa-types.ts`) and
`just e2e`, adding a `tests/e2e` case that drives `set-atmosphere` with the moon + per-pixel fields over
the control plane and asserts the `EnvironmentDto` echo carries them plus a validation-clean log. Update
`docs/content/explanations/image-based-lighting/procedural-atmosphere.md` + its hub `_index.md` row in the
same change, with the slim `What | File | Symbols` table pointing at `Ibl::bake`/`should_rebake`/`sun_transmittance`,
`atmos_skygen.slang`, `AtmosphereRole`/`AtmosphereSettings`, and `SetAtmosphereParams`.

## References

Dynamic sky re-bake + amortization:

- Sébastien Hillaire — *A Scalable and Production Ready Sky and Atmosphere Rendering Technique* (EGSR 2020, LUT parameterisation + per-LUT cost): https://sebh.github.io/publications/egsr2020.pdf
- Unreal Engine — *Sky Lights* (Real Time Capture, Time Slicing, exponential blend): https://dev.epicgames.com/documentation/unreal-engine/sky-lights-in-unreal-engine?lang=en-US
- Bruneton & Neyret — *Precomputed Atmospheric Scattering* (the transmittance-texture predecessor): https://ebruneton.github.io/precomputed_atmospheric_scattering/

Sun / moon coupling:

- Unreal Engine — *Sky Atmosphere Component* (Atmosphere Sun Light, sun/moon indices 0/1, per-pixel transmittance, 120000 lux / 0.545°, 0.26 lux / 0.568°): https://dev.epicgames.com/documentation/unreal-engine/sky-atmosphere-component-in-unreal-engine
- *Limb darkening* (Hestroffer–Magnan `I(μ)/I(1)=1−u(1−μ)`, `u≈0.6`): https://en.wikipedia.org/wiki/Limb_darkening
- Kuzminykh 2021 — *Physically Based Real-Time Rendering of the Moon* (Hapke/Lommel-Seeliger BRDF, opposition surge, earthshine, night luminance): https://elib.dlr.de/203152/1/Bachelorarbeit_Alexander_Kuzminykh_20210818.pdf
- Jensen et al. — *Night Rendering* (moon as light, earthshine ~0.19 W/m², solar ~1300 W/m² at the moon): http://graphics.ucsd.edu/~henrik/papers/night/night.pdf
