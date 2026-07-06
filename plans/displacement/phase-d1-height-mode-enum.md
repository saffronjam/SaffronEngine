# Phase D1 — one Height Map + a `HeightMode` enum (Bump / Parallax / Displacement)

**Status:** IMPLEMENTED (engine + editor compile; `clippy -D warnings` clean on the touched crates;
material serde/golden + `HeightMode` round-trip + control lib tests green — full GPU e2e deferred to
the planned test rehaul). The `displacement: bool` is gone: a `saffron_core::HeightMode` enum
(`bump`/`parallax`/`displacement`, default Bump) lives beside `BlendMode` and flows through
`MaterialAsset` → `SubmeshMaterial` → the feature-bit resolve. Bump got a new `FEATURE_HEIGHT_BUMP`
(64) shader path (height-gradient shading normal, flat silhouette, shared with the DISPLACE branch in
`mesh.slang`); Parallax keeps `FEATURE_HEIGHT`, Displacement keeps `FEATURE_DISPLACE`. The `.smat`
wire field is `heightMode` (golden reseeded, both inline + on-disk fixtures updated). `material-get` /
`material-update` carry `heightMode` + `heightScale`, and the Material editor shows a **Height mode**
dropdown (+ height scale) when a Height Map is assigned. `docs/.../native-materials.md` updated. The
global `set-displacement` renderer toggle is untouched (orthogonal master switch).
**Scope:** `saffron-assets` (material model + `.smat` JSON + golden), `saffron-rendering`
(`instancing.rs` feature bits, `lighting.slang`/`mesh.slang`), `saffron-protocol` (material DTO →
`gen-protocol`), `saffron-control` (material commands), `editor` (material inspector UI)
**Depends on:** the geometry-engine track (A/B) — it introduced the `displacement: bool` +
`FEATURE_DISPLACE`/`FEATURE_HEIGHT` bits this phase generalizes. Feeds **D2** (import routing) and
pairs with **D3** (POM quality).

## Goal

Replace the opaque per-material `displacement: bool` (`material.rs:80`) with a three-way
**`HeightMode` enum — `Bump` / `Parallax` / `Displacement`** — over a single **Height Map** slot. This
is the pattern the major editor engines converge on (a **2026 cross-engine survey**, see the README):
Unity HDRP's `Displacement Mode` (None / Vertex / Pixel-POM / Tessellation), Godot's Height feature
(offset parallax / Deep-Parallax POM), Blender's `Bump Only` / `Displacement Only` /
`Displacement and Bump`. One grayscale map; the **mode**, not the asset, picks the technique.

Today the bool only distinguishes Parallax (`false` → `FEATURE_HEIGHT` POM) from Displacement (`true`
→ `FEATURE_DISPLACE`), it has **no editor UI** (only `heightScale` is exposed —
`fieldRenderer.tsx:102`, `InspectorPanel.tsx:90`), and there is **no bump-only mode** — the safe,
artifact-free baseline every surveyed engine offers. The enum fixes all three.

## The three modes

| Mode | Technique | Feature bit | Silhouette | Cost | When |
|---|---|---|---|---|---|
| **Bump** | height→normal gradient only (no parallax, no geometry) | new `FEATURE_HEIGHT_BUMP` | flat | ~free | safe default; far-field / low-poly degrade; Blender `Bump Only`, Unity `None`-shading |
| **Parallax** | 24-step POM UV march (existing `parallaxUv`) | `FEATURE_HEIGHT` | flat | medium (fragment) | fake depth on near-perpendicular surfaces (floors/walls) |
| **Displacement** | real per-vertex displacement (B2 `displace` compute pre-pass) | `FEATURE_DISPLACE` | **true** | high (needs tessellated geometry + BLAS refit, C1) | hero surfaces, library Displacement maps |

## Approach

1. **Material model** (`material.rs`): delete `displacement: bool`; add `height_mode: HeightMode`
   (a `serde`-tagged enum, kebab string on the wire — `"bump"`/`"parallax"`/`"displacement"`). Choose
   the fresh-material default = **`Bump`** (the artifact-free baseline; a bare material with no height
   map is unaffected either way). Keep `height_scale` (its *meaning* is mode-dependent — parallax
   march depth vs world-space displacement amplitude; note this, resolved per open-question #2 in the
   geometry README).
2. **Serialization** (`material_asset_to_json` `:209` / `material_asset_from_json` `:253`): emit/parse
   `heightMode` in place of `displacement`. Regenerate the golden (`fixtures/golden/material.smat`) and
   fix the golden unit test (`material.rs` ~`:755`) — NO-COMPAT: change the serialized form outright,
   no dual read.
3. **Render material** (`render_material.rs:110`): carry `height_mode` onto `SubmeshMaterial`
   (`draw_list.rs`), replacing its `displacement` bool + `defaults()` (`:96`).
4. **Feature-bit selection** (`instancing.rs:1028`): map the mode → bit —
   `Bump → FEATURE_HEIGHT_BUMP`, `Parallax → FEATURE_HEIGHT`, `Displacement → FEATURE_DISPLACE`. Add
   `FEATURE_HEIGHT_BUMP` to the constants (`instancing.rs`, `thumbnail_render.rs`) and to
   `lighting.slang` (`:504`+).
5. **Shaders** (`mesh.slang`): add the **Bump** path — apply the height-gradient bump normal (the same
   finite-difference derivation as the `FEATURE_DISPLACE` branch at `:63`) but **skip both** `parallaxUv`
   (`:29`) **and** vertex displacement, so height reads as shaded relief with a flat silhouette. Parallax
   and Displacement branches are unchanged (Displacement's vertex move is B2's compute pre-pass).
6. **Protocol + editor**: the material DTO (`dto.rs`) carries `displacement: bool` today
   (`openrpc.generated.json:8008`); replace it with `heightMode` and run `bun run gen:protocol`. Add a
   **Height Mode** dropdown to the material inspector (Bump/Parallax/Displacement), shown only when a
   Height Map is assigned; drive it through the typed client + `material-update` command.
7. **Global toggle stays orthogonal**: `set-displacement {0|1}` (`commands_render.rs:901`) is the
   renderer-wide compute-pre-pass master switch (debug/perf), **not** the per-material mode. Leave it;
   note the relationship in its help text.

## Verification

- A `.smat` round-trips its `heightMode`; each mode selects the expected feature bit
  (unit-test the `instancing` mapping).
- Bump mode renders height as shaded relief with **no UV swimming and no silhouette change**; Parallax
  matches today's POM; Displacement matches B2 on the dense sphere.
- `just engine` + `cargo test -p saffron-assets -p saffron-control` + `bun run check` (regenerated
  protocol) green; golden reseeded intentionally.

## Risks

- **Wire change ripples to the editor** (unlike the import-clutter fix, which reused DTOs) — the typed
  client + inspector must update in lockstep with `gen-protocol`.
- **Bump needs a new shader path** — small, but a new feature bit means a PSO-cache variant.
- **`height_scale` unit ambiguity across modes** — document it; a value good for POM depth is not the
  same as a world-space displacement amplitude (D2 sets a sane amplitude on import).
