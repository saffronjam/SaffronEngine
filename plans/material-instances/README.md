# Material instances (entity materials reference `.smat` assets + sparse overrides)

**Status:** NOT STARTED

Collapse the two parallel material worlds into one. Today an entity carries its PBR parameters as an
**inline blob** (`Material` / `MaterialSet` components) that is forked from — and drifts forever apart
from — the `.smat` **material assets** authored in the graph/material editor. This plan moves the
entity-facing model onto the industry-standard shape: **a submesh-indexed slot that _references_ a
`.smat` material asset plus an optional _sparse_ per-object override map**, with the `.smat` owning the
one parameter schema. Along the way it fixes the inspector bugs that make the current component
unusable.

## Why

There are three mutually-exclusive material components today
(`engine/crates/scene/src/component.rs`):

1. `Material` — one inline PBR blob for the whole mesh.
2. `MaterialSet { slots: Vec<MaterialSlot> }` — a per-submesh inline PBR blob (byte-identical fields to
   `Material`), indexed by each submesh's `material_slot`.
3. `MaterialAsset { material: Uuid }` — the **only** component that references a `.smat` by id.

`AssetServer::resolve_entity_materials` (`engine/crates/assets/src/render_material.rs`) applies strict
precedence `MaterialAsset → MaterialSet → Material`: a `.smat` reference, when present, *shadows* the
inline data entirely. There is **no** code path that resolves a `MaterialSet` slot to a `.smat` — the
inline slots and the `.smat` assets never merge.

The split is born at import. `AssetServer::bake_model` (`engine/crates/assets/src/import.rs`) writes
each glTF/OBJ material as a `.smat`-shaped chunk in the `.smodel` container and records an
`AssetType::Material` catalog row — but `apply_imported_materials` / `instantiate_model`
(`engine/crates/assets/src/spawn.rs`) reads those back and **copies the params inline** into a
`MaterialSet`, keeping **no** back-reference to the source material. So every imported model uses inline
slots that are disconnected from the `.smat` chunks the material editor edits. Editing a slot changes
only that one entity; editing a `.smat` reaches only entities that carry a `MaterialAsset` component —
which imports never attach (the `material-assign` command that would attach one has no UI caller).

This is the anti-pattern every mature engine avoids (Unity `sharedMaterials[]` + `MaterialPropertyBlock`;
Unreal Material → Material Instance → MID; Godot per-surface Material Override): **loose per-entity PBR
values divorced from any material asset.** The correct model, and the destination of this plan, is:

> **shader/graph → `.smat` material asset (owns the exposed-parameter schema + defaults) → optional
> material instance (`parent` + sparse overrides) → `MaterialSet` slot that _references_ a material
> asset + an optional sparse per-object override map, indexed by submesh.**

The `.smat` asset already carries the bones: `saffron_assets::material::MaterialAsset`
(`engine/crates/assets/src/material.rs`) already has the full PBR factor set, a `graph`, a `parent`
id, and a sparse `overrides` map — plus the asset-side instance commands
(`material-create-instance`, `material-set-override`) already exist. What is missing is (a) a declared
*exposed-parameter schema*, and (b) an entity model that references + sparsely overrides an asset
instead of copying it.

### The inspector bugs (why the component feels broken today)

One root cause underlies most of it: the inspector renders all ~19 slot/material fields, but the write
command `set-material` (`SetMaterialParams`, `engine/crates/protocol/src/dto.rs`) accepts only **8**
(`baseColor, albedoTexture, metallicRoughnessTexture, metallic, roughness, emissive, emissiveStrength,
unlit`) and has **no `#[serde(deny_unknown_fields)]`**, so every other key is *silently discarded* by
serde. The handler still bumps `scene_version`, the reconcile poll re-reads the unchanged value, and the
optimistic edit is clobbered with no error. This hits **both** paths:

- **Standalone `Material`:** `applyWrite` (`editor/src/panels/InspectorPanel.tsx`) routes non-texture
  `Material` fields through `client.setMaterial` → `heightScale`, `normalStrength`, `alphaCutoff`,
  `doubleSided`, `blend` all revert. (Its four extra textures happen to survive via `setComponentField`;
  `albedoTexture`/`metallicRoughnessTexture` via `assignAsset`.)
- **`MaterialSet` slots:** `onSlotFieldChange` → `slotCoalescerFor` → `client.setMaterial(…, slotIndex)`
  routes **every** field, including all textures, through the same 8-field DTO → the same reverts, plus
  the four extra textures (`normal`/`occlusion`/`emissive`/`height`) are dropped too.

## Goal

- **One material data model on the entity.** A `MaterialSet` slot holds `{ material: Uuid, overrides }`,
  submesh-indexed. Inline PBR fields no longer live on the entity. The three material components
  collapse toward this single reference+override shape.
- **One parameter schema.** The `.smat` asset declares its exposed parameters (name, type, default); the
  params-buffer layout, the instance/slot override validation, and the inspector all derive from that one
  list. No second hand-maintained field list anywhere.
- **Imports reference, not copy.** A model import attaches slots that reference the baked `.smat`
  material assets by id; editing the `.smat` propagates to every instance.
- **A usable inspector.** Editing any material parameter is smooth (optimistic-local, coalesced send)
  and persists; the inspector distinguishes "tweak this object" (writes a sparse override) from "edit the
  material" (opens the graph), Unreal-instance style.
- **No parallel paths.** `set-material` is deleted; all material writes go through the generic
  `set-component` (full-DTO) write everyone else uses.

## Phases (dependency-ordered)

| Phase | File | Summary | Depends on |
|-------|------|---------|------------|
| 1 | `phase-1-inspector-material-writes.md` | Unify all inspector material writes on `set-component`; delete `set-material` + `SetMaterialParams`; fix the reverting/dropped fields on both `Material` and `MaterialSet`; add the `blend` field hint and delete the dead `alphaClip` hint; fix the `component_schemas` "Material" schema (`doubleSided`/`blend`). **Independently shippable correctness fix on today's model** — the user is hitting these bugs now. | — |
| 2 | `phase-2-smat-exposed-parameter-schema.md` | Give the `.smat` `MaterialAsset` a declared exposed-parameter schema (name/type/default), derived from the graph (or the fixed übershader param set); reconcile the packed `orm_texture` vs the component's separate `metallic_roughness_texture` + `occlusion_texture`; derive the params-buffer layout and the `parent`/`overrides` validation from the schema. | — (can start in parallel with 1) |
| 3 | `phase-3-slot-reference-and-overrides.md` | Redefine the entity material model: `MaterialSlot` becomes `{ material: Uuid, overrides: SparseMap }`; **delete** the inline PBR fields from `Material`/`MaterialSlot`; collapse the three components toward the reference model; rebuild `resolve_entity_materials` to resolve `asset defaults → instance overrides → slot overrides` into one params buffer. | 2 |
| 4 | `phase-4-import-attaches-references.md` | Change `bake_model` to emit standalone `.smat` assets and `apply_imported_materials`/`instantiate_model` to attach slots that **reference** them by id instead of copying params inline; ship a built-in default `.smat` for the material-less case. | 3 |
| 5 | `phase-5-inspector-override-editor.md` | Rework the `MaterialSet`/material inspector into an override editor: each exposed parameter shows an "overridden" marker; editing writes into `slot.overrides`; an "Edit material" button opens the `.smat` graph; wire a first-class material-link picker (replacing the orphaned `material-assign`). Delete every remaining inline-param write path. | 3, 4 |

The chain: Phase 1 stands alone (fixes today's UI). Phase 2 establishes the schema Phase 3 resolves
against. Phase 3 is the model cutover. Phase 4 makes imports produce the new shape. Phase 5 is the UI for
the new shape. Phases 3–5 replace the inline world outright — no inline path survives alongside the
reference model (NO-LEGACY).

## Grounding (key current-code entry points)

| What | File | Symbols |
|------|------|---------|
| The three entity material components (inline + reference) | `engine/crates/scene/src/component.rs` | `Material`, `MaterialSlot`, `MaterialSet`, `MaterialAsset` |
| Component (de)serialization + registry wiring | `engine/crates/scene/src/serde.rs`, `registry.rs` | `material_slot_to_json`, `material_slot_from_json` |
| The blend axis enum | `engine/crates/core/src/blend.rs` | `BlendMode`, `BlendMode::from_wire` |
| The `.smat` asset (already has factors, graph, parent, overrides) | `engine/crates/assets/src/material.rs` | `MaterialAsset`, `default_material_asset`, `load_material_asset_raw` |
| Component-precedence resolution to GPU material | `engine/crates/assets/src/render_material.rs` | `resolve_entity_materials`, `build_submesh_material`, `lower_slot`, `resolve_material_asset` |
| GPU-side material struct | `engine/crates/rendering/src/draw_list.rs` | `SubmeshMaterial` |
| Model import bakes materials into the `.smodel` container | `engine/crates/assets/src/import.rs` | `AssetServer::bake_model` |
| Spawn copies baked materials **inline** (the fork point) | `engine/crates/assets/src/spawn.rs` | `apply_imported_materials`, `instantiate_model` |
| The field-limited material write command (to delete) | `engine/crates/control/src/commands_scene.rs` | `set-material` handler |
| Its DTO (8 fields, no `deny_unknown_fields`) | `engine/crates/protocol/src/dto.rs` | `SetMaterialParams` |
| Asset-side material + instance/override commands | `engine/crates/control/src/commands_asset.rs` | `material-create`, `material-assign`, `material-create-instance`, `material-set-override`, `material-set-graph`, `assign-asset`, `ensure_material` |
| The component schema with `additionalProperties:false` | `engine/crates/protocol/src/schema.rs` | `component_schemas` "Material" / "MaterialSet" |
| Inspector write routing (generic + slot paths) | `editor/src/panels/InspectorPanel.tsx` | `applyWrite`, `coalescerFor`, `onFieldChange`, `onSlotFieldChange`, `slotCoalescerFor`, `recordSlotEdit` |
| Field hints (the dead `alphaClip`, the missing `blend`) | `editor/src/components/fieldRenderer.tsx` | `FIELD_HINTS`, `renderField`, `resolveHint` |
| The typed control wrappers | `editor/src/control/client.ts` | `setMaterial`, `setComponent`, `setComponentField`, `assignAsset`, `materialAssign` |
| The non-addable component list (why MaterialSet has no "Add") | `editor/src/lib/componentOrder.ts` | `COMPONENT_ORDER`, `NON_ADDABLE`, `canonicalComponentNames` |
| The material graph model shared with the wire | `editor/src/materials/graph.ts`; `editor/src/panels/MaterialGraphEditor.tsx`, `MaterialEditorPanel.tsx` | React Flow model → Slang codegen |

## Ground rules

- **One write path.** After Phase 1 there is exactly one command that writes material parameters:
  `set-component` (full DTO, read-modify-write). `set-material` / `SetMaterialParams` are deleted, not
  left dormant. `assign-asset` survives only for the drag-an-asset-onto-an-entity *auto-attach* flow, not
  for inspector field edits.
- **One parameter schema.** From Phase 2 on, the `.smat` exposed-parameter list is the single source of
  truth for what a material parameter *is*. The params buffer, override validation, and inspector all
  derive from it — no field is hand-listed in a second place (that duplication is what caused the
  reverting-field bug).
- **The cutover deletes the old world.** Phase 3 removes the inline PBR fields from `Material` /
  `MaterialSlot` in the same change that introduces the reference+override shape; Phase 4 removes the
  inline-copy import path in the same change that adds reference-attach. No "additive for now" — a
  superseded path is deleted with its replacement (NO-LEGACY). Migration of existing project files is out
  of scope (fresh-start codebase).
- **Overrides are sparse and typed.** A slot/instance override map holds only the parameters that differ
  from the referenced material, keyed by the exposed-parameter name and validated against its type. Never
  a full copy of the parameter set.
- **Each phase ends green.** `just engine` + `just prepare-for-commit` (format + clippy `-D warnings`),
  `bun run check` in `editor/` where a wire type changed (regenerate `@saffron/protocol` via
  `xtask gen-protocol` — never hand-edit `sa-types.ts`), `just e2e`, and the `docs/` material page +
  its hub row updated when the concept changes.

Detail lives in the phase files; this page is the index.
