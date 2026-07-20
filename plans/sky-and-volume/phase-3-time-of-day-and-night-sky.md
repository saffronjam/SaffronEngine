# Phase 3 — Time-of-day driver and the night sky: ephemeris, artist curves, stars, and moon phase

**Status:** COMPLETED

Part of `plans/sky-and-volume/` (dynamic sky, time-of-day, and volumetric clouds). Phases 1 and 2 made the Hillaire sky *dynamic* — the atmosphere re-bake is amortized behind a sun-angle threshold (not the synchronous `device.wait_idle` full re-bake), the "Sun" and "Moon" `DirectionalLight`s read their color/intensity/disc radiance from the Transmittance LUT, and the sky-light IBL (SH diffuse + amortized specular + DDGI retint) reconverges per sun-move. But the sun still only moves when a *user* drags the light, and the moment it drops below the horizon there is nothing behind the atmosphere — no stars, no Milky Way, no phased moon, and a physically-dim night either crushes to black or, if exposure is raised, reads as washed-out day. This phase makes the sky move *on its own over a day* and *populates the night*: an ephemeris + artist-curve time-of-day driver that positions the Phase-1 sun/moon lights and authors appearance, a physically-based star field + Milky Way extincted by the existing transmittance ledger, a synodic moon phase advanced across days, and a scotopic/Purkinje low-light adaptation in the tonemap path.

It depends on Phase 1 (the amortized re-bake gate, the atmosphere-role sun/moon `DirectionalLight`s, the transmittance-coupled key lights and moon disc) and Phase 2 (the SH sky-light + the per-frame `atmosphere_live` refresh). It does **not** re-plan the sky LUT chain, does **not** rebuild aerial perspective or the froxel fog, and renders **no clouds** — clouds are Phases 4–6, and this phase only *reserves* the cloud coverage and cloud-type curve channels for them (it never renders either curve against a volume that does not yet exist). The star field composites as background radiance *behind* the atmosphere on the existing `(transmittance, in-scatter)` reads — it never introduces a second transmittance ledger.

## Goal

- **Time-of-day is scene state, driven by one command.** A `TimeOfDaySettings` block on `SceneEnvironment` (date, normalized time-of-day scalar, latitude/longitude, enabled + manual-override flags, and the appearance curves) round-trips through `environment_to_json`/`environment_from_json` exactly like `FogSettings`/`AtmosphereSettings`, edited by a single `set-time-of-day` merge command modeled on `set-fog`, scriptable from `sa` with no per-command CLI code, surfaced back through `EnvironmentDto`. It is *not* a `RenderSettings` field.
- **Direction is physical, appearance is authored (the hybrid).** The sun/moon light *directions* come from an NREL-SPA-class ephemeris (astronomically correct arc for the locale/date), feeding the Phase-1 sun-move re-bake gate; *appearance* (exposure, tint) comes from 1D artist curves indexed by **sun elevation** (a stable normalized-day proxy), authored with the reused monotone-cubic curve widget. A manual override bypasses the ephemeris entirely for cinematics.
- **The night has real content.** A catalog-driven star field (Yale Bright Star Catalog → HDR luminance via the Pogson ratio, spectral type → blackbody RGB, rotated by local sidereal time) and a Milky Way cubemap render behind the sky, extincted by the atmosphere transmittance + in-scatter so they fade through twilight and feed true HDR luminance to bloom/eye-adaptation.
- **The moon is phased and calibrated.** The Phase-1 moon disc's in-shader phase tracks the sun geometrically; the ephemeris drives its direction so the synodic phase advances across in-game *days*, and it is calibrated to physical night luminance with earthshine on the dark limb.
- **A dim night reads as night.** A scotopic/Purkinje low-light adaptation folds into the mandatory tonemap pass (rod/cone blend + blue shift + acuity loss), gated by a night-adaptation scalar the driver derives from sun elevation.
- **One write path per setting.** `set-time-of-day` carries the driver state; the exposure/tint curves live on the same block. When the exposure curve is active it is *the* authority for tonemap exposure (the manual `set-exposure` is overridden, not run beside it); disabling it returns control to `set-exposure`.

## Design stance (grounded in current engine practice)

The engine already exposes every input this phase needs. The sun is the first `DirectionalLight` picked by `gather_directional_light` (`engine/crates/assets/src/render_scene.rs`) and re-aimed by its world rotation; `drive_env_bake` turns `sun_dir = -light_dir` into the atmosphere bake, and Phase 1 widened that bake's gate so a moving sun is amortized rather than stalling. The visible-sky pass is a fullscreen triangle (`sky.slang` `fragmentMain`) that reconstructs the world view ray from the inverse view-projection — the exact place a per-pixel night sky and Milky Way belong. The Transmittance and Sky-View LUTs are already published as sampling views (`Ibl::transmittance_view`, `Ibl::sky_view_lut_view`) and consumed by the aerial-perspective/fog composite, so extincting stars is a *read* of the same ledger, not a new one. The tonemap pass (`tonemap.slang` `computeMain`) is the single mandatory HDR→display seam and applies its transforms to scene-linear `lin` before the operator — the correct home for a scotopic pre-transform. The editor's only 1D curve widget (`ToneCurve` in `editor/src/components/ToneCurve.tsx`, monotone-cubic Fritsch–Carlson via `evalCurve`/`monotoneSlopes`) is a drop-in for a scalar-vs-day ramp. So this phase is *driver + content + adaptation* over built seams, with one genuinely new resource: a baked star catalog SSBO and its instanced-point pass.

**Hybrid ephemeris + curves — direction physical, look authored.** The production-correct time-of-day model is not "spin a skybox" and not "keyframe every color by wall-clock". The sun/moon *direction* comes from an NREL Solar Position Algorithm–class model (date, latitude, longitude → azimuth/elevation, ±0.0003°; NREL SPA, https://www.nrel.gov/midc/spa/, Reda & Andreas https://docs.nrel.gov/docs/fy08osti/34302.pdf), so shadows, the arc, and star/moon rise-set are astronomically correct for the locale; *appearance* comes from artist 1D curves indexed by **sun elevation** — not the clock — so a "sunset" key lands at the right geometry at any latitude/season (Ultra Dynamic Sky "Simulate Real Sun", https://www.ultradynamicsky.com/Documentation/V6/6-20). A manual override reproduces the current behaviour (the authored `DirectionalLight.direction` is used verbatim) for cinematics. This is the modern answer that replaces both a hand-cranked skybox spin and a wall-clock color ramp; there is no second, decoupled "sunset color" path.

**Physically-based star field, extincted by the one transmittance ledger.** Stars are driven from a real catalog (Yale Bright Star Catalog, ~9,100 stars to magnitude 6.5, http://tdc-www.harvard.edu/catalogs/bsc5.html), not an artist star texture: apparent magnitude → HDR luminance via the Pogson 2.512 ratio (`Δm = 2.512^(m_ref − m)`, 5 magnitudes = 100×; Jensen et al. https://graphics.stanford.edu/~henrik/papers/nightsky/, joren https://jorenjoestar.github.io/post/realistic_stars/realistic_stars/), spectral type → blackbody RGB (temperature→CIE, http://www.vendian.org/mncharity/dir3/blackbody/), and the whole equatorial-frame catalog rotated by local sidereal time (`R = Rz(−LMST)·Ry(lat − π/2)`, LMST from Julian date + longitude, ~15.04°/hr). Each star is extincted by sampling `Ibl::transmittance_view()` toward its direction, and the atmosphere in-scatter (`Ibl::sky_view_lut_view()`) is added over it — so daylight and twilight wash the stars out automatically rather than a hand-faded alpha. Feeding true HDR luminance (not a magnitude-derived sprite size) lets bloom + eye-adaptation produce the glare.

**Moon phase from geometry, calibrated to night luminance.** The Phase-1 moon disc already computes its phase in-shader from the sun direction (`dot(sunDir, moonNormal)`, terminator tracks the sun) with a Hapke/Lommel-Seeliger BRDF and earthshine. Because *both* the sun and the moon are now driven from the ephemeris, the synodic phase advances across days for free (the moon's ephemeris position relative to the sun evolves over the ~29.53-day synodic month; DigitalRune https://digitalrune.github.io/DigitalRune-Documentation/html/975dcbca-5c87-4f33-a372-0cfe7f50cebe.htm). This phase adds only the lunar ephemeris that drives the disc's direction, the date→phase advance, and a night-luminance calibration (full moon ~0.26 lux, disc ~2500 cd/m², earthshine ~0.15–0.19 W/m²). No baked moon-phase texture is introduced.

**Scotopic / Purkinje adaptation so night reads as night.** A physically dim night must run a low-light tone-mapping model, not a flat brightness cut or a blue color-grade (which reads as desaturated day). A rod/cone blend with the Purkinje blue shift (rod peak ~505 nm relatively brightens blues, dims reds), desaturation, and reduced acuity is applied in the tonemap path, keyed on a night-adaptation scalar the driver derives from sun elevation (Kirk & O'Brien 2011, https://dl.acm.org/doi/10.1145/2010324.1964937). One code path, gated to zero by day.

> **ephemeris (date/lat/long → sun/moon direction) → write the Phase-1 role-tagged `DirectionalLight`s → existing `gather_directional_light` + `drive_env_bake` re-bake the sky/IBL through the Phase-1 gate; appearance curves (indexed by sun elevation) drive tonemap exposure + sky/ambient tint at submit time; the visible-sky pass adds the Milky Way + extincted stars behind the atmosphere; the tonemap pass applies scotopic adaptation.** No new transmittance ledger, no clouds, no second sky command.

## NO-LEGACY checklist for this phase

- `set-time-of-day` is **the** command for the driver — one `SetTimeOfDayParams`, one `TimeOfDaySettings` block, one merge handler. Time-of-day knobs are never smuggled onto `set-environment`/`set-atmosphere`, and there is no second "day-night enable" boolean beside `enabled`.
- The ephemeris is **the** source of sun/moon direction when time-of-day is `enabled` and not `manual_override`. The driver *derives* the role-tagged `DirectionalLight.direction` each frame (a runtime application, no `scene_version` bump, no undo entry) — it does not add a parallel "sun angle" field the renderer reads instead of the light. Manual override falls back to the authored `direction`; there is exactly one active direction source at a time.
- The exposure curve is **the** exposure authority while active — the driver calls `Renderer::set_exposure` per frame from `evalCurve(exposureCurve, sunElevationNorm)`, and the manual `set-exposure` is overridden (not layered) until the curve is disabled. No two exposure writers running together.
- The moon disc stays the Phase-1 `atmos_skygen` feature; this phase feeds it the ephemeris moon direction + date-derived phase and calibrates luminance. It does **not** stand up a second moon renderer or a baked phase texture.
- Stars/Milky Way composite as **background radiance extincted by the existing transmittance ledger** (`Ibl::transmittance_view` + `Ibl::sky_view_lut_view`), never additively on top of the sky (which would keep them visible in daylight) and never through a second atmosphere volume.
- The new `TimeOfDaySettings` block + `SetTimeOfDayParams` + `set-time-of-day` are registered across **all** protocol tripwire lists (`dto.rs`, `command.rs` `COMMANDS`/`COMMAND_FIXTURES`/`DTO_TYPE_NAMES`/`scene_domain`, `codegen.rs` `ts_decls`/`struct_fragments`, `schema.rs` hand-authored `TimeOfDaySettingsDto` + `Environment` block, `tests/inventory.rs`, `tests/schema_fragments.rs`, `tools/check-control-schema/check.ts`) in one change — the build and the schema-fragment byte-equivalence test refuse to compile otherwise.

## 0 — Time-of-day scene state, serde, and the `set-time-of-day` protocol surface

The plumbing prerequisite: a `TimeOfDaySettings` block mirroring `FogSettings` end-to-end, so date/time/locale and the appearance curves persist in the project `environment` block and are drivable from `sa` before any driver or shader work.

| What | File | Symbols |
|---|---|---|
| The state block + `Default` | `engine/crates/scene/src/environment.rs` | add `TimeOfDaySettings` (mirror `FogSettings`): `enabled: bool`, `manual_override: bool`, `time_of_day: f32` (normalized `[0,1)`), `year/month/day: i32`, `latitude/longitude: f32` (deg), `day_length_seconds: f32`, `exposure_curve`, `tint_curve`, `coverage_curve`, and `cloud_type_curve` control-point ramps; nest `time_of_day: TimeOfDaySettings` on `SceneEnvironment` + its `Default` |
| JSON round-trip | `engine/crates/scene/src/serde.rs` | add `time_of_day_to_json`/`time_of_day_from_json` (template: `fog_to_json`/`fog_from_json`), a `tod_curve_to_json`/`_from_json` (array of `{x,y}` via `f32_value`), and the `("timeOfDay", …)` entry in `environment_to_json`/`environment_from_json`; camelCase keys, per-field defaults on read |
| Wire DTO | `engine/crates/protocol/src/dto.rs` | add `SetTimeOfDayParams` (template: `SetFogParams` — `Option<T>` per field, `json` escape hatch, `coerce::opt_boolean` for the two bools, curves as `Option<Vec<[f32;2]>>`); reuse `EnvironmentDto` as the result |
| Command table + tripwires | `engine/crates/protocol/src/command.rs` | `COMMANDS` row `set-time-of-day` (params `SetTimeOfDayParams`, result `EnvironmentDto`) right after `set-fog`; `COMMAND_FIXTURES` `("set-time-of-day","time-of-day-noon")`; `DTO_TYPE_NAMES` `SetTimeOfDayParams`; `scene_domain()` list after `set-fog` |
| Codegen lists | `engine/crates/protocol/src/codegen.rs` | `decl_entry!(SetTimeOfDayParams)` in `ts_decls()`; `frag_entry!(SetTimeOfDayParams)` in `struct_fragments()` |
| Hand-authored schema | `engine/crates/protocol/src/schema.rs` | a `TimeOfDaySettingsDto` block (template: the `FogSettingsDto` block, `additionalProperties:false` + full `required`) and the `timeOfDay` `$ref` + `required` entry on the `Environment` block |
| Test tripwires | `engine/crates/protocol/tests/inventory.rs`, `engine/crates/protocol/tests/schema_fragments.rs` | `SetTimeOfDayParams` in the `inventory![…]` list and a `check!(SetTimeOfDayParams, …)` fragment assertion |
| e2e fixture body | `tools/check-control-schema/check.ts` | a `paramsForFixture` case `"time-of-day-noon"` returning `{ time: 0.5 }` (or `{ timeOfDay: 0.5 }`) |
| Control handler | `engine/crates/control/src/commands_scene.rs` | in `register_scene_commands`, `reg.register::<SetTimeOfDayParams, EnvironmentDto>("set-time-of-day", …)` — copy the `set-fog` merge: `environment_to_json` → merge `params.json` first → merge each `Some` field into the `timeOfDay` sub-object (validate `time_of_day ∈ [0,1]`, `latitude ∈ [−90,90]`, `longitude ∈ [−180,180]` with `Error::command`) → `environment_from_json` → assign `active_scene().environment` → bump `scene_version` → return `environment_dto(ctx)` |

> Decision to record when building: **curves are a compact `[[f32;2]]` array on the wire (control-point ramp), not a baked LUT.** The renderer evaluates them per frame; the editor authors them with the same monotone-cubic model (`evalCurve`) the wire round-trips, so host and editor agree on the curve exactly. Reuse `EnvironmentDto` as the result (fewer tripwires: no new result type in `DTO_TYPE_NAMES`/`codegen`/`inventory`).

## 1 — The ephemeris driver

Position the Phase-1 sun/moon lights from an NREL-SPA-class solar/lunar ephemeris each frame, feeding the existing re-bake gate, with a manual-override escape hatch.

| What | File | Symbols |
|---|---|---|
| The ephemeris math (new module) | `engine/crates/assets/src/time_of_day.rs` (new, or a `saffron-geometry` helper) | `julian_date(year,month,day,tod)`, `solar_position(jd, lat, long) -> (azimuth, elevation)` (NREL SPA topocentric subset — atmospheric-refraction terms optional), `lunar_position(jd, lat, long)`, `local_sidereal_time(jd, long)`, `dir_from_az_el(az, el) -> Vec3`; unit tests pinning a known date/locale against SPA reference angles |
| The per-frame apply | `engine/crates/assets/src/render_scene.rs` | a new `drive_time_of_day(scene)` called at the top of `render_scene` (before `gather_directional_light`): when `env.time_of_day.enabled && !manual_override`, compute sun/moon directions and write the role-tagged sun/moon `DirectionalLight.direction` via `scene.for_each::<&mut DirectionalLight, _>` (Phase-1 role); a runtime derive, no persistence |
| Feeds the existing bake gate | `engine/crates/assets/src/render_scene.rs`, `engine/crates/rendering/src/ibl.rs` | unchanged: `gather_directional_light` picks the sun, `drive_env_bake` sets `sun_dir = -light_dir` → `request_env_bake` → Phase-1 `should_rebake` sun-angle threshold amortizes the re-bake; the moon light rides the Phase-1 second-light path |

> Decision to record when building: **the driver mutates `DirectionalLight.direction` in place as a transient runtime application, never a persisted edit.** A day-night cycle must not spam the undo stack or bump `scene_version` 60×/s. The *source of truth* is `time_of_day` on `SceneEnvironment`; the light direction is derived from it each frame and consumed by the existing gather. This runs in the shared `render_scene`, so the cycle plays in both the editor host and the exported player.

> Decision to record when building: **manual override is the whole-ephemeris escape hatch.** With `manual_override` (or `enabled == false`), the driver is a no-op and the authored `DirectionalLight.direction` is used verbatim — the exact current behaviour, so cinematics that hand-key the sun are unaffected.

## 2 — Appearance curves indexed by sun elevation

Author exposure and tint as 1D curves over the day and apply them at submit time. Reserve the cloud coverage and cloud-type channels for Phases 4–6 (neither curve is rendered until `CloudSettings` exists).

| What | File | Symbols |
|---|---|---|
| The normalized-day proxy | `engine/crates/assets/src/render_scene.rs` | `drive_time_of_day` returns `sun_elevation_norm ∈ [0,1]` (e.g. `saturate(elevation_deg/90*0.5 + 0.5)`) — the stable index for every look curve, not the clock |
| Exposure curve → tonemap | `engine/crates/rendering/src/renderer.rs` | when `exposure_curve` is active, the driver calls `Renderer::set_exposure(evalCurve(exposureCurve, sunElevationNorm))` each frame (overriding the manual `set-exposure` while active) |
| Tint curve → sky/ambient | `engine/crates/assets/src/render_scene.rs`, `engine/crates/rendering/src/ibl.rs` | the tint curve (an RGB ramp — reuse the `ToneCurve` master/R/G/B channels) multiplies a per-frame factor into `SkyRenderSettings.intensity` (via `submit_sky`) and the ambient tint (`ddgi_sky` / `use_sky_for_ambient` feed), a submit-time modulation with no re-bake |
| Curve evaluation (host side) | `engine/crates/assets/src/time_of_day.rs` | a Rust `eval_monotone_curve(points, x)` matching the editor's `evalCurve` (Fritsch–Carlson monotone cubic) so host and editor agree byte-for-behaviour |

> Decision to record when building: **cloud coverage and cloud type are named curve channels reserved for Phases 4–6, not rendered here.** `CloudSettings` does not exist until Phase 4; the curve model on `TimeOfDaySettings` carries both channels' control points, but Phase 3 wires only exposure + tint to live targets. When Phase 4 lands `CloudSettings`, its coverage and type read `evalCurve(coverageCurve, sunElevationNorm)` and `evalCurve(cloudTypeCurve, sunElevationNorm)` — the seam is left clean, never a cloud curve driving a nonexistent volume.

## 3 — The night sky: star field + Milky Way

Render a catalog-driven star field and Milky Way behind the sky, extincted by the existing transmittance + in-scatter reads so they fade through twilight and feed true HDR luminance to bloom/eye-adaptation.

| What | File | Symbols |
|---|---|---|
| Baked catalog + resource | `engine/assets/` (a compact baked Yale BSC5 data file) + `engine/crates/rendering/src/stars.rs` (new) | `StarCatalog`: load BSC5 at startup into an SSBO of `{ dir_j2000: Vec3, luminance: f32, rgb: Vec3 }` (magnitude → `Δm = 2.512^(m_ref − m)` HDR luminance, `m_ref ≈ 7.0`; spectral type → blackbody RGB); the baked table lives under `engine/assets/` (an `xtask` bake step is acceptable) |
| Star draw pass (new shader) | `engine/assets/shaders/stars.slang` (new) | `vertexMain` (instanced, one star per instance: rotate `dir_j2000` by the sidereal matrix, project through `view_proj`, emit a small PSF quad), `fragmentMain` (Gaussian PSF × HDR luminance × RGB × `T(dir)` from the transmittance LUT); additive into scene color in the sky region, before geometry |
| Milky Way + extinction (extend) | `engine/assets/shaders/sky.slang` | in `fragmentMain`, before returning the atmosphere `envCube` sample, add a Milky Way cubemap sample (rotated by the sidereal matrix) extincted by `T(dir)`; the sky-view in-scatter dominates by day so both fade — bind the transmittance + sky-view LUT views + Milky Way cube + a `NightSkyParams` UBO into the `Sky` set |
| Sky-pass wiring | `engine/crates/rendering/src/ibl.rs` | extend `Sky` (its descriptor set / `SkyPush`→`NightSkyParams`) with the sidereal matrix, moon dir, star intensity, and the extra bindings; `Sky::submit` carries the night params; `record_sky` + a new star-points record run in the sky region; bind `Ibl::transmittance_view()` + `Ibl::sky_view_lut_view()` |

> Decision to record when building: **stars are extincted background radiance, not additive-on-top.** `star_out = star_HDR · T(dir)`, composited into the background *before* the atmosphere in-scatter is added, so a bright twilight sky-view in-scatter swamps a dim star and it fades exactly as a real star does. Reuse the LUTs already published (`transmittance_view`/`sky_view_lut_view`) — this is a read of the one ledger, never a second atmosphere evaluation. Feed HDR luminance (Pogson), not a magnitude-baked sprite size, so bloom + eye-adaptation own the glare; give the point a PSF so a sub-pixel star does not shimmer under camera motion.

## 4 — Moon phase and calibration

Feed the Phase-1 moon disc the lunar ephemeris and advance the synodic phase across days; calibrate to physical night luminance with earthshine.

| What | File | Symbols |
|---|---|---|
| Moon direction from ephemeris | `engine/crates/assets/src/render_scene.rs`, `engine/crates/assets/src/time_of_day.rs` | `drive_time_of_day` writes the Phase-1 moon `DirectionalLight.direction` from `lunar_position(jd, …)`; the disc's phase (`dot(sunDir, moonNormal)`, Phase-1 `atmos_skygen`) then tracks the sun and advances over the ~29.53-day synodic month for free as the ephemeris positions evolve |
| Phase + earthshine + luminance | `engine/assets/shaders/atmos_skygen.slang` (Phase-1 disc), `engine/crates/rendering/src/ibl.rs` | pass the date-derived illuminated fraction `(1 − cos(elongation))/2` and calibrate disc luminance (~2500 cd/m²) + earthshine on the dark limb (~0.15–0.19 W/m², bluish); the moon key light stays the Phase-1 transmittance-tinted second atmosphere light (~0.26 lux full) |

> Decision to record when building: **no baked moon-phase texture.** The phase is geometric from the two ephemeris directions; the terminator is always correct for the time and advances across days without a texture atlas. This phase adds the lunar ephemeris + the luminance calibration, and reconciles with — does not rebuild — the Phase-1 disc.

## 5 — Scotopic / Purkinje low-light adaptation

Make a physically-dim night read as night in the mandatory tonemap pass.

| What | File | Symbols |
|---|---|---|
| The adaptation op (new) | `engine/assets/shaders/tonemap_ops.slang` | `public float3 scotopicAdapt(float3 lin, float nightFactor)`: a rod/cone blend keyed on the local + adapting luminance with the Purkinje blue shift (rod peak ~505 nm relatively brightens blues, dims reds), desaturation, and reduced acuity; identity at `nightFactor == 0` |
| Wired into the tonemap | `engine/assets/shaders/tonemap.slang` | in `computeMain`, apply `scotopicAdapt(lin, push.nightFactor)` to scene-linear `lin` **before** `grade`/`tonemapAndEncode` — one code path, gated off by day; extend the `Push` struct with `nightFactor` |
| The night-adaptation scalar | `engine/crates/assets/src/render_scene.rs`, `engine/crates/rendering/src/renderer.rs` | the driver derives `night_factor` from sun elevation (0 above the horizon → 1 deep night) and pushes it into the tonemap (a new setter alongside `set_exposure`); a future auto-exposure could key it on measured luminance instead |

> Decision to record when building: **key the mesopic strength on the sun-elevation night factor, not a flat cut.** A brightness cut or blue LUT reads as desaturated day; the rod/cone blend + blue shift is the physically-motivated model (Kirk & O'Brien). It composes with the tonemap operator and the creative LUT — it is a pre-transform on `lin`, not a replacement for either.

## 6 — Editor scrubber, curves, and docs

| What | File | Symbols |
|---|---|---|
| Time-of-day panel section | `editor/src/panels/EnvironmentPanel.tsx` | a Time of Day section (template: the Fog section): a `Row` per field — an enabled `Switch`, a manual-override `Switch`, a time scrubber (`NumberDrag` 0–24 h ↔ normalized), date `NumberDrag`s, latitude/longitude, and the `ToneCurve` widget bound to the exposure/tint curves; drive through a new `patchTod` + `todCoalescerFor` + `recordTodEdit` mirroring `patchFog`/`fogCoalescerFor`/`recordFogEdit`, bracketed by the shared `onDragStart`/`onDragEnd` |
| Typed client wrapper | `editor/src/control/client.ts` | `setTimeOfDay(tod: Partial<Environment["timeOfDay"]>): Promise<Environment>` (template: `setFog`), auto-checked against the generated `CommandName` union after `bun run gen:protocol` |
| Curve widget reuse | `editor/src/components/ToneCurve.tsx` | reuse `ToneCurve`/`evalCurve`/`monotoneSlopes`/`CurvePoint` for the scalar-vs-day ramp (the widget shell + monotone eval; the RGB tint uses the master/R/G/B channels, exposure uses the master channel only) |
| Docs pages + hub | `docs/content/explanations/image-based-lighting/time-of-day.md` (new), `night-sky.md` (new), `_index.md` | two `## Pages` rows: `time-of-day` (ephemeris + hybrid curves + the driver, `time_of_day.rs` · `drive_time_of_day`) and `night-sky` (BSC5 stars, Milky Way, moon phase, scotopic, `stars.slang` · `StarCatalog`); template: `procedural-atmosphere.md` (TOML front matter, `math = true`, slim `What | File | Symbols` prose, a mermaid flow, KaTeX for the Pogson/LMST formulas) |

## Out of scope (later phases)

- **Clouds and cloud weather rendering (Phases 4–6).** No cloud shape, march, shadow, or ledger fold here. The coverage and cloud-type curve channels are *reserved* on `TimeOfDaySettings` and consumed by `CloudSettings` when Phase 4 lands; Phase 3 renders neither curve.
- **Real-time ray-traced sky GI for the moving sun's bounce (unscheduled).** The Phase-2 SH sky retints ambient; capturing the sun's *moving indirect bounce* is the RT-probe end state, not built here.
- **Altitude / space-view atmosphere (unscheduled).** `cameraAltitude` stays hardcoded 0 in the LUT push (ground observer); the ephemeris is topocentric for a ground locale. Flight/orbital views are deferred.
- **Auto-exposure driving the night factor from measured luminance (unscheduled).** The scotopic strength keys on the sun-elevation night factor this phase; a metered-luminance eye-adaptation is a later refinement.

## Known interactions (note, don't over-engineer)

- **Exposure authority.** While the exposure curve is active, the driver's per-frame `set_exposure` is the sole exposure writer; the Render Stats manual `set-exposure` is overridden until the curve is disabled. Surface this in the panel so a user is not surprised the manual slider "does nothing" mid-cycle.
- **Re-bake churn.** The driver moves the sun continuously, but the Phase-1 sun-angle threshold + amortized specular reconvergence absorb it; do not add a second dirty flag. A very fast scrub crosses the threshold every frame — that is the amortized path working, not a bug.
- **Star extinction vs. `atmosphere_live`.** `transmittance_view`/`sky_view_lut_view` carry a live tint only when `Ibl::atmosphere_live()`; with a non-atmosphere sky, gate star extinction to a neutral pass-through (stars still show, just un-reddened), mirroring how the fog composite gates `useSkyLut`.
- **Manual sun + night sky.** With `manual_override`, the ephemeris is off but the sidereal star rotation still needs a time source — drive the sidereal matrix from the `time_of_day` scalar + date regardless of the override (override bypasses only the *sun/moon direction*, not the calendar).

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Boot a fixture scene with an atmosphere-enabled sky headless (`just run-engine-headless`, on the NVIDIA GPU) with a validation-clean log and confirm the day-night cycle drives the frame: `sa set-time-of-day --enabled true --timeOfDay 0.05` (pre-dawn) must render extincted stars + a phased moon and read as *night*, while `--timeOfDay 0.5` (noon) shows a bright sky with no stars, and each `--timeOfDay` scrub must change the frame relative to the last (the sun/moon move, exposure follows the curve). Confirm `sa set-time-of-day --manualOverride true` returns the sun to its authored direction. Because wire types changed (`SetTimeOfDayParams`, the `TimeOfDaySettings` block), run `bun run check` in `editor/` (regenerates `@saffron/protocol` — the `Environment` TS type gains `timeOfDay`; never hand-edit `sa-types.ts`) and `just e2e`, adding a `tests/e2e` case that drives `set-time-of-day` over the control plane and asserts the `EnvironmentDto` echo carries the scrubbed time + a validation-clean log. Add the two `docs/content/explanations/image-based-lighting/` pages (`time-of-day`, `night-sky`) and their hub `_index.md` rows in the same change, using the slim `What | File | Symbols` table: `time_of_day.rs` (`drive_time_of_day`), `stars.slang`/`engine/crates/rendering/src/stars.rs` (`StarCatalog`), `sky.slang` (`fragmentMain`, Milky Way + extinction), `atmos_skygen.slang` (moon phase), `tonemap.slang`/`tonemap_ops.slang` (`scotopicAdapt`), `engine/crates/scene/src/environment.rs` (`TimeOfDaySettings`), and `SetTimeOfDayParams`.

## References

Ephemeris + hybrid time-of-day:

- NREL — Solar Position Algorithm (SPA) reference: https://www.nrel.gov/midc/spa/
- Reda & Andreas — Solar Position Algorithm for Solar Radiation Applications (NREL/TP-560-34302): https://docs.nrel.gov/docs/fy08osti/34302.pdf
- Ultra Dynamic Sky 6.20 — Time of Day, color curves, Simulate Real Sun (the hybrid model): https://www.ultradynamicsky.com/Documentation/V6/6-20

Star field + night sky:

- Yale Bright Star Catalog (BSC5, ~9,100 naked-eye stars): http://tdc-www.harvard.edu/catalogs/bsc5.html
- Rendering Astronomic Stars (joren) — LMST rotation, magnitude→luminance, blackbody color: https://jorenjoestar.github.io/post/realistic_stars/realistic_stars/
- Jensen et al. — A Physically-Based Night Sky Model (SIGGRAPH 2001): https://graphics.stanford.edu/~henrik/papers/nightsky/
- Jensen et al. — Night Rendering (moon as light, earthshine, scotopic vision): http://graphics.ucsd.edu/~henrik/papers/night/night.pdf
- Blackbody color datafiles (Mitchell Charity) — temperature → RGB: http://www.vendian.org/mncharity/dir3/blackbody/

Moon phase + earthshine:

- DigitalRune — Sky (Milky Way cubemap, moon phase in shader, sun disc, scattering): https://digitalrune.github.io/DigitalRune-Documentation/html/975dcbca-5c87-4f33-a372-0cfe7f50cebe.htm
- Kuzminykh 2021 — Physically Based Real-Time Rendering of the Moon (Hapke BRDF, earthshine, night luminance): https://elib.dlr.de/203152/1/Bachelorarbeit_Alexander_Kuzminykh_20210818.pdf

Low-light adaptation:

- Kirk & O'Brien — Perceptually Based Tone Mapping for Low-Light Conditions (Purkinje): https://dl.acm.org/doi/10.1145/2010324.1964937
- Purkinje effect (mesopic blue shift): https://en.wikipedia.org/wiki/Purkinje_effect
