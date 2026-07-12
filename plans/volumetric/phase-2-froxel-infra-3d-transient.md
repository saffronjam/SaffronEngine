# Phase 2 — 3D transient images, 3D compute dispatch & the froxel-grid module: standing up the volume infra

**Status:** COMPLETED — delivered scope is the new render-graph capability (§1 3D transient images, §2 `groups_z` dispatch, §3 the froxel-grid module + `FogGridParams`, §6 the device-backed round-trip proof). §4 (the persistent `scatterExtinction` volumes + descriptor sets) and §5 (the `lighting.slang` shared-module refactor + `hgPhase`) are **relocated to Phase 3** and marked below: a NO-LEGACY refactor and its unexercised scaffolding land atomically with their first consumer (the injection shader), where the extraction is verifiable end-to-end. Their specs are retained here for reference; Phase 3 §0 builds them.

Part of `plans/volumetric/` (volumetric + height fog as one scattering subsystem). This phase ships the one real infrastructure gap the froxel pipeline needs and nothing else: 3D (`depth > 1`, `TYPE_3D`) images out of the transient pool, a `groups_z` dispatch path, a `froxel_fog.rs` sub-state module (grid constants that align to the clustered cull, a `FogGridParams` UBO with an exponential-Z CPU mirror + unit test, descriptor layouts, and the ping-pong `scatterExtinction` + `integration` volumes), and the `lighting.slang` shared-module refactor that makes the Phase-3 injection shader able to `import` the light-eval and shadow helpers. It stands entirely alone (dependency `—`): it renders **no** fog, adds **no** `set-fog` command (that is Phase 1) and touches **no** wire DTO, so `xtask gen-protocol` does not run this phase. The whole slice is proven by a throwaway debug froxel-fill dispatch that writes a known pattern into a 3D transient volume and samples it back, asserting the round-trip through the render graph.

## Goal

The renderer can allocate a frustum-shaped `rgba16f` 3D volume as a per-frame scratch, dispatch a compute grid over it in three dimensions, and describe the froxel grid the same way the light cull already describes its clusters — with the CPU depth mapping locked to the shader by a unit test. Concretely:

- `TransientResources::acquire_image_3d` + a `TransientImage3D` slot (`transient.rs`), backed by the existing `Image3D` wrapper, copying the keyed grow-only discipline of `acquire_image` verbatim — the correct transient fit for a resizable frustum scratch.
- A `groups_z` parameter on the one `Renderer::add_compute_pass` helper (`renderer.rs`), with all 18 screen-space callers migrated to pass `1` in the same change (NO-COMPAT — no second helper, no bespoke body per fog pass).
- A `froxel_fog.rs` module: `FROXEL_GRID_{X,Y,Z}` + quality tiers, a `FogGridParams` UBO reusing the cluster's exponential view-space Z distribution, a `froxel_to_cluster` mapping helper, and a `froxel_grid_matches_shader` CPU-mirror unit test (the analog of `cluster_grid_matches_shader`), plus the descriptor layouts and the `scatterExtinction`/`integration` 3D volumes.
- `lighting.slang` refactored: `clusterIndexFor`, `distanceAttenuation`, `punctual`, `pcfShadow`, `pointShadow`, `rayQueryShadow` promoted into a **public**, resource-parameterized shared module (the `giprobe` module is the template), a new public `hgPhase` lifted from `atmos_skyview.slang`, and every existing caller (`mesh.slang`, `meshlet.slang`) migrated in the same change.
- A `froxel_debug.slang` compute shader + `import_image_3d` wiring that writes a coordinate pattern into a 3D transient volume and samples it into a readback buffer — the verification harness, deleted by Phase 3 when the real inject/integrate passes replace it.

## Design stance (grounded in current engine practice)

These are built the modern, technically-correct way — the froxel grid aligns to the engine's existing exponential-Z cluster partition and reuses the Global-SDF 3D-volume pattern, not a bespoke fog-only depth curve or a persistent-only volume that fights the graph.

**The render graph is already 3D-ready; only the pool and the dispatch helper are not.** `RenderGraph::import_image_3d` exists and its own docstring states a 3D image is "tracked identically to a 2D image for barrier purposes — the barrier transitions the whole image and dimensionality is irrelevant" (`render_graph.rs`, `import_image_3d` ~l.494; it delegates to `import_image` with a `COLOR` aspect). Barrier derivation is dimension-agnostic, `RgUsage::StorageImageRwCompute` (~l.50 → `GENERAL`) and `RgUsage::SampledReadCompute` (~l.52) apply unchanged, and a compute pass body can already `cmd_dispatch(gx, gy, gz)` — the GDF composite does exactly that (`renderer.rs`, `gdf-composite` body ~l.6688). So the graph needs no change. The two real gaps are surgical: `TransientResources::acquire_image` only ever calls `Image::new`, which hardcodes `image_type(TYPE_2D)` + `depth: 1` (`resources.rs` ~l.272-279), so no `depth > 1` image can leave the pool; and `Renderer::add_compute_pass` hardcodes `cmd_dispatch(groups_x, groups_y, 1)` (`renderer.rs` ~l.8586). A bespoke per-fog-pass body is **not** the answer — extending the one helper is the NO-COMPAT fit.

**The Global-SDF albedo cache is the canonical precedent for the volume, and `Image3D` is the type to reuse — not a new one.** GDF already owns a persistent `rgba16f` (`R16G16B16A16_SFLOAT`, `GDF_ALBEDO_FORMAT` ~l.59) `Image3D` (`global_sdf.rs`, `albedo: Image3D` ~l.296), created `STORAGE | SAMPLED`, written as a `STORAGE_IMAGE` in `GENERAL` from a compute scatter pass and sampled as a `COMBINED_IMAGE_SAMPLER` with a linear sampler, imported via `import_image_3d` and dispatched `gx,gy,gz`. The froxel volume is the same shape; the only difference is lifetime. The `Image3D` wrapper (`resources.rs`, `Image3D::new` ~l.420 → `ImageType::TYPE_3D` image + `ImageViewType::TYPE_3D` view) is complete and working, so the transient path wraps it exactly as the 2D path wraps `Image` — one 3D image type, one 3D acquire, no third concept.

**The froxel grid is a strict refinement of the cluster grid, so it borrows the cluster's math.** The clustered cull runs on a fixed `16×9×24` exponential-Z partition (`lighting.rs`, `CLUSTER_GRID_X/Y/Z` ~l.36-40) whose slice boundaries are `z = -near * (far/near)^(k/Nz)` (`cluster_aabb` ~l.919; `clusterIndexFor` in `lighting.slang` ~l.466 inverts it as `zSlice = log(depth/near)/log(far/near) * gridZ`). The fog grid uses a finer XY tiling (`160×90`) and its own Z count but the **identical** exponential distribution, so a fog froxel maps to the containing cull cluster through `clusterIndexFor` and reads that cluster's light list — the exact ReSTIR reuse pattern. The CPU mirror (`froxel_grid_matches_shader`) locks this, following `cluster_grid_matches_shader` (`lighting.rs` ~l.1052).

> **`Image::new` (2D-only) + `Image3D` (persistent 3D) → add `acquire_image_3d` (transient 3D wrapping `Image3D`) + `groups_z` on `add_compute_pass` → `froxel_fog.rs` grid consts + `FogGridParams` UBO + descriptor layouts + `scatterExtinction`/`integration` volumes → `lighting.slang` helpers promoted to a public module → a debug froxel-fill proves write+sample through the graph.** No fog is rendered; the exponential-Z mapping is the one correctness contract, pinned by a CPU-mirror test.

## NO-LEGACY checklist for this phase

- **One 3D image path.** The transient volume reuses the existing `Image3D` type; there is no third 3D wrapper and no `depth`-carrying variant of `ImageDesc`. `Image` stays strictly 2D (its `extent: vk::Extent2D` is load-bearing across every offscreen target and screen-space set) — the transient pool gains a parallel `TransientImage3D` slot, mirroring `TransientImage`, not replacing it.
- **`add_compute_pass` gains `groups_z`; every caller migrates in the same change.** All 18 screen-space call sites (`renderer.rs` l.7036–l.8480) pass `1` explicitly — the `z = 1` signature does not survive next to a `z = groups_z` one. No sibling `add_compute_pass_3d`.
- **The froxel-grid constants live in exactly one place.** `FROXEL_GRID_{X,Y,Z}` + the quality tiers are defined once in `froxel_fog.rs`; the CPU mirror and the shader both read them, and `froxel_grid_matches_shader` fails the build if the two drift — the same tripwire discipline as `cluster_grid_matches_shader`.
- **The `lighting.slang` helpers are extracted once and every caller migrates together.** `mesh.slang` (`import lighting;` l.1) and `meshlet.slang` (`import lighting;` l.15) both move to the new public surface in the same change — no file keeps a private copy, and the helpers are not duplicated into the fog shader (the anti-pattern `restir_initial.slang` shows today, where the cluster/light math is copy-duplicated).
- **The debug froxel-fill is a verification fixture, not a feature.** It is gated off by default and is deleted by Phase 3 when the real `fog_inject`/`fog_integrate` passes land — it is never left running beside them.

## 1 — 3D transient images: teaching the pool `depth > 1`

**File `engine/crates/rendering/src/transient.rs`.**

Add `acquire_image_3d` beside `acquire_image` (~l.204) and a `TransientImage3D` slot beside `TransientImage` (~l.81), stored on `FrameTransient` in a new `images_3d: Vec<TransientImage3D>` field (~l.90). The keyed grow-only logic is a direct copy of the 2D acquire: look up the slot by `&'static str` key, return the cached `(image, view)` when the descriptor matches, else build a fresh `Image3D` and replace the slot. Reclaim in `begin_frame` (~l.115) is fence-safe by the same contract — a 3D slot is only reallocated after its frame's fence has signalled.

```rust
struct TransientImage3D {
    key: &'static str,
    image: Image3D,          // the existing TYPE_3D wrapper — reused, not reinvented
    extent: vk::Extent3D,    // the key's match test (depth is part of identity)
    format: vk::Format,
    usage: vk::ImageUsageFlags,
}

impl TransientResources {
    /// Acquire a transient 3D image under a stable `key`, keyed exactly like
    /// [`acquire_image`]. Returns `(image, view)` for `import_image_3d`
    /// (initial layout `UNDEFINED`). Backed by [`Image3D`].
    pub fn acquire_image_3d(
        &mut self,
        frame: usize,
        key: &'static str,
        extent: vk::Extent3D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> crate::Result<(vk::Image, vk::ImageView)> { /* mirror acquire_image */ }
}
```

The froxel key array follows `BLOOM_MIP_KEYS` (~l.28): a static `&'static str` per logical volume so each keeps an independent slot and independent barriers.

```rust
pub(crate) const FROXEL_VOLUME_KEYS: [&str; 2] = ["froxel-integration", "froxel-debug"];
```

> Decision to record when building: **reuse `Image3D`, do not fold a 3D extent into `ImageDesc`.** `Image3D::new` already builds the `TYPE_3D` image + `TYPE_3D` view correctly and is the type GDF depends on. Widening `ImageDesc` to a `vk::Extent3D` + `image_type` would force every 2D offscreen/screen-space caller to reason about depth for no gain and would duplicate `Image3D`'s body — a second code path for one concept. The transient 3D acquire wraps the one 3D type, exactly as the 2D acquire wraps the one 2D type.

> Decision to record when building: **the integration volume is transient; the `scatterExtinction` history is not.** A same-frame scratch that the composite consumes and discards is the textbook transient fit, so `acquire_image_3d` serves it. Temporal reprojection (Phase 4) reads *last frame's* scattering, which the transient pool's grow-only slot does not preserve deterministically across frames — so the `scatterExtinction` current/history pair is a persistent module-owned `Image3D` ping-pong (GDF-style), stood up in §4. This phase keys the two distinctly so Phase 4 can swap them without re-plumbing; it does not build the reprojection.

## 2 — 3D compute dispatch: `groups_z` on the one helper

**File `engine/crates/rendering/src/renderer.rs`.**

`add_compute_pass` (~l.8547) records a `RgPass::compute` body that binds the PSO + set, optionally pushes, and dispatches. Add a `groups_z: u32` parameter and forward it into `cmd_dispatch(cmd, groups_x, groups_y, groups_z)` (the one line at ~l.8586). Then update all 18 call sites (l.7036–l.8480) to pass `1`, so the 2D screen-space passes read exactly as before and the fog passes (Phase 3) pass `FROXEL_GRID_Z.div_ceil(tz)`.

```rust
fn add_compute_pass(
    &self,
    graph: &mut RenderGraph,
    name: &'static str,
    pipeline: &Arc<crate::Pipeline>,
    set: vk::DescriptorSet,
    accesses: &[(RgResource, RgUsage)],
    push: Option<Vec<u8>>,
    groups_x: u32,
    groups_y: u32,
    groups_z: u32, // new — 1 for every screen-space pass
) { /* … cmd_dispatch(cmd, groups_x, groups_y, groups_z) … */ }
```

> Decision to record when building: **extend the single helper, do not add `add_compute_pass_3d` or hand-roll a body per fog pass.** The graph places no constraint on `z`; the only thing 2D-bound is this convenience helper's hardcoded `1`. A second helper or a bespoke `RgPass::compute` body per fog pass (the GDF-composite shape) would duplicate the bind/push/dispatch scaffold for no reason — the fog passes are uniform full-grid dispatches with a single push, exactly what the helper is for. GDF keeps its bespoke body only because it fans out many per-region pushes in one pass; the froxel passes do not.

## 3 — The froxel-grid module and its `FogGridParams`

**File `engine/crates/rendering/src/froxel_fog.rs` (new).**

A sub-state module shaped like the cluster half of `lighting.rs` + the resource half of `global_sdf.rs`: the grid constants, the quality tiers, the `FogGridParams` UBO, the CPU depth mapping + its mirror test, and (in §4) the volumes + descriptor sets. No fog is evaluated here — this is the grid's data model.

```rust
pub const FROXEL_GRID_X: u32 = 160;
pub const FROXEL_GRID_Y: u32 = 90;

/// Z slice count per quality tier — high keeps the full 128, medium/low drop to 64.
/// XY drops to 128×72 at low. The exponential distribution is identical to the cull.
pub enum FroxelQuality { Low, Medium, High }

/// `rgba16f`, matching the GDF albedo cache's `GDF_ALBEDO_FORMAT`.
pub const FROXEL_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FogGridParams {
    inverse_projection: [[f32; 4]; 4], // froxel-center reconstruction (mirror ClusterParams)
    inverse_view: [[f32; 4]; 4],
    grid_size: [u32; 4],  // froxel x, y, z, + flags (light count reused from the cull)
    screen_size: [f32; 4], // w, h, + pad
    z_planes: [f32; 4],   // fogNear = max(cameraNear, 0.1), fogFar = 128, ...
}
```

The depth mapping and the froxel→cluster helper are pure CPU, mirroring `cluster_aabb` / `clusterIndexFor`:

```rust
/// View-space Z at the near edge of froxel slice `k` — the same exponential curve the
/// cull uses (`lighting.rs::cluster_aabb`): `-near * (far/near)^(k / Nz)`.
pub fn froxel_slice_view_z(near: f32, far: f32, k: u32, nz: u32) -> f32 { /* … */ }

/// The cull cluster (`16×9×24`) containing a fog froxel — reconstruct the froxel-center
/// pixel + view Z and index the cull grid exactly as `clusterIndexFor` does. Lets the
/// Phase-3 injection read the coarse cluster's light list per froxel.
pub fn froxel_to_cluster(params: &FogGridParams, fx: u32, fy: u32, fz: u32) -> u32 { /* … */ }
```

The mirror test locks the two mappings, following `cluster_grid_matches_shader` (`lighting.rs` ~l.1052):

```rust
#[test]
fn froxel_grid_matches_shader() {
    // 1. the froxel Z curve is the cull Z curve at the finer slice count
    // 2. froxel_to_cluster round-trips into the same cull index clusterIndexFor produces
    // 3. FROXEL_FORMAT == GDF_ALBEDO_FORMAT (one rgba16f volume convention)
}
```

> Decision to record when building: **reuse the cull's exponential-Z distribution, not a fog-specific curve.** The whole point of the froxel→cluster mapping is that a fog froxel lands inside one cull cluster and reads its already-built light list (the ReSTIR reuse). A different Z curve would break that correspondence and force a dedicated fog light cull. `fogFar = 128 m` bounds the grid; beyond it the analytic height fog (Phase 1) carries the far field.

## 4 — Descriptor layouts and the volumes

> **Relocated to Phase 3 (§0).** The persistent volumes + descriptor sets have no Phase-2-observable behavior (nothing writes or reads them until injection exists), so building them here is unexercised scaffolding that clippy flags as dead code. They land in Phase 3 alongside their first consumer. Spec below is the authoritative reference Phase 3 §0 follows.

**File `engine/crates/rendering/src/froxel_fog.rs`, `descriptors.rs`, `pipelines.rs`.**

The module owns the persistent `scatterExtinction` ping-pong (two `Image3D`, `STORAGE | SAMPLED`, `GENERAL`-written / linear-sampled — the GDF albedo pattern), swapped each frame; the `integration` volume comes from `acquire_image_3d` (§1) as a same-frame scratch. The descriptor layout mirrors the GDF write/read split: a `STORAGE_IMAGE` binding for the volume the compute pass writes and a `COMBINED_IMAGE_SAMPLER` binding (with `linear_sampler`) for the volume a later pass samples, plus the `FogGridParams` UBO. Build the PSOs through `Pipelines::build_compute_multi` (`pipelines.rs` ~l.2582) when a pass needs the bindless set alongside the froxel set, `build_compute` (~l.2570) otherwise.

```rust
struct FroxelFog {
    scatter: [Image3D; 2],     // current / history ping-pong (persistent, GDF-style)
    scatter_index: usize,      // flips per frame
    grid_params: Buffer,       // host-mapped FogGridParams UBO
    write_set: vk::DescriptorSet,   // STORAGE_IMAGE(volume) + FogGridParams
    sample_set: vk::DescriptorSet,  // COMBINED_IMAGE_SAMPLER(volume) + FogGridParams
    quality: FroxelQuality,
}
```

Bump the `STORAGE_IMAGE` and `COMBINED_IMAGE_SAMPLER` pool budgets in `descriptors.rs` by the froxel set count so the pool never undersizes, the same discipline the screen-space and GDF sets follow.

> Decision to record when building: **`scatterExtinction` is a persistent pair; `integration` is transient.** This is the §1 lifetime split made concrete — the two volumes have genuinely different lifetimes (one crosses frames for reprojection, one does not), so one pool path does not fit both. The debug proof (§6) exercises the transient path, which is the new capability; the persistent pair is a straight `Image3D` copy of the GDF cascade allocation.

## 5 — The `lighting.slang` shared-module refactor

> **Relocated to Phase 3 (§0).** This is a NO-LEGACY refactor (extract the light/shadow helpers → migrate `mesh.slang`/`meshlet.slang`) that is only verifiable *with* its first consumer: the correct atomic cutover extracts the helpers, adds the fog injection caller, and proves mesh/meshlet render byte-identically — all in one change. Extracting in Phase 2 with no new consumer is a refactor for a caller that does not exist yet. It lands in Phase 3 §0. Spec below is the authoritative reference.

**File `engine/assets/shaders/lighting.slang` + `mesh.slang` + `meshlet.slang`.**

`lighting.slang` is `module lighting;` (l.1) whose light-eval and shadow helpers are file-private and hard-decorated to the mesh's descriptor sets: `clusterIndexFor` (~l.466), `distanceAttenuation` (~l.371), `punctual` (~l.427), `pcfShadow` (~l.142), `pointShadow` (~l.128), `rayQueryShadow` (~l.282). A compute fog pass cannot `import lighting` and call them because their bindings are baked in. Promote them into a **public**, resource-parameterized surface — the `giprobe` module is the exact template: `giprobe.slang` declares **no** bindings and takes its atlases + placement as parameters (`public float4 ddgiSampleIrradiance(DdgiVolume vol, Sampler2D irradianceAtlas, Sampler2D distanceAtlas, …)`), so the mesh keeps its textures at set 5 and the resolve pass keeps its own set, with no shared set number. Do the same for the light+shadow helpers: each takes the cluster/light SSBOs, the shadow maps, and the TLAS as parameters rather than reading fixed bindings. Add a public `hgPhase(cosTheta, g)` lifted verbatim from `atmos_skyview.slang` (~l.52) — today the only Henyey-Greenstein in the tree, and it lives in the atmosphere path, not the punctual lighting path.

```hlsl
// public, resource-parameterized — no bindings declared here (giprobe shape)
public uint  clusterIndexFor(ClusterParams p, float2 pixel, float viewZ);
public float distanceAttenuation(float dist, float range);
public float hgPhase(float cosTheta, float g);            // lifted from atmos_skyview.slang
public float pcfShadow(Sampler2DShadow map, float4x4 viewProj, float3 worldPos);
public float pointShadow(SamplerCube staticCube, SamplerCube dynCube, /* … */ float3 worldPos);
public float rayQueryShadow(RaytracingAccelerationStructure tlas, float3 worldPos, float3 toLight, float maxDist);
// `punctual` splits: the shared attenuation+cone+shadow gate is public; the BRDF stays in the mesh path,
// the HG phase is the fog path's substitute (fog uses radiance * hgPhase, not brdf * ndotl).
```

Migrate `mesh.slang` and `meshlet.slang` to the public surface in the same change — they pass their existing bindings as arguments; the shade result is byte-identical.

> Decision to record when building: **parameterize the helpers (giprobe shape), do not create a shared-bindings module the fog pass imports at the mesh's set numbers.** A shared-bindings module would force the fog compute layout to match the mesh's set/binding decorations, coupling two unrelated pipeline layouts. The `giprobe` approach — no bindings in the module, resources passed in — lets the fog pass declare its own froxel-friendly layout while calling the one copy of the light+shadow math. This is why `punctual`'s BRDF is *not* dragged into the shared module: the fog path substitutes `hgPhase`, so only the attenuation/cone/shadow gate is shared.

## 6 — The debug froxel-fill: proving write + sample through the graph

**File `engine/assets/shaders/froxel_debug.slang` (new) + `renderer.rs`.**

A two-dispatch harness that exercises the whole new path with no scene dependency. Pass A (`StorageImageRwCompute` on the volume) writes a deterministic coordinate pattern — e.g. `float4(fx, fy, fz, 1) / gridSize` — into the `integration` transient volume, dispatched `FROXEL_GRID_X/8, FROXEL_GRID_Y/8, FROXEL_GRID_Z/8` through the `groups_z`-extended helper (§2). Pass B (`SampledReadCompute` on the same volume, imported via `import_image_3d`) reads it back through a linear sampler into a host-mapped storage buffer at a handful of froxel centers; the host asserts the samples equal the pattern. This proves: a `depth > 1` image left the transient pool (§1), a 3D grid dispatched (§2), and the render graph derived the `GENERAL → SHADER_READ_ONLY → GENERAL` transitions on a 3D image (the `import_image_3d` claim). Gate the whole harness behind a `SAFFRON_FROXEL_DEBUG` env check read once at renderer init so it never runs in a normal frame.

> Decision to record when building: **the debug fill is a fixture, deleted in Phase 3.** It is the phase's verification, not a shipped feature. Phase 3 replaces `froxel_debug.slang` + its two dispatches with the real `fog_inject`/`fog_integrate` passes on the same volumes and descriptor sets — the debug shader is removed in that same change, not left behind a flag (NO-LEGACY).

## Out of scope (later phases)

No fog is injected, integrated, or composited — Phase 3 (`phase-3-froxel-inject-integrate-composite.md`) adds `fog_inject.slang` / `fog_integrate.slang` and the composite, consuming the volumes, descriptor sets, `FogGridParams`, and the public `lighting` helpers this phase stands up. Temporal reprojection and the `scatterExtinction` history swap are Phase 4 — this phase keys the ping-pong distinctly and leaves the history seam clean but builds no reprojection. Local `FogVolume` density (Phase 5) and the aerial-perspective volume (Phase 6) both reuse `acquire_image_3d` + `import_image_3d` from here without extending them. No `set-fog` field, no DTO, no persistence, no editor panel row touches this phase — the wire surface is Phase 1's and Phase 3's.

## Known interactions (note, don't over-engineer)

- **Transient lifetime vs. reprojection history.** `acquire_image_3d` is fence-safe and its slot survives `MAX_FRAMES_IN_FLIGHT` frames, but reprojection needs a *deterministic* read of last frame's scattering, which the grow-only pool does not guarantee. The §1/§4 split (transient `integration`, persistent `scatterExtinction` pair) is the resolution; do not try to make the transient pool serve cross-frame history.
- **Froxel XY finer than the cull XY.** `160×90` fog froxels map into `16×9` cull tiles, so many froxels share one cluster's light list. That is intended (the cull is coarse; the fog grid is fine for integration resolution) and matches the ReSTIR reuse. A dedicated fog-froxel light cull is a possible future refinement, not a Phase-2 requirement — `froxel_to_cluster` is sufficient.
- **`build_compute_multi` set count.** The Phase-3 injection will need the bindless set alongside the froxel set; `build_compute_multi` already supports several set layouts (`pipelines.rs` ~l.2582, the DDGI trace's set-0/1/2 case). Stand up the froxel layout now so Phase 3 only adds the shader, not the PSO plumbing.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Confirm `cargo test -p saffron-rendering froxel_grid_matches_shader` passes (the CPU/GPU depth mapping is locked, the analog of `cluster_grid_matches_shader`). Boot a real fixture scene headless with the harness enabled — `SAFFRON_FROXEL_DEBUG=1 just run-engine-headless` on the NVIDIA GPU — and confirm a validation-clean log with the sampled-back pattern matching the written coordinate pattern, and **no visual change to the scene** (this phase renders no fog). This phase touches no wire DTO, so `xtask gen-protocol`, `bun run check`, and `just e2e` are not required here — Phase 1 (`set-fog`) and Phase 3 (`fog.mode`) carry those. Add the render-graph 3D-transient-image capability to the docs in the same change: a note in `docs/content/explanations/frame-and-render-graph/limits-and-seams.md` (the transient pool now allocates `TYPE_3D` volumes via `acquire_image_3d`, dispatched in 3D through the `groups_z` helper, imported by `import_image_3d`) and its hub `docs/content/explanations/frame-and-render-graph/_index.md` row, with the `What | File | Symbols` table pointing at `transient.rs` (`acquire_image_3d`, `TransientImage3D`), `renderer.rs` (`add_compute_pass` `groups_z`), and `froxel_fog.rs` (`FROXEL_GRID_X`, `FogGridParams`, `froxel_grid_matches_shader`).
