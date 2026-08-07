# schemas/control

This directory is not the source of truth for command payloads. The control wire contract is
DTO-first:

- source DTOs live in the Rust `saffron-protocol` crate (`engine/crates/protocol/src/`),
  consumed by `saffron-control` to register and dispatch every command;
- `cargo run -p xtask -- gen-protocol` emits the reply-envelope schema, OpenRPC document, command
  manifest, TypeScript declarations, and Luau definitions;
- `tools/check-control-schema/check.ts` validates live command results against the generated
  OpenRPC schemas and compares live `help` with the generated manifest.

## Generated files

`envelope.schema.json`, `openrpc.generated.json`, and `command-manifest.generated.json` are generated.
The reply envelope embeds the generated `ControlFailureDto` schema, including its domain-specific
diagnostic payloads. Command result payloads come from the DTO structs rather than sibling schema
files.

## Editing rules

- Do not add new hand-authored payload schemas here.
- Add or change command DTOs under `engine/crates/protocol/src/`, then run
  `cargo run -p xtask -- gen-protocol`.
- Commit the regenerated envelope, OpenRPC, manifest, TypeScript, and Luau artifacts.
- If a command cannot be exercised by the live contract test, add its skip reason or fixture in the
  generator manifest source, not in the test body.
