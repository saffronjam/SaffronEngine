+++
title = 'Shared types'
weight = 7
+++

# Shared types

Shared types are the data shapes carried over the control wire. They are authored once as Rust DTO
structs in the `saffron-protocol` crate and generated outward to TypeScript, OpenRPC schemas, the
command manifest, and the Lua type defs. The generated artifacts are committed and checked for
freshness, so a command cannot drift silently between the engine, the editor, the `sa` CLI, and the
docs.

## How it works

`saffron-protocol` is the single source of truth. The DTO structs carry `serde` (the runtime
encoding), `schemars` (the OpenRPC fragments), and `ts-rs` (the TypeScript declarations) derives, so
there is no hand-written parser — the derives read the struct at compile time. `cargo run -p xtask --
gen-protocol` assembles the editor-facing artifacts from those derives:

```
engine/crates/protocol/src/                 (DTO source of truth)
        |
        `-- cargo run -p xtask -- gen-protocol
              |-- editor/src/protocol/sa-types.ts
              |-- schemas/control/envelope.schema.json
              |-- schemas/control/openrpc.generated.json
              |-- schemas/control/command-manifest.generated.json
              `-- schemas/control/sa.generated.luau
```

The runtime registers commands with `CommandRegistry::register::<P, R>`. That entry deserializes the
params DTO `P` from the request JSON, runs the typed handler, and serializes the result DTO `R` back.
The same `P`/`R` type names are listed in the static `COMMANDS` table, which the codegen emitters read
to build the OpenRPC `methods` and the manifest. Registry and table are joined by name, with a
`#[test]` asserting the live registry and the manifest carry the same command set.

The freshness gate is a byte-identity test: the xtask re-emits each artifact and asserts it equals the
committed file. A DTO change that is not regenerated fails the test. The generated envelope schema
embeds `ControlFailureDto`, while the generated OpenRPC document contains the params and result
schemas for registered commands.

## Wire invariants

These hold across the whole protocol:

- **IDs are u64, carried as decimal strings.** The `Uuid` wire newtype serializes to a quoted decimal
  JSON string via a `serde_with` adapter, so JavaScript never rounds it through `number`. Reads accept
  a string *or* a number (`PickFirst`) for CLI convenience.
- **camelCase on the wire.** DTO fields are renamed to camelCase, and those are the wire keys and the
  generated TypeScript field names.
- **Field declaration order is the positional order.** `args[i]` fills the `i`-th declared field, and
  `required` in the OpenRPC fragment lists the non-`Option` fields in declaration order.
- **`Transform.rotation` is Euler XYZ radians.** UIs that show degrees convert at the edge.
- **Spot-light angles are degrees.** `innerAngle` and `outerAngle` stay degrees on the wire.
- **Camera uses `near`/`far`.** ECS cameras and the editor fly-camera use the same key names.
- **Failures are closed tagged unions.** Every failure carries `code` and `message`; code
  `diagnostic` also carries a tagged domain payload. Rust clients, the native shell, and the schema
  reject unknown fields, while every boundary rejects string-only errors.
- **A tagged enum carries its discriminant as a field.** `#[serde(tag = "state")]` writes the
  kebab-case variant name under `state`, beside that variant's camelCase fields; only an untagged
  enum serializes as `{"Variant": {…}}`. The `ts-rs` derive needs the tag and both renames spelled
  out again under `#[ts(…)]`, so `every_tagged_union_carries_its_tag_in_typescript` compares the
  schema fragment against the emitted declaration and fails when the TypeScript describes a shape
  no reply carries.

Component bodies and the scene environment come from the Rust scene DTOs. `inspect.components` is
validated as a registry-keyed map of component DTOs, and `set-component.json` is the generated
`ComponentBody` union, so a generic `set-component` write accepts any registry-shaped record.

## In the code

| What | File | Symbols |
|---|---|---|
| DTO source of truth | `engine/crates/protocol/src/` | the params/result DTOs and `ControlFailureDto` |
| Wire id newtype | `engine/crates/protocol/src/uuid.rs` | `Uuid` (decimal-string serde + `JsonSchema`) |
| Static command table | `engine/crates/protocol/src/command/` | `COMMANDS`, `CommandSpec`, `COMMAND_FIXTURES`, `COMMAND_SKIPS` |
| OpenRPC + positional order | `engine/crates/protocol/src/schema.rs` | `fragment_for`, `positional_field_order`, `standalone_schema_for` |
| Codegen surface | `engine/crates/protocol/src/codegen.rs` | `ts_decls`, `schema_fragments` |
| Typed command registration | `engine/crates/control/src/registry.rs` | `CommandRegistry::register`, `fold_positional_args` |
| Generator (xtask) | `engine/xtask/src/protocol/mod.rs` | `emit`, `emit_envelope_schema`, `emit_openrpc`, `emit_manifest` |
| Editor protocol types | `editor/src/protocol/sa-types.ts` | `WireUuid`, `CommandParamsMap`, `CommandResultMap` |
| Contract / freshness tests | `engine/crates/protocol/tests/`, `engine/crates/protocol/src/codegen.rs`, `engine/crates/e2e/tests/contract_u64.rs` | `inventory`, `wire`, `schema_fragments`, `every_tagged_union_carries_its_tag_in_typescript`, the byte-identity tests, `contract_u64` |

## Related

- [sa CLI](../sa-cli-protocol/) — the request/response shape and token coercion these types describe
- [Scene commands](../scene-commands/) — component editing and selection counters
- [Asset commands](../asset-commands/) — project, catalog, and thumbnail commands
- [Control plane](../control-plane-architecture/) — how typed handlers are registered and dispatched
