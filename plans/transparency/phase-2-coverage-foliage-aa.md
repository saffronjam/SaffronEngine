# Phase 2 — Coverage-based anti-aliasing for masked foliage

**Status:** IN PROGRESS — alpha-to-coverage (the default-config path) COMPLETED; two refinements
remain, each gated on a prerequisite the engine doesn't yet carry.

**As built.** `gpu_types::Material` gained a `masked` flag (resolved per submesh from
`BlendMode::Masked` in the draw-list batcher, alongside the `blend` routing — see Phase 1's
per-submesh note; a masked submesh of an otherwise-opaque mesh gets its own A2C batch).
`PsoKey` gained `alpha_to_coverage`, derived as `masked && sample_count > 1` — so a masked material
at 1× shares the opaque PSO and only masked-under-MSAA mints a distinct pipeline.
`build_mesh_pipeline_with_module` sets `alpha_to_coverage_enable` on that permutation and passes a
new fragment spec constant (`constant_id 1`, `kAlphaToCoverage`); `mesh.slang` `fragmentMain` then
rescales the cutout alpha around the cutoff by its screen-space derivative
(`saturate((opacity - cutoff)/max(fwidth(opacity),1e-4) + 0.5)`) and returns it as `SV_Target.a`
per-sample coverage, instead of the hard `discard`. This is automatic under the default `msaa4` AA
mode (no toggle — automatic-under-MSAA is the modern default, e.g. UE's foliage default, and it is
more correct than a manual switch since it derives from the existing `aa` setting). Verified
validation-clean on the GPU (a masked two-material entity records the A2C PSO under `msaa4` with no
Vulkan error, alongside a translucent entity).

**Remaining (each blocked on a missing prerequisite, so deferred rather than half-built):**

- **Hashed/dithered alpha for the TAA-without-MSAA config.** A2C only exists under MSAA; a
  `taa`-only (1×) pipeline would still hard-cut. Hashed alpha resolves *only* under TAA and would be a
  **rendering bug if shipped half-way**, so it is a coherent follow-up, not a quick add. Three
  prerequisites, none of them present yet:
  - *A per-frame seed in the fragment.* `LightGlobals` carries no frame index (`extra_flags.zw` are
    reserved but unpopulated); a monotonic counter (`frame_history.rs` `frame_counter`) must be
    folded into `extra_flags.z` so the stochastic threshold varies per frame for TAA to integrate.
  - *TAA-active state in the PSO key.* `PsoKey` only knows `sample_count`; `Pipelines` would need a
    `taa` flag (mirroring `set_sample_count`) so the key derives `masked && sample_count == 1 && taa`.
  - *The depth-prepass masking contract.* The prepass (`depthPrepassFragment`) does a **deterministic
    hard discard and binds no light set** on purpose. A stochastic main-pass test would disagree with
    it, writing depth where no color follows — holes. Hashed alpha requires either running the
    identical hashed test in the prepass (binding `globals` there for the seed) or excluding
    hashed-masked materials from the prepass. This is the load-bearing design work.
  Non-blocking: the default AA is `msaa4`, which A2C already covers; the prepass defaults off.
- **Preserve-coverage mipmaps on masked-texture import.** Box-downsampled alpha thins masked foliage
  at distance. The correct fix (Castaño coverage-preserving mip rescale) is *per-texture*, but the
  uploader has no per-texture "this alpha is a coverage mask" flag — a texture can back both masked
  and opaque materials. Needs a texture-import coverage flag before it can be done correctly.

Part of `plans/transparency/`. Depends on Phase 1 (the `BlendMode` enum and the blend axis on `PsoKey`).
This phase does **not** touch the translucent pass; it improves the **masked** (`BlendMode::Masked`)
path — the modern foliage/leaf/hair technique that AAA engines use instead of blending. It is what the
`CompareAlphaCoverage` "fur" model exists to demonstrate: masked coverage looks better than blend for
dense intersecting geometry, without sorting.

## Why

Alpha-tested masking today is a hard 1-bit `discard` (`FEATURE_ALPHACLIP` in `mesh.slang` `fragmentMain`
vs `pbr.w`), so masked edges alias badly and thin foliage pops in/out with distance (the classic
mip-fade problem). The two standard fixes, both order-independent and sort-free:

- **Alpha-to-coverage (A2C)** under MSAA — hardware turns the shader's alpha into a subset of MSAA
  coverage samples, giving soft edges while the fragment stays opaque and depth-sortable. UE ships this
  on-by-default for foliage.
- **Hashed / dithered alpha** — a per-fragment stochastic threshold resolved by **TAA** into smooth
  coverage. The default in TAA pipelines; also fixes the distance-fade problem.

This engine already has MSAA and TAA (`engine/crates/rendering/src/aa.rs`, `engine/assets/shaders/taa.slang`),
so both are available substrates.

## Goal

Masked materials render with anti-aliased coverage: A2C when MSAA is active, hashed alpha (TAA-resolved)
otherwise (and for the 1× path). Edges on `fur_mask` and the `AlphaBlendModeTest` Cutoff panels are
smooth, and thin geometry stays stable with distance. One coherent policy, selected by render settings —
not two competing masked paths left side by side.

## NO-LEGACY checklist

- The masked path has exactly one coverage policy per configuration (A2C under MSAA, hashed at 1×), not a
  legacy hard-`discard` kept alongside. The plain `discard` becomes the hashed-threshold discard; A2C is
  a pipeline-state add on the same masked draw.
- Any masked-texture mipmap handling is applied at import once, not toggled per draw.

## 1 — Alpha-to-coverage on the masked PSO (MSAA)

**File `engine/crates/rendering/src/pipelines.rs`.** In `build_mesh_pipeline_with_module`, when the key
is masked **and** `sample_count > 1`, set `alpha_to_coverage_enable(true)` on the
`PipelineMultisampleStateCreateInfo`. This needs a masked discriminant on the PSO — extend `PsoKey` so
masked is distinguishable (Phase 1 added `blend: bool`; add `masked: bool` here, or a small
`enum BlendPso { Opaque, Masked, Blend }` replacing both flags for one clean axis). A2C only exists on
the masked+MSAA pipeline; opaque and translucent are unaffected.

## 2 — Edge-sharpen + hashed alpha in the übershader

**File `engine/assets/shaders/mesh.slang`** (`fragmentMain`, masked branch), shared helpers in
`engine/assets/shaders/lighting.slang`.

- **A2C edge sharpen.** Raw alpha into A2C bands; rescale around the cutoff by the screen-space
  derivative so edges stay crisp instead of muddy:
  `alpha = saturate((alpha - cutoff) / max(fwidth(alpha), 1e-4) + 0.5);` then output that alpha (A2C
  reads `SV_Target.a`). Gate on a masked+MSAA feature/spec-constant so the opaque/blend paths skip it.
- **Hashed alpha (1× / TAA path).** Replace the hard `discard` with a stochastic threshold: hash the
  screen-space (or object-space) position + the frame's TAA jitter index into a `[0,1)` threshold and
  `discard` when `alpha < threshold`. Object-space hashing (Wyman/McGuire) keeps the noise stable under
  camera motion; the TAA jitter varies it per frame so TAA integrates it to smooth coverage. Pull the
  jitter seed from the same per-frame data TAA already uses.
- Select A2C-sharpen vs hashed via a render-settings flag / spec constant (see §3) so there is one
  branch, not both firing.

The depth-prepass masked branch (`depthPrepassFragment`) must use the **same** test as the color pass so
prepass depth matches — mirror whichever policy is active (hashed prepass discard, or A2C in the prepass
if the prepass is MSAA).

## 3 — Render-settings toggle

**File `engine/crates/rendering/src/render_settings.rs`** (+ the control command that mutates settings).
Add a coverage-AA mode (`A2C` under MSAA / `Hashed` / off) so the technique is inspectable and
driveable, and add a matching `sa` control command per the "Keep current" rule (a feature adding engine
state worth driving gets one control registration). Thread it into the PSO key + the shader spec
constant.

## 4 — Preserve-coverage mipmaps for masked textures

**File `engine/crates/assets/src/` texture import** (the mip generation path). Alpha-tested coverage
shrinks as box-filtered mips lower the average alpha, so distant foliage thins/vanishes. Apply Castaño's
"preserve coverage" mip rescale (rescale each mip's alpha so the fraction above the cutoff matches mip 0)
when a texture backs a masked material. This is an import-time, one-shot fix on the alpha channel of
masked base-color textures — no runtime cost.

## Milestone gate

`just engine` + `just prepare-for-commit` clean. Headless-boot `CompareAlphaCoverage` and
`AlphaBlendModeTest`: `fur_mask` and the Cutoff panels show anti-aliased edges; thin fur stays stable as
the camera pulls back (no distance pop). Validation-clean log. Extend the docs alpha-modes page with the
coverage-AA section (A2C vs hashed, when each applies) and the hub `_index.md` row.
