# saffron-protocol — the wire DTOs

The single source of truth for the control-plane wire types, shared by the engine, the `sa` CLI, and
the protocol codegen (`serde` / `schemars` / `ts-rs` derives). There is no parser: the struct *is*
the model and `derive` reads it at compile time, so no hand-written serialization exists.

It depends only on `saffron-core`, which is what lets the `sa` CLI link the DTOs without linking the
engine. Keep it that way.

Vegetation is the largest domain: about a third of the generated TypeScript bindings are vegetation
types.

## Layout

| File | Owns |
|---|---|
| `dto/` | The general command DTOs, one file per domain (`render`, `asset`, `scene`, `physics`, …) plus `coerce.rs`, the bool-coercing readers every params bool field uses |
| `scene_dto.rs` | Scene, entity, and component DTOs |
| `vegetation_dto/` | Every vegetation, plant, biome, and ecology DTO, one file per domain |
| `control_dto.rs` | Envelope, failure, and diagnostic payloads |
| `command/` | `table.rs` (the manifest table), `metadata.rs` (`COMMAND_FIXTURES`, `COMMAND_SKIPS`), `dto_names.rs` (`DTO_TYPE_NAMES`), and `CommandSpec` in `mod.rs` |
| `codegen.rs` | The single ordered DTO universe (`dto_universe!`) that drives both the emitted TypeScript declarations and the OpenRPC fragments |
| `schema.rs`, `uuid.rs` | Schema helpers and the wire uuid type |

Both DTO directories are directory modules: `mod.rs` declares the submodules and glob-re-exports
them, so the crate's public surface is flat and a DTO's file is an organisational choice only.

Generated artifacts land in `bindings/` (TypeScript), `schemas/control/` (OpenRPC + manifest), and
`editor/src/protocol/sa-types.ts`. Regenerate with `cargo run -p xtask -- gen-protocol` and commit
the result; never hand-edit any of them.

## Rules that are easy to break

- **Field declaration order is wire-significant.** It is the positional-CLI-argument order and the
  OpenRPC `required` order. Reordering fields in a struct is a wire change even though nothing in
  Rust notices.
- **ts-rs does not honour `#[serde(default)]`.** An optional Rust field generates as a *required*
  TypeScript field. The editor then either fabricates a value it should have omitted or fails to
  typecheck against a payload that is legal on the wire. `cargo build` cannot see this — check the
  generated `.ts` when you add a defaulted field.
- **A tagged enum mirrors its tag into `#[ts(…)]`.** `ts-rs` reads `#[serde(tag = "kind")]` only
  while the rest of that attribute is a form it understands — `deny_unknown_fields` alongside it
  makes it drop the tag and emit an *externally* tagged union that no engine reply ever matches, and
  `no-serde-warnings` means the build says nothing. So spell the tag and both renames out again:
  `#[ts(export, tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]`.
  `every_tagged_union_carries_its_tag_in_typescript` in `codegen.rs` compares the `schemars`
  fragment against the emitted declaration and fails when one is missing.
- **A new command touches six places.** All of them, in one change:
  1. a `CommandSpec` in `command/table.rs`, positioned to match where it is registered in
     `saffron-control` (see that crate's `AGENTS.md` — registration order is manifest order);
  2. its result type in `DTO_TYPE_NAMES`;
  3. an entry in `COMMAND_FIXTURES`, or in `COMMAND_SKIPS` with a stated reason;
  4. an entry in `codegen.rs`'s `dto_universe!` list (nested DTOs need only this, not
     `DTO_TYPE_NAMES`);
  5. a row in the control-command reference under `docs/`, which carries a count header and so has a
     completeness contract;
  6. `cargo run -p xtask -- gen-protocol`, with the regenerated artifacts committed.
- **A command that the live contract test cannot exercise declares why.** Put the skip reason in
  `COMMAND_SKIPS` here, not as a special case in the test body.
- **Optional fields are omitted, never null.** Serialize with `skip_serializing_if`; a `null` on the
  wire is a different value from an absent key for several editor code paths.
- **Entity ids and `PlantId` are strings on the wire.** They are u64 and u128 in the engine; a
  numeric JSON type loses precision in the browser.
