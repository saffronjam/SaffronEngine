# Phase 1 — DFAO trace correctness + a signal-correct denoiser

**Status:** COMPLETED

## Goal / context

The per-pixel DFAO prepass already runs, but it looks wrong on the current per-mesh field: the
fully-open Sponza roof shows zebra/moiré, and flat walls show low-frequency "stain" blobs. Two
defects on the *consuming* side of the field cause the zebra independent of the (coarse) field
itself, and this phase fixes them so the path is presentable before the bigger bake/GDF phases land.
The blobs are a field-resolution problem and are out of scope here (they belong to P2's densified
MDF) — this phase is strictly trace correctness + denoise + the indirect band split.

Three concrete defects:

1. **The cone trace self-occludes on open surfaces.** `sdfSkyOcclusion` in
   `engine/assets/shaders/sdf.slang` starts each cone march at `kStart = max(0.15, voxel)` and
   floors the step at `kStepFloor = max(0.1, voxel * 0.5)` — both coupled to the coarsest occluder's
   world voxel size. On a coarse field a marginal start sometimes lands inside the trilinearly
   reconstructed surface band of the very surface being shaded, so a flat open roof intermittently
   reads as partly occluded; the per-pixel/per-frame cone rotation then turns that into the moving
   zebra. It also traces only **6 equal-weight cones**, which under-samples the hemisphere and biases
   the AO toward the cone azimuths.

2. **The denoiser is the wrong filter for the signal.** The SDF-AO temporal pass reuses
   `engine/assets/shaders/ssgi_accum.slang` (the `request_ssgi_accum` PSO), whose history step does a
   3×3 neighborhood **min/max color clamp** on the full rgba. The SDF-AO map packs
   `(ao, octBentNormal.x, octBentNormal.y, viewZ)`; clamping the *octahedral-encoded* normal channels
   componentwise against a neighborhood AABB re-locks banding onto the bent normal (the encoding is
   non-linear, so a per-channel min/max is meaningless for a direction) and fights the EMA. The
   accumulator must EMA the scalar `ao`, and `normalize`/slerp the **decoded** bent normal, not clamp
   the encoded channels.

3. **Indirect diffuse is occluded twice.** In `engine/assets/shaders/lighting.slang`,
   `evalLighting` scales the analytic sky irradiance by the large-range DFAO term (`sky.ao`) **and**
   then multiplies the assembled indirect by `ao`, which already folds in the small-range
   screen-space GTAO (`aoMap`). Stacking a large-range and a small-range occlusion term double-darkens
   interiors. The band split must be decided now: indirect diffuse is occluded by **one** large-range
   term.

Symptom this phase removes: the zebra/moiré on the open roof, and the visibly doubled darkening of
indirect diffuse. (The blobs remain until P2.)

## Work (ordered)

### 1. Fix the cone trace in `sdf.slang`

In `sdfSkyOcclusion`:

- **Replace the 6 equal cones with ~9 solid-angle-weighted hemisphere cones.** Keep one cone along
  `n`, then two cosine-weighted rings around it (an inner ring nearer the normal, an outer ring nearer
  the horizon) for ~9 directions total. Weight each cone's contribution to both `ao` and the bent
  normal by the **solid angle of its hemisphere cap** (a cosine/`sin·dcos` weight per ring), and
  normalize `ao` by the sum of weights instead of dividing by a fixed count. Keep the per-pixel +
  per-frame `rotation` of the tangent frame (it is what lets the new `sdf_ao_accum` average
  decorrelated cone sets). Re-derive the per-ring tilt angles from the cosine-weighted hemisphere so
  the cap weights and the cone half-angle stay consistent (do not hard-depend on the exact "9" — pick
  the ring counts that tile the hemisphere evenly and document the choice; the design note flags the
  9-cone figure as approximate).
- **Replace the voxel-coupled start/step with a world-space self-shadow bias.** Drop `kStart`/
  `kStepFloor`'s `max(…, voxel …)` coupling. Start every cone a fixed **world-space** offset off the
  surface along the cone direction (a small absolute bias, e.g. on the order of a few cm, expressed in
  world units and independent of the field's voxel size), and floor the sphere-march step at a small
  fixed world-space minimum. The bias must be large enough that the first tap clears the shaded
  surface's own reconstructed band on a flat open surface (no self-occlusion → no zebra) and small
  enough not to miss a near occluder. Keep the DFAO penumbra ratio
  `occ = min(occ, saturate(d / (coneHalfAngle * t)))`, the `kMaxDist` reach cap, and the step count.
- Apply the **same world-space bias change** to `sdfReflectionOcclusion` (it shares the
  `kStart`/`kStepFloor` voxel coupling) so the specular reflection-occlusion cone stops self-occluding
  for the same reason. Its cone count is unchanged (specular is a single cone along `R`).

Note: the `voxel` parameter (`globals.sdfOcclusion.z` / the push `params.y`) becomes unused by the
trace once the bias is world-space. Leave the plumbing in place this phase only if it is still read
elsewhere; if nothing reads it after this change, remove the parameter and its push/globals field in
the same change (NO LEGACY — do not leave a dead field). Confirm by grep before deciding.

### 2. New `engine/assets/shaders/sdf_ao_accum.slang`

Create a dedicated temporal accumulator for the SDF-AO signal, replacing the `ssgi_accum.slang` reuse
for the `sdf-ao-accum` pass. Same descriptor *shape* as the TAA/SSGI accumulator (3 samplers:
current, history, motion; 2 storage images: resolved-out, history-out) so it binds the existing
`taa_set_layout` and the existing per-view `sdf_ao_accum_sets` with no new layout. Behaviour:

- **Reproject** previous-frame history through the motion vector (as today).
- **Depth-validated history:** reject history whose carried `viewZ` (the `.a` channel) diverges from
  this pixel's current `viewZ` beyond a relative threshold — the disocclusion test the SSGI
  accumulator already does in alpha. Also reject off-screen reprojection and the first/invalid frame
  (`params.y < 0.5`) → weight 0 (disocclusion fill: take the current sample).
- **EMA the scalar `ao`** (`.r`) with the history weight — no neighborhood clamp on `ao` (a scalar
  AABB clamp is unnecessary once depth validation rejects wrong surfaces, and it is what mis-handles
  the normal).
- **Decode both bent normals, slerp/normalize, re-encode.** Decode current and history `octBentNormal`
  via the `sdf` module's `octDecode`, blend the **decoded directions** (a normalized lerp / slerp by
  the history weight), `normalize`, then `octEncode` the result. Never min/max the encoded `gb`
  channels.
- **Write** `(ao, octBentNormal.x, octBentNormal.y, viewZ)` to both the resolved map (sampled by the
  mesh at set 4 binding 5) and the next-frame history, carrying this frame's `viewZ` in `.a` for next
  frame's reprojection test.

Import the `sdf` module for `octEncode`/`octDecode` (they are `public` there) so the encode lives in
one place. The push can stay the 16-byte `SsgiAccumPush` shape (history weight + valid flag); if the
slerp needs a separate normal-history weight, grow a dedicated `SdfAoAccumPush` and pin its size with
a `const _` assert and the `screen_space_push_sizes_match_slang` test.

### 3. Wire the new accumulator (rendering crate)

- `engine/crates/rendering/src/pipelines.rs`: add `request_sdf_ao_accum`, building
  `shaders/sdf_ao_accum.spv` against `taa_set_layout` with the chosen push size (mirror
  `request_ssgi_accum`). Cache it on a new `sdf_ao_accum: Option<Arc<Pipeline>>` field alongside the
  existing `sdf_ao` field. Do **not** route the SDF-AO path through `request_ssgi_accum` anymore.
- `engine/crates/rendering/src/renderer.rs`: in the frame-graph build where the `sdf-ao-accum` pass is
  recorded (the `if let (Some(accum), Some(motion)) = (&pipelines.ssgi_accum, motion)` block under the
  `sdf-ao` trace), use the new `sdf_ao_accum` pipeline instead of `ssgi_accum`. Build/request it next
  to `request_sdf_ao` (gated on `want_sky_occ`, the same condition). The `ssgi_accum` PSO stays for the
  real SSGI path only.
- `engine/crates/rendering/src/ssao.rs`: if a new `SdfAoAccumPush` is introduced, add it here next to
  `SsgiAccumPush` with its `const _` size assert and extend the push-size test. The SDF-AO history EMA
  weight reuses `SSGI_HISTORY_WEIGHT` unless a different constant is wanted; if it diverges, name a new
  constant rather than overloading the SSGI one.
- The per-view images, sets, layout transitions, and history ping-pong in
  `engine/crates/rendering/src/view_target.rs` are already SDF-AO-specific (`sdf_ao_raw`,
  `sdf_ao_resolved`, `sdf_ao_history[2]`, `sdf_ao_accum_sets[2]`) — they do not change shape. Only the
  pipeline bound to them changes. Verify the `sdf_ao_accum_sets` binding plan still matches the new
  shader's binding indices (current=0, history=1, motion=2, resolved=3, history-out=4).

### 4. Settle the indirect band split in `lighting.slang`

In `evalLighting` (the `globals.counts.z != 0` IBL branch): indirect **diffuse** is occluded by the
**one** large-range DFAO term `sky.ao` (sampled from the resolved `sdfAoMap`), times the material
occlusion texture (`surf.occlusion`, genuine baked albedo-scale detail). Remove the screen-space GTAO
`aoMap` multiply from the indirect-**diffuse** assembly so the large-range DFAO and the contact-range
GTAO are not stacked. Concretely: today `indirectIrr = irradiance * sky.ao`, then
`ambient = indirect * ao + specularIBL` where `ao = surf.occlusion * aoMap`. After this change the
indirect-diffuse occlusion is `sky.ao * surf.occlusion` only; GTAO no longer multiplies indirect
diffuse.

Keep GTAO (`aoMap`) where it is the correct contact-scale term:
- the **specular-AO** path (`specAO` uses `ao` for the roughness-aware specular occlusion) — leave it,
  but recompute `ao` for that term from `surf.occlusion * aoMap` locally so specular keeps GTAO
  contact detail while diffuse does not;
- the **non-IBL fallback** branch (`globals.counts.z == 0`) — unchanged;
- the **SSGI bounce** add and the `aoMap` debug view mode (channel 11) — unchanged.

This is the decision recorded for later phases: large-range occlusion of indirect diffuse is one term
(the DFAO sky factor now; DDGI ray-miss in P5/P6); GTAO is contact-scale only. Document it in a brief
comment at the band-split site (what the code does now, no change-journey note).

## Files

- `engine/assets/shaders/sdf.slang` — `sdfSkyOcclusion` (9 solid-angle-weighted cones, world-space
  self-shadow bias, weight-normalized `ao` + bent normal); `sdfReflectionOcclusion` (world-space bias);
  `octEncode`/`octDecode` reused by the new accumulator. Possibly drop the now-unused `voxel` param.
- `engine/assets/shaders/sdf_ao_accum.slang` — **new**: EMA on `ao`, slerp/normalize on the decoded
  bent normal, depth-validated history, disocclusion fill. Imports `sdf`.
- `engine/assets/shaders/lighting.slang` — `evalLighting` band split: indirect diffuse occluded by
  `sky.ao * surf.occlusion` (one large-range term), GTAO kept for specular-AO + fallback only.
- `engine/crates/rendering/src/pipelines.rs` — `request_sdf_ao_accum` + a `sdf_ao_accum` cache field;
  stop routing SDF-AO through `request_ssgi_accum`.
- `engine/crates/rendering/src/renderer.rs` — bind `sdf_ao_accum` (not `ssgi_accum`) in the
  `sdf-ao-accum` pass; request it next to `request_sdf_ao` under `want_sky_occ`.
- `engine/crates/rendering/src/ssao.rs` — `SdfAoAccumPush` (only if the push grows) + its size assert
  and test entry; SDF-AO EMA weight constant.
- `engine/crates/rendering/src/view_target.rs` — no shape change; verify the `sdf_ao_accum_sets`
  binding indices match the new shader.
- `engine/xtask` shader build picks up the new `sdf_ao_accum.slang` automatically (it globs
  `assets/shaders/*.slang`); confirm a `.spv` is emitted.

## Reuse

- **Extend, do not rewrite, `sdf.slang`'s cone trace** — keep the DFAO penumbra ratio, the tangent
  frame + `rotation`, and the sphere-march; only the cone set, the weighting, and the start/step bias
  change. `octEncode`/`octDecode` are already `public` and stay the single encode home.
- **Model `sdf_ao_accum.slang` on `ssgi_accum.slang`'s structure** (reprojection, the alpha-carried
  `viewZ` depth test, the EMA blend) — it is the right skeleton; the only substantive change is
  replacing the 3×3 color clamp with scalar EMA + decoded-normal slerp.
- **Reuse the existing per-view SDF-AO images, sets, history ping-pong, and `taa_set_layout`** in
  `view_target.rs` — they already exist for this path; nothing in their lifecycle changes.
- **Reuse `SSGI_HISTORY_WEIGHT`** unless a distinct weight is justified.
- The `lighting.slang` DDGI replace-by-coverage and specular-AO machinery is correct — touch only the
  diffuse occlusion band.

## Risks

- **The world-space self-shadow bias is a tuning knob.** Too small → residual self-occlusion zebra on
  the open roof; too large → a near occluder (a column edge) is missed and contact darkening is lost.
  Tune against the open roof (must go clean) and an interior column (must still occlude) on the NVIDIA
  path before declaring it fixed. It must be a world-space absolute, not re-coupled to voxel size.
- **Coarse field still bands.** With the current per-mesh field the *blobs* remain (P2 territory); do
  not chase them here. Verify only that the *zebra/moiré* (the temporal/marginal-start artifact) is
  gone — a stable low-frequency stain is acceptable interim and is P2's job.
- **Decoded-normal slerp cost/seam.** The bent normal sits in the surface upper hemisphere away from
  the octahedral seam, so decode→blend→encode is safe; still confirm no NaNs when history weight is 0
  (disocclusion) and the decoded direction is degenerate (fall back to the geometric `n`).
- **Push-size drift.** If `SdfAoAccumPush` is introduced, a wrong byte size silently corrupts the
  dispatch — the `const _` assert + the `screen_space_push_sizes_match_slang` test must cover it.
- **Validation cleanliness.** The new PSO binds the existing `taa_set_layout`; a binding-index mismatch
  between `sdf_ao_accum.slang` and `view_target.rs`'s plan would trip Vulkan validation — check the
  headless smoke is validation-clean.

## Verification

1. `just engine` builds clean (workspace + shaders; the new `sdf_ao_accum.spv` is emitted).
2. `just prepare-for-commit` — `cargo fmt` clean and `cargo clippy --workspace -- -D warnings` clean
   for this phase's changes; the `ssao.rs` push-size test passes (run it isolated:
   `cargo test -p saffron-rendering screen_space_push_sizes_match_slang`).
3. Headless smoke is validation-clean: `just run-engine-headless` (or
   `SAFFRON_EXIT_AFTER_FRAMES=N ./engine/target/debug/saffron-host` under a headless Weston) exits 0
   with no Vulkan validation errors — confirms the new PSO + set bindings are correct.
4. **Visual on the NVIDIA path** (`just run`, Sponza), the load-bearing check for this phase: the
   fully-open roof shows **no zebra/moiré** and no crawling under camera drag (the marginal-start +
   denoiser fix); interiors are no longer doubly-darkened (the band-split fix). The "GI" debug view
   mode (channel 12) and the SDF-AO factor isolate the indirect/occlusion term for inspection. The
   blobs may persist (expected — P2).
5. Profiler: the `sdf-ao` + `sdf-ao-accum` cost stays in the same ~1.25 ms envelope (this phase is
   cost-neutral by design — 9 cones vs 6 is a small delta; the prepass is not removed until P6).
