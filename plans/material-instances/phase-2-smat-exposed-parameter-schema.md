# Phase 2 — Exposed-parameter schema on the `.smat` material asset

**Status:** COMPLETED

**As built (scope refinement):** the engine-side schema foundation landed here — the
`ExposedParam` / `ExposedParamKind` schema (`engine/crates/assets/src/material_schema.rs`,
`pbr_exposed_parameters()` + `exposed_parameter()`), `apply_overrides` widened to cover the full
exposed set (`material.rs`, with a drift-guard test that fails if the schema and `apply_overrides`
diverge), and override validation in `material-set-override` (`commands_asset.rs` — unknown key or
mistyped value now rejects with a typed error). The **`material-schema` wire command and the material
docs-page rewrite are folded into Phase 5**, where the inspector's override editor consumes them —
adding the command with its consumer avoids an unconsumed wire surface and one docs churn. The ORM
decision is settled here (the exposed schema's texture params are `albedoTexture`, `ormTexture`,
`normalTexture`, `emissiveTexture`, `heightTexture` — the packed `ormTexture` is canonical); the
entity slot adopts that representation in Phase 3. The params-buffer layout stays a direct field
mapping in `build_submesh_material` (the übershader's uniform layout is fixed), with the exposed schema
as the single source of truth for overrides/validation/inspector rather than the GPU struct.

Part of `plans/material-instances/`. Establish the **one parameter schema** that Phases 3–5 resolve
against: give the `.smat` `MaterialAsset` a declared list of *exposed parameters* (name, type, default),
and make the params-buffer layout and the instance/slot override validation derive from that one list
instead of a hand-maintained field set. This phase can proceed in parallel with Phase 1; it is a
prerequisite for Phase 3.

## Why

The `.smat` asset (`saffron_assets::material::MaterialAsset`, `engine/crates/assets/src/material.rs`)
already stores the full PBR factor set (`base_color`, `metallic`, `roughness`, `emissive`,
`emissive_strength`, `normal_strength`, `alpha_cutoff`, `height_scale`, `uv_tiling`, `uv_offset`),
texture ids (`albedo_texture`, `orm_texture`, `normal_texture`, `emissive_texture`, `height_texture`),
flags (`unlit`, `double_sided`, `blend`), a `graph`, a `parent` id, and a sparse `overrides` map. But
the set of parameters a material *exposes* is implicit — spread across that struct and the übershader.
An instance's `overrides` map (`material-set-override {material, field, value}`) is validated only
loosely, and the entity model in Phase 3 needs a typed schema to validate slot overrides against and to
render an override editor from (Phase 5).

There is also a real **texture-representation divergence** to resolve here: the inline component
(`Material` / `MaterialSlot`, `engine/crates/scene/src/component.rs`) stores
`metallic_roughness_texture` **and** `occlusion_texture` as two separate ids, while the `.smat` asset
stores a single packed `orm_texture` (AO=R, roughness=G, metallic=B — see the module docs in
`render_material.rs`). When the entity slot becomes a reference to the asset (Phase 3), the two must
agree on one representation.

## Goal

- A declared `exposed parameters` list on the material asset: each entry is `{ name, type, default }`
  where `type` is one of a small closed set (`scalar`, `color3`, `color4`, `vec2`, `bool`, `enum(blend)`,
  `texture`). For a fixed-übershader material the list is the standard PBR set; for a graph-authored
  material the list is derived from the graph's exposed input nodes.
- The params-buffer layout (`SubmeshMaterial` / the GPU params struct in
  `engine/crates/rendering/src/draw_list.rs`) derives from the exposed-parameter list — one place defines
  a parameter's identity, type, and default.
- `material-set-override` / `material-create-instance` validate each override key against the exposed list
  and reject unknown or mistyped keys with a typed error.
- **Resolve the ORM divergence:** standardize on the asset's packed `orm_texture` as the canonical
  representation (it is what the GPU path and the übershader already consume). The exposed-parameter list
  carries a single `orm_texture` (or a named `metallicRoughness`+`occlusion` pair only if the übershader
  genuinely samples them separately — verify in `render_material.rs`/the Slang shaders which it is, and
  pick the one the shader actually uses). Document the decision in the material docs page.

## Changes (sketch — detail during implementation)

**`engine/crates/assets/src/material.rs`.** Add the exposed-parameter descriptor type and a function that
yields a material's exposed list: for a graph material, from the `graph` (the editor-shaped JSON's
exposed inputs — see `editor/src/materials/graph.ts` for the node model); for a non-graph material, the
fixed PBR set. `default_material_asset` supplies the defaults. Keep the concrete factor fields as the
*storage* of the master material's defaults, but treat the exposed list as the authoritative schema for
overrides.

**`engine/crates/assets/src/render_material.rs`.** Make `build_submesh_material` populate the GPU
`SubmeshMaterial` by walking the exposed-parameter list + resolved values, rather than field-by-field, so
adding/removing an exposed parameter is a one-place change. Fold the instance resolution
(`resolve_material_asset` already applies `parent` + `overrides`) into a single
`defaults → parent chain → overrides` reducer keyed by exposed-parameter name.

**`engine/crates/control/src/commands_asset.rs`.** In `material-set-override` /
`material-create-instance`, validate the override field name + value type against the parent's exposed
list; add a `material-schema {id}` (or extend `material-get`) read command so the editor can fetch the
exposed list for the override editor (Phase 5) and the `sa` CLI can inspect it.

**Protocol + docs.** New/edited DTOs for the exposed-parameter list and the `material-schema` read;
regenerate `@saffron/protocol`. Update the material docs page under `docs/content/` describing the
exposed-parameter schema and the ORM decision, plus its hub `_index.md` row.

## Verification

- `just engine` + `just prepare-for-commit` clean; `bun run check`; `just e2e`.
- Golden/snapshot: extend `engine/crates/assets/tests/smat_golden_snapshot.rs` to cover the exposed list
  + a validated override; assert an unknown override key is rejected.
- A `sa material-get` / `sa material-schema` shows the exposed parameters for both a fixed-übershader
  material and a graph-authored one.
