# Docs, sa scriptability, and the verification gate

**Status:** IMPLEMENTED. Docs: `docs/content/explanations/geometry-and-assets/built-in-primitives.md`
+ the hub `_index.md` row. `sa`: primitives are scriptable via the extended `add-entity` preset
(protocol regenerated — `sa.generated.luau` updated). e2e: `tests/e2e/primitives.test.ts` asserts the
reserved-id `Mesh` + `MaterialSet`, no catalog rows, and save/reload round-trip. The host-independent
gate is green; the e2e / control-schema / present-only smoke are host-dependent and unrunnable in the
dev sandbox (they validate in CI — see the README status note).
**Scope:** `docs/`, `saffron-control`/`sa`, `tests/e2e`
**Depends on:** phase-3..phase-5

## Goal

The built-in-primitive facility is documented, scriptable, and covered by the gate — "done" per
AGENTS.md keep-current rules.

## Touch points

- **`docs/`** — add `docs/content/explanations/geometry-and-assets/built-in-primitives.md`
  (concept + why: native, non-asset, reserved-id; the `What | File | Symbols` table pointing at
  `BuiltinMesh`, `ensure_builtin_meshes`, `geometry::primitives`, the picker chip). Add the hub row in
  `docs/content/explanations/geometry-and-assets/_index.md`. Run the docs `humanizer` pass.
- **`sa` CLI / `saffron-control`** — the extended `AddEntityPreset` flows through `add-entity`
  automatically, so `sa` can spawn primitives from a shell. Confirm the command manifest/openrpc
  regen (`xtask gen-protocol`) reflects the new preset variants and the frozen-manifest contract test
  is updated.
- **`tests/e2e`** — a test that spawns each primitive over the control plane, asserts it renders
  (validation-clean log), serializes as a reserved-id, and adds **no** catalog rows.

## Verification

- `just check` green (workspace build + shaders + control-schema contract + frontend build).
- `just e2e` covers primitive spawn + no-catalog-pollution.
- Docs build (`hugo`) clean; hub row present.
