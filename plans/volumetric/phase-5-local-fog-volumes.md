# Phase 5 — Local FogVolume entities: box/sphere volumes with gizmo & noise

**Status:** COMPLETED

Part of `plans/volumetric/` (the Wronski-2014 / Hillaire-2015 froxel + analytic-height fog planset). This phase adds a placeable **`FogVolume`** hecs component — a bounded box or sphere of local participating media — that is injected into the *same* froxel grid the global fog uses, during the Phase-3 density evaluation. Because it feeds `fog_inject.slang`'s `sigma_t`/`sigma_s` alongside the analytic height base, a local volume is lit by the clustered light list, shadowed by every shadow family, temporally reprojected, integrated front-to-back, and composited by the identical stages — there is no special-case local-fog path. It ships soft edges, a per-volume exponential height slab, animated tiling 3D noise with wind advection, a `fog-volume` create preset, a viewport billboard glyph plus a passive bounds wireframe, and generic Inspector editing over `set-component-field`. It depends on Phase 3 (the froxel inject/integrate/composite core must exist). It does **not** add a new control command, does not add a new render-graph capability (the 3D transient volume + 3D dispatch already landed in Phase 2), and does not touch the scene-wide `set-fog` surface (Phase 1).

## Goal

A user creates a Fog Volume from the Create menu, sees a glyph + bounds wireframe in the viewport, moves it with the ordinary translate gizmo, and edits its density/noise/wind in the Inspector — and the froxel fog fills that box or sphere with drifting, scene-lit, self-shadowing haze. Concretely:

- A new `FogVolume` component (`engine/crates/scene/src/component.rs`, mirroring `ReflectionProbe`) with `shape` (box/sphere), `extents`/`radius`, `edge_falloff`, `density`, `albedo`, `emissive`, `phase_g`, `height_falloff`, `noise_scale`/`noise_intensity`/`noise_detail`, `wind`, `speed` — `Default` + `SceneSerialize` + `register_component!` + every component tripwire.
- A `FogVolumeUpload` snapshot + `SceneRenderer::submit_fog_volumes` (mirror `gather_reflection_probes` / `submit_reflection_probes` in `render_scene.rs`), a `FogVolumeGpu` std430 record, and a `MAX_FOG_VOLUMES` upload SSBO bound into the Phase-3 `fog_inject` descriptor set.
- The injection loop in `fog_inject.slang` extended to accumulate each in-bounds volume's `sigma_t` with a `smoothstep` soft edge, an optional exponential height slab, per-volume `albedo`/`emissive`/`phase_g`, and a two-octave tiling-3D-noise erosion drifting on `wind * time`.
- `AddEntityPreset::FogVolume` (one arm in `commands_scene.rs`, one enum variant in `dto.rs`) as the *only* spawn path; editing rides the existing generic `set-component-field`/`set-component`/`inspect` — no new per-component command.
- `BillboardKind::FogVolume` glyph + a box/sphere bounds wireframe in `host/src/overlay.rs` so a meshless volume is visible and pickable; placement reuses the existing translate/rotate/scale native gizmo unchanged.
- `FogVolume.*` `FIELD_HINTS` rows, a `CreateMenu` preset row, and a `COMPONENT_ORDER` entry — the Inspector renders the rest generically.

## Design stance (grounded in current engine practice)

The engine already snapshots a bounded, transform-positioned volume component into a per-frame GPU upload list and consumes it in a shader: `gather_reflection_probes` walks `(&Transform, &mut ReflectionProbe)` into `Vec<ReflectionProbeUpload>` (`engine/crates/assets/src/render_scene.rs`, `gather_reflection_probes` ~l.1037), `submit_reflection_probes` hands it to the renderer, and `ReflectionProbeUpload` (`engine/crates/rendering/src/ibl.rs`) carries `origin`/`influence_radius`/`box_extent`. A `FogVolume` is the same shape one consumer earlier: instead of arming a cubemap capture, its upload record is bound as a small SSBO into the froxel injection pass and read per-froxel. Nothing about the plumbing is novel — the component-registration macro, the `SceneSerialize` seam, the per-frame gather, the `AddEntityPreset` preset arm, and the generic registry-driven Inspector are all in place and are the exact attach points.

**Unified injection, not a bespoke local-fog pass.** A local volume contributes to the *same* `sigma_t`/`sigma_s` the analytic height base and global density already write in `fog_inject.slang` (Phase 3). This is the modern-correct destination — Godot's single-buffer FogVolume model, Unreal's "same authoring, froxel backend when volumetrics are on", EEVEE's froxel density accumulation — where local density is just another term summed into the medium before lighting. A separate forward-marched "local fog" pass that lit and composited volumes independently is deliberately **not** offered as a parallel path: it would double the light-eval cost, miss the shared temporal reprojection, and re-derive integration the froxel grid already does correctly. One medium, one froxel grid, one integrate.

**Soft edges and erosion noise as density modulation.** Each volume's contribution is `density * smoothstep(0, edge_falloff, distToBoundary)` so volumes blend into the surrounding medium instead of hard-clipping at their bounds (Godot `FogMaterial.edge_fade`, Unreal Local Fog Volume falloff). Animated detail is a two-octave tiling 3D noise sampled at `worldPos * noise_scale - wind * time` — a low-frequency base with a higher-frequency detail octave subtracted (the Frostbite/Horizon "erosion" shape), driven by `wind`+`speed` for drift. The noise is a precomputed **tiling** Perlin-Worley 3D texture, the authorable, seamless, industry-standard choice (Godot's `density_texture`, Frostbite cloud/fog volumes), not inline per-froxel gradient noise — a texture gives repeatable erosion shapes and tiles without seams across a large volume, and the read is trivially cheap next to the per-light loop it sits inside.

> **Scene `FogVolume` component → `gather_fog_volumes` → `FogVolumeGpu[]` SSBO → per-froxel bounds test + soft edge + noise → `sigma_t` in `fog_inject.slang` → the identical Phase-3 integrate/composite.** Local volumes branch only at the density-evaluation line; everything downstream (lighting, shadows, temporal, integration, composite) is shared unchanged.

## NO-LEGACY checklist for this phase

- The `FogVolume` component is **the** way to author local density. There is no second "local fog" primitive, no per-entity density blob on `Mesh`, no bespoke pass. One component, one `FogVolumeGpu` record, one injection loop.
- Editing rides the generic seam only. `set-component-field` / `set-component` / `inspect` drive every `FogVolume` field (registry-driven, `dirty`-free) — no new `set-fog-volume` command is added. Spawning is the single `AddEntityPreset::FogVolume` arm; no alternate creation path.
- The component is registered in *every* tripwire in one change: `register_component!(reg, FogVolume, "FogVolume")` + `BUILTIN_COMPONENT_NAMES` (`registry.rs`), `impl SceneSerialize for FogVolume` (`serde.rs`), `COMPONENT_NAMES` (`schema.rs`), the `FogVolume` interface + `Components` field + `ComponentBody` arm (`component_block.ts`), and `REGISTERED` (`luau.rs`). The `registry_is_complete` and Luau-reachability tests fail the build until all lists agree, by design.
- The `SceneRenderer` trait gains `submit_fog_volumes` and **all** impls are migrated together: `RendererScene` (the live renderer), the mock `RecordingRenderer` test stub in `render_scene.rs`, and the picking-only stub — no impl left un-migrated.
- `AddEntityPreset` gains `FogVolume` in `dto.rs` and the arm in `commands_scene.rs` in the same change; the kebab-case `"fog-volume"` string flows into the TS `EntityPreset` union via `xtask gen-protocol` (never hand-edit `sa-types.ts`).

## 1 — The `FogVolume` component

**File `engine/crates/scene/src/component.rs`.**

Add `FogVolume` next to `ReflectionProbe` (~l.774), shaped like it: a `#[derive(Clone, Copy, Debug, PartialEq)]` plain struct positioned by the entity's `Transform`, with a `Default`. Introduce a small `FogShape` enum (box/sphere) rather than reusing `Collider`'s `Shape` — a fog volume only has two bounds primitives and must not inherit capsule/hull/mesh cases it can't inject; a dedicated enum keeps the wire surface and the Inspector picker honest.

```rust
/// A placeable box/sphere of local participating media, injected into the froxel fog grid
/// during density evaluation. Positioned by the entity's `Transform`; `extents` are box
/// half-extents in local space, `radius` is the sphere radius. All optical fields default
/// to a light, drifting haze.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FogShape {
    Box,
    Sphere,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogVolume {
    pub shape: FogShape,       // box or sphere bounds
    pub extents: Vec3,         // box half-extents (local space)
    pub radius: f32,           // sphere radius
    pub edge_falloff: f32,     // soft-edge width (world units) → smoothstep to boundary
    pub density: f32,          // sigma_t at full interior
    pub albedo: Vec3,          // single-scatter albedo (sigma_s = albedo * sigma_t)
    pub emissive: Vec3,        // added in-scatter radiance
    pub phase_g: f32,          // Henyey-Greenstein anisotropy (-1..1)
    pub height_falloff: f32,   // per-volume exponential slab (0 = uniform)
    pub noise_scale: f32,      // world→noise frequency
    pub noise_intensity: f32,  // 0 = off, 1 = full erosion
    pub noise_detail: f32,     // detail-octave weight (subtractive erosion)
    pub wind: Vec3,            // advection direction
    pub speed: f32,            // advection speed (wind * speed * time)
}
```

`Default`: `shape: Box`, `extents: Vec3::splat(5.0)`, `radius: 5.0`, `edge_falloff: 1.0`, `density: 0.5`, `albedo: Vec3::splat(0.9)`, `emissive: Vec3::ZERO`, `phase_g: 0.0`, `height_falloff: 0.0`, `noise_scale: 0.2`, `noise_intensity: 0.0`, `noise_detail: 0.5`, `wind: Vec3::ZERO`, `speed: 0.1`. The defaults spawn a visible, static, uniform box so a fresh volume renders immediately; noise stays off until the user dials it in.

> Decision to record when building: **a dedicated `FogShape { Box, Sphere }`, not `Collider`'s `Shape`.** Fog injection only handles two closed bounds primitives; borrowing the collider enum would surface capsule/convex-hull/mesh options the injection loop cannot evaluate, forcing a runtime "unsupported shape" branch. One purpose-built enum keeps the picker and the shader exhaustive with no dead arms.

## 2 — Serialization and registry tripwires

**File `engine/crates/scene/src/serde.rs`.**

Add `impl SceneSerialize for FogVolume` mirroring the `ReflectionProbe` impl (~l.398): a `to_json` that emits camelCase keys and a `load_json` that reads each with `json_f32_or` / `vec3_from_json`, defaulting per field. The key names are the wire contract the Inspector and `component_block.ts` bind to — `shape` (a `"box"`/`"sphere"` string like `Collider.shape`), `extents`, `radius`, `edgeFalloff`, `density`, `albedo`, `emissive`, `phaseG`, `heightFalloff`, `noiseScale`, `noiseIntensity`, `noiseDetail`, `wind`, `speed`.

```rust
impl SceneSerialize for FogVolume {
    fn to_json(&self) -> Value {
        object([
            ("shape", Value::String(fog_shape_name(self.shape).into())),
            ("extents", vec3_to_json(self.extents)),
            ("radius", f32_value(self.radius)),
            ("edgeFalloff", f32_value(self.edge_falloff)),
            ("density", f32_value(self.density)),
            ("albedo", vec3_to_json(self.albedo)),
            ("emissive", vec3_to_json(self.emissive)),
            ("phaseG", f32_value(self.phase_g)),
            ("heightFalloff", f32_value(self.height_falloff)),
            ("noiseScale", f32_value(self.noise_scale)),
            ("noiseIntensity", f32_value(self.noise_intensity)),
            ("noiseDetail", f32_value(self.noise_detail)),
            ("wind", vec3_to_json(self.wind)),
            ("speed", f32_value(self.speed)),
        ])
    }
    fn load_json(&mut self, value: &Value) -> Result<()> { /* json_f32_or / vec3_from_json per field */ Ok(()) }
}
```

**File `engine/crates/scene/src/registry.rs`.** Add `register_component!(reg, FogVolume, "FogVolume")` beside the `ReflectionProbe` line (~l.401) and a `"FogVolume"` entry in `BUILTIN_COMPONENT_NAMES` (~l.421). The `registry_is_complete` test asserts the two lists agree, so both land in the same edit. Registration order is load-bearing (it sets the emit order); place `FogVolume` right after `ReflectionProbe` to match the panel/`COMPONENT_ORDER` slot.

> Decision to record when building: **`FogVolume` is a durable, user-editable component (registered `true`), unlike the runtime-only helpers.** It round-trips through the project file automatically because `SceneSerialize` + `register_component!` is the whole contract — no `document.rs` change, exactly as `ReflectionProbe` needs none.

## 3 — Placement preset

**File `engine/crates/protocol/src/dto.rs`.** Add a `FogVolume` variant to `AddEntityPreset` (~l.158), kebab-cased to `"fog-volume"` by the enum's `#[serde(rename_all = "kebab-case")]`. This is the only wire change and it rides the already-registered `add-entity` command — no new `CommandSpec`, no new codegen tripwire beyond the enum itself.

**File `engine/crates/control/src/commands_scene.rs`.** Add an `AddEntityPreset::FogVolume` arm to the `add-entity` match (mirror the `ReflectionProbe` arm ~l.1165): create an entity named "Fog Volume", `add_component(FogVolume::default())`, nudge the `Transform` translation up so a fresh volume is not buried at the origin, bump `scene_version`, select it, return the entity ref. Update the command's help string (~l.1098) to list `fog-volume`. Editing needs nothing here — `set-component-field` / `set-component` / `inspect` are registry-driven.

## 4 — Component wire schema

**File `engine/crates/protocol/src/schema.rs`.** Add `"FogVolume"` to `COMPONENT_NAMES` (~l.661) — this drives the `Components` aggregate and the `ComponentBody` union in the generated openrpc.
**File `engine/xtask/src/protocol/component_block.ts`.** Add a hand-authored `FogVolume` interface (mirror the `ReflectionProbe` interface ~l.82, with the camelCase fields above and `shape: "box" | "sphere"`), add `FogVolume?: FogVolume;` to the `Components` interface (~l.201), and add `| FogVolume` to the `ComponentBody` union (~l.224).
**File `engine/xtask/src/protocol/luau.rs`.** Add `"FogVolume"` to `REGISTERED` (~l.52) so the reachability walk finds the wire shape.
Then regenerate: `cargo run -p xtask -- gen-protocol` rewrites `schemas/control/*.generated.json`, `sa.generated.luau`, and `editor/src/protocol/sa-types.ts` (the `EntityPreset` union gains `"fog-volume"`; `ComponentBody` gains `FogVolume`). **Never** hand-edit `sa-types.ts`.

## 5 — Gather and upload seam

**File `engine/crates/rendering/src/froxel_fog.rs`** (the fog module stood up in Phase 2/3). Add a `FogVolumeUpload` snapshot (mirror `ReflectionProbeUpload` in `ibl.rs` ~l.188) carrying the world transform + all optical fields, and a std430 `FogVolumeGpu` record (byte-asserted like `GpuLight` in `gpu_types.rs`) with the shape packed as a `u32` and the box/sphere bounds in world space. Add a `MAX_FOG_VOLUMES` cap and an SSBO in the fog descriptor set that `fog_inject` reads.

```rust
/// Per-frame snapshot of one FogVolume, transform baked to world space, handed to the
/// froxel injection pass without the renderer depending on the scene.
#[derive(Debug, Clone, Copy)]
pub struct FogVolumeUpload {
    pub world_from_local: Mat4,   // for oriented-box bounds test + noise sampling frame
    pub center: Vec3,
    pub shape: FogShape,          // → u32 in FogVolumeGpu
    pub extents: Vec3,
    pub radius: f32,
    pub edge_falloff: f32,
    pub density: f32,
    pub albedo: Vec3,
    pub emissive: Vec3,
    pub phase_g: f32,
    pub height_falloff: f32,
    pub noise_scale: f32,
    pub noise_intensity: f32,
    pub noise_detail: f32,
    pub wind: Vec3,
    pub speed: f32,
}
```

**File `engine/crates/assets/src/render_scene.rs`.** Add `gather_fog_volumes(scene) -> Vec<FogVolumeUpload>` cloning `gather_reflection_probes` (~l.1037): walk `(&Transform, &FogVolume)`, cap at `MAX_FOG_VOLUMES`, bake `world_from_local` from the entity's world transform. Add `submit_fog_volumes(&[FogVolumeUpload])` to the `SceneRenderer` trait (beside `submit_reflection_probes` ~l.91), call it in `render_scene` right before the Phase-3 fog passes run, implement it on `RendererScene` (forwards to a `Renderer` setter that uploads the SSBO), and add the stub to the `RecordingRenderer` mock (~l.1489, record `Call::FogVolumes(n)`) and any picking-only stub — all impls in the same change.

> Decision to record when building: **loop a small global upload list per froxel, not a per-volume froxel cull.** With a handful of volumes the bounds test inside the injection loop is cheaper than a second cull pass, and it mirrors `gather_reflection_probes`' bounded global list exactly. If volume counts grow, culling volumes into the froxel grid the way lights are culled is the correct upgrade — deferred to Future, seam left clean.

## 6 — Volume injection: soft edges, height slab, animated noise

**File `engine/assets/shaders/fog_inject.slang`** (created in Phase 3). Extend the density-evaluation section — the point where `sigma_t` accumulates the analytic height base + global density, *before* the per-light in-scatter loop — with a loop over the bound `FogVolumeGpu[]`. This adds **no** new render-graph capability: the 3D transient volume, the 3D dispatch, and the fog descriptor set already exist from Phases 2–3; this is one more SSBO binding and one loop.

For each volume: reconstruct the froxel-center world position (already computed for the height base), transform into the volume's local frame via `world_from_local`'s inverse, compute signed distance to the box (`abs` of local pos minus `extents`) or sphere (`length(local) - radius`), take `distToBoundary` and apply `edge = smoothstep(0, edge_falloff, distToBoundary)` so density fades to zero at the surface. Multiply by an optional exponential height slab `exp(-height_falloff * max(localUp, 0))` so a box can itself be a local ground-haze slab. Sample the tiling noise and accumulate.

```hlsl
// inside the froxel-center density eval, after the height/global base:
for (uint i = 0; i < fog.volumeCount; ++i) {
    FogVolumeGpu v = fogVolumes[i];
    float3 local = mul(v.localFromWorld, float4(worldPos, 1.0)).xyz;
    float d = (v.shape == FOG_SHAPE_BOX)
        ? sdBox(local, v.extents)          // <=0 inside
        : length(local) - v.radius;
    if (d >= v.edgeFalloff) continue;      // outside soft margin
    float edge  = smoothstep(v.edgeFalloff, 0.0, d);      // 1 interior → 0 at boundary
    float slab  = (v.heightFalloff > 0.0) ? exp2(-v.heightFalloff * max(local.y, 0.0)) : 1.0;
    float n     = fogNoise(worldPos, v);   // two-octave tiling erosion, drift = wind*speed*time
    float sigma = v.density * edge * slab * n;
    sigma_t += sigma;
    sigma_s += v.albedo * sigma;           // folded into the shared scatter term
    emissive += v.emissive * sigma;        // added to in-scatter with the global emissive
    // phase_g blends toward this volume's anisotropy weighted by its share of sigma_t
}
```

`fogNoise` samples a precomputed **tiling** Perlin-Worley 3D texture at `worldPos * v.noiseScale - v.wind * v.speed * time`: a low-frequency base octave minus a higher-frequency detail octave scaled by `noiseDetail`, the whole modulated by `noiseIntensity` (0 → the volume is uniform, the texture sample is skipped). Bake the noise volume once into a persistent `Image3D` at renderer init via a small `fog_noise_bake.slang` compute pass, reusing the Phase-2 `Image3D` + 3D-dispatch infra, and bind it as a `COMBINED_IMAGE_SAMPLER` with a linear-repeat sampler (the `create_linear_repeat_sampler` precedent in `global_sdf.rs`).

> Decision to record when building: **a precomputed tiling Perlin-Worley 3D noise texture, not inline procedural gradient noise.** The texture tiles seamlessly across an arbitrarily large volume, gives the repeatable erosion shapes artists expect (Frostbite/Horizon erosion, Godot `density_texture`), and costs one trilinear fetch inside a loop already dominated by light eval. Inline per-froxel procedural noise is cheaper to wire but seams at volume scale and cannot reproduce Worley-based erosion — it is not offered as a parallel path.

> Decision to record when building: **`phase_g` is blended per-volume, weighted by each volume's `sigma_t` share.** A froxel inside two overlapping volumes with different anisotropy resolves to a `sigma_t`-weighted mean `g`, so a single HG evaluation in the light loop stays correct without a per-light-per-volume phase blow-up. Signed/negative-density order-independent accumulation (Godot's atomic-packed model) is a Future refinement, noted below.

## 7 — Viewport visibility: billboard glyph + bounds wireframe

**File `engine/crates/host/src/overlay.rs`.** A `FogVolume` entity has no mesh, so it needs a glyph to be visible and pickable, and a passive wireframe so its bounds read at a glance. Add `FogVolume` to the `BillboardKind` enum (~l.33), classify it in `billboard_kind` (~l.321, `scene.has::<FogVolume>(entity)` → `BillboardKind::FogVolume`), and add a render arm in the billboard match (~l.615) drawing a small cloud/fog glyph (a new `add_fog_icon` helper alongside `add_bulb_icon` / `add_camera_icon`). In `build_debug_overlays` (~l.979), draw the bounds: for a box, `add_world_oriented_box(vertices, view_projection, &model, extents, color, w, h)` (~l.865) using the entity's world matrix + `extents`; for a sphere, either three `add_world_aabb`-style rings or an axis-aligned bounds via `add_world_aabb` from `center ± radius` (~l.763) — reuse the existing builders, add no new geometry primitive.

Placement reuses the existing translate/rotate/scale `build_native_gizmo` unchanged — there is deliberately **no** box-resize gizmo in the product. Extents are numeric `vec3` Inspector fields with a passive wireframe, exactly matching `Collider.halfExtents` and `ReflectionProbe.boxExtent`.

> Decision to record when building: **the wireframe is passive (debug-overlay range), the glyph is on-top (billboard range).** The box/sphere bounds sit in the depth-tested overlay range so geometry occludes them naturally; the glyph sits in the on-top range so a volume is always clickable even when its wireframe is behind a wall — matching how light bulbs and camera glyphs stay pickable.

## 8 — Editor: create preset, component order, field hints

- **File `editor/src/app/CreateMenu.tsx`.** Add one `CREATE_PRESETS` row `{ label: "Fog Volume", preset: "fog-volume", icon: CloudFog }` beside the `reflection-probe` row (~l.52), importing `CloudFog` from `lucide-react`. The `preset` string is typed from the regenerated `EntityPreset` union, so it compiles only after `gen-protocol` runs.
- **File `editor/src/lib/componentOrder.ts`.** Add `"FogVolume"` to `COMPONENT_ORDER` right after `"ReflectionProbe"` (~l.22) so the Inspector section and Add-Component menu order it correctly.
- **File `editor/src/components/fieldRenderer.tsx`.** Add `FogVolume.*` rows to `FIELD_HINTS` (mirror the `ReflectionProbe` block ~l.134): `shape` → an `enum` with `{box, sphere}` options (like `Collider.shape` ~l.167); `albedo`/`emissive` → `color3`; `density`/`edgeFalloff`/`phaseG`/`heightFalloff`/`noiseIntensity`/`noiseDetail` → `slider` with sensible ranges (`phaseG` at `-0.99..0.99`); `extents`/`wind` → `vec3`; `radius`/`noiseScale`/`speed` → `number`. No other editor code — `InspectorPanel` renders the component generically via `renderField`, add/remove is generic, and edits ride `set-component-field`.

## Out of scope (later phases)

- **No scene-wide fog controls.** The `set-fog` command and the global froxel medium are Phase 1 / Phase 3; this phase only adds bounded local density that rides the existing inject/integrate/composite. A `FogVolume` in a scene with fog disabled contributes nothing — it is a strict addition to the froxel medium, which only runs in volumetric mode.
- **No per-light interaction beyond the shared loop.** Per-light volumetric-scattering intensity and cast-volumetric-shadow flags are Phase 4; a `FogVolume` is lit by whatever lights the Phase-3/4 injection already evaluates for its froxels, with no volume-specific light gating.
- **No aerial perspective coupling.** Phase 6's aerial-perspective volume is a separate medium; local volumes live only in the near/mid fog grid.

## Known interactions (note, don't over-engineer)

- **Overlapping volumes.** Densities sum in the injection loop, which reads correctly for additive haze. Signed/negative density (a volume that *subtracts* fog) and order-independent overlap resolution (Godot's atomic-packed signed-int buffer) are the modern end state but only matter when many volumes overlap — keep the summed loop now; the seam (one accumulate line) upgrades cleanly when volume counts grow.
- **Temporal reprojection of a moving volume.** A volume dragged by the gizmo shifts density between frames; the Phase-4 history blend reprojects the *linear* scatter+extinction, so a moving volume smears slightly for a few frames but does not ghost through the non-linear transmittance. The 5% blend already tuned for moving lights is sufficient; no volume-specific reset is warranted.
- **Volume larger than `fogFar`.** A volume extending past the froxel range only injects density within `fogFar`; beyond it the analytic height fog carries the far field. This is correct — a local volume is a near/mid-field primitive by design.
- **Bounds test frame vs. noise frame.** Both use `world_from_local` so a rotated box's soft edge and its noise drift stay in the volume's own frame — a box rotated 45° erodes along its local axes, matching the wireframe the user sees.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises; the `registry_is_complete` and Luau-reachability tests must stay green (all component tripwire lists updated together). Spawn a volume over a shadow-casting occluder with a scene light and volumetric fog on: `sa add-entity --preset fog-volume`, then boot that fixture headless (`just run-engine-headless`) on the NVIDIA GPU and confirm a validation-clean log plus a bounded, drifting patch of local density that is lit by the scene light and self-shadows — and that the billboard glyph is pickable (`sa pick`) and the bounds wireframe renders. Drive an edit over the control plane — `sa set-component-field` on `FogVolume.density`, `FogVolume.noiseIntensity`, and `FogVolume.wind` — and confirm the frame changes. Because a wire type changed (a new registered `FogVolume` component + the `fog-volume` preset), run `bun run check` in `editor/` (regenerates `@saffron/protocol` — `ComponentBody` must gain `FogVolume` and the `EntityPreset` union `"fog-volume"`; never hand-edit `sa-types.ts`) and `just e2e`, adding a `tests/e2e` case that spawns via `add-entity --preset fog-volume`, reads the entity back with `inspect`, drives `set-component-field` on `FogVolume.density`/`FogVolume.wind`, and asserts the round-trip plus a validation-clean log. Add the "Local fog volumes" page under `docs/content/` (what a `FogVolume` is, the box/sphere bounds + soft edge, the height slab, the tiling-noise erosion + wind drift, and the `What | File | Symbols` table: `engine/crates/scene/src/component.rs` `FogVolume`; `engine/crates/assets/src/render_scene.rs` `gather_fog_volumes`; `engine/assets/shaders/fog_inject.slang`; `engine/crates/host/src/overlay.rs` `BillboardKind::FogVolume`) and its hub `_index.md` row in the same change.
