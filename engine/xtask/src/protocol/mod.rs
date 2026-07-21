//! `xtask gen-protocol`: the TypeScript, envelope, OpenRPC, manifest, and Luau emitters.
//!
//! The DTO crate (`saffron-protocol`) is the single source of truth: its `ts-rs` derives give
//! the field metadata (via [`saffron_protocol::ts_decls`]) and its `schemars` fragments give the
//! OpenRPC per-DTO schemas (via [`saffron_protocol::schema_fragments`]). This module assembles
//! the five editor-facing artifacts:
//!
//! - `editor/src/protocol/sa-types.ts` — header, the `WireUuid` alias, the complete DTO inventory,
//!   and the `CommandParamsMap`/`CommandResultMap`.
//! - `schemas/control/envelope.schema.json` — the shared success/failure reply envelope.
//! - `schemas/control/openrpc.generated.json` — the `{ openrpc, info, methods, components.schemas
//!   }` envelope, with methods in command-table order and schemas generated from Rust DTOs.
//! - `schemas/control/command-manifest.generated.json` — the fixture/skip ledger.
//! - `schemas/control/sa.generated.luau` — the single Luau defs file: the `sa.*` API surface
//!   ([`luau::emit_api_defs`], from the `saffron-script` binding table) followed by the
//!   `:get_component` component snapshots ([`luau::emit_component_defs`]), generated from one
//!   source (no hand-written `library/sa.lua` overlay; the regen-freshness diff is the drift
//!   guard).
//!
//! Field declaration order is load-bearing (positional-CLI / OpenRPC-`required` order); ts-rs
//! and `serde_json`'s `preserve_order` keep it.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use saffron_protocol::{
    COMMANDS, ControlFailureDto, fixture_for, schema_fragments, skip_for, standalone_schema_for,
    ts_decls,
};
use serde_json::{Map, Value, json};

pub mod luau;
mod ts;

/// The `generatedBy` string the manifest carries.
pub const GENERATED_BY: &str = "cargo run -p xtask -- gen-protocol";

/// The emitted artifacts, relative to the repo root, paired with their content.
pub struct Artifacts {
    /// `editor/src/protocol/sa-types.ts`.
    pub sa_types: String,
    /// `schemas/control/envelope.schema.json`.
    pub envelope_schema: String,
    /// `schemas/control/openrpc.generated.json`.
    pub openrpc: String,
    /// `schemas/control/command-manifest.generated.json`.
    pub manifest: String,
    /// `schemas/control/sa.generated.luau` — the single Luau defs file: the `sa.*` API surface
    /// plus the typed `:get_component` component snapshots, both from the one binding source.
    pub luau_defs: String,
}

/// Assemble the artifacts from the protocol crate (no I/O).
#[must_use]
pub fn emit() -> Artifacts {
    let decls = DtoDecls::load();
    Artifacts {
        sa_types: ts::emit_sa_types(&decls),
        envelope_schema: emit_envelope_schema(),
        openrpc: emit_openrpc(),
        manifest: emit_manifest(),
        luau_defs: luau::emit_defs(),
    }
}

/// Write the artifacts under `repo_root`, returning the list of paths written.
pub fn run(repo_root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let artifacts = emit();
    let targets = [
        (
            repo_root.join("editor/src/protocol/sa-types.ts"),
            artifacts.sa_types,
        ),
        (
            repo_root.join("schemas/control/envelope.schema.json"),
            artifacts.envelope_schema,
        ),
        (
            repo_root.join("schemas/control/openrpc.generated.json"),
            artifacts.openrpc,
        ),
        (
            repo_root.join("schemas/control/command-manifest.generated.json"),
            artifacts.manifest,
        ),
        (
            repo_root.join("schemas/control/sa.generated.luau"),
            artifacts.luau_defs,
        ),
    ];
    let mut written = Vec::new();
    for (path, content) in targets {
        let parent = path
            .parent()
            .with_context(|| format!("artifact path has no parent: {}", path.display()))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
        std::fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

/// The generated control reply envelope. The failure branch is the standalone schemars model for
/// [`ControlFailureDto`]; the open success payload remains unconstrained for command-specific DTOs.
fn emit_envelope_schema() -> String {
    let Value::Object(mut failure) = standalone_schema_for::<ControlFailureDto>() else {
        unreachable!("ControlFailureDto schema is an object")
    };
    let definitions = failure.remove("$defs").unwrap_or_else(|| json!({}));
    failure.remove("$schema");
    failure.remove("title");

    let document = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "envelope.schema.json",
        "$comment": GENERATED_BY,
        "title": "ControlReplyEnvelope",
        "description": "Outer response envelope for every control-protocol reply.",
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": {},
                    "ok": { "const": true },
                    "result": {},
                },
                "required": ["id", "ok", "result"],
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": {},
                    "ok": { "const": false },
                    "error": Value::Object(failure),
                },
                "required": ["id", "ok", "error"],
            },
        ],
        "$defs": definitions,
    });
    pretty(&document)
}

/// The parsed `ts-rs` declarations, indexed by ident, plus the parsed enum-union strings — the
/// field-metadata model the TS emitter walks. `ts-rs` is the parser; this only re-models its
/// output.
pub struct DtoDecls {
    /// DTO identifiers in the canonical Rust inventory order.
    ordered: Vec<String>,
    /// `ident -> parsed declaration` for every DTO type (structs + enums + `Uuid`).
    by_ident: HashMap<String, Decl>,
}

/// One parsed DTO declaration.
enum Decl {
    /// A struct: ordered fields `(name, ts-rs type token)`.
    Struct(Vec<(String, String)>),
    /// An enum / alias: the verbatim right-hand side (e.g. `"a" | "b"` or `string`).
    Alias(String),
}

impl DtoDecls {
    /// Parse every protocol DTO's `ts-rs` `decl()` into the field-metadata model.
    fn load() -> Self {
        let mut ordered = Vec::new();
        let mut by_ident = HashMap::new();
        for (ident, decl) in ts_decls() {
            ordered.push(ident.to_owned());
            by_ident.insert(ident.to_owned(), parse_decl(&decl));
        }
        Self { ordered, by_ident }
    }

    fn get(&self, ident: &str) -> Option<&Decl> {
        self.by_ident.get(ident)
    }
}

/// Parse a `ts-rs` `type X = ...;` declaration into a [`Decl`]. A `{ ... }` object body becomes
/// ordered struct fields; `Record<string, never>` is an empty struct; anything else (an enum
/// union or the `string` alias) is kept verbatim as an [`Decl::Alias`].
fn parse_decl(decl: &str) -> Decl {
    let rhs = decl
        .split_once('=')
        .map(|(_, r)| r.trim().trim_end_matches(';').trim())
        .unwrap_or(decl);
    if rhs == "Record<string, never>" {
        return Decl::Struct(Vec::new());
    }
    if !has_top_level_union(rhs)
        && let Some(inner) = rhs.strip_prefix('{').and_then(|s| s.strip_suffix('}'))
    {
        return Decl::Struct(parse_fields(inner));
    }
    Decl::Alias(rhs.to_owned())
}

/// Whether a declaration contains a union separator outside every object/array/generic group.
/// Tagged enums from ts-rs have the shape `{ kind: "a", ... } | { kind: "b", ... }`; treating
/// that as one object would emit an invalid TypeScript interface.
fn has_top_level_union(value: &str) -> bool {
    let mut depth = 0_i32;
    let mut quoted = false;
    let mut escaped = false;
    for ch in value.chars() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => quoted = true,
            '{' | '[' | '(' | '<' => depth += 1,
            '}' | ']' | ')' | '>' => depth -= 1,
            '|' if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

/// Split a `ts-rs` object body into ordered `(field, type)` pairs, dropping the `/** ... */`
/// doc-comment blocks ts-rs interleaves (the committed wire TS carries no field docs) and
/// honoring `<>`/`{}`/`[]` nesting when splitting on the top-level commas.
fn parse_fields(inner: &str) -> Vec<(String, String)> {
    let without_docs = strip_doc_comments(inner).replace('\n', " ");
    let mut fields = Vec::new();
    let mut depth = 0_i32;
    let mut current = String::new();
    for ch in without_docs.chars() {
        match ch {
            '<' | '{' | '[' => depth += 1,
            '>' | '}' | ']' => depth -= 1,
            _ => {}
        }
        if ch == ',' && depth == 0 {
            push_field(&mut fields, &current);
            current.clear();
        } else {
            current.push(ch);
        }
    }
    push_field(&mut fields, &current);
    fields
}

fn push_field(fields: &mut Vec<(String, String)>, raw: &str) {
    let field = raw.trim();
    if field.is_empty() {
        return;
    }
    if let Some((name, ty)) = field.split_once(':') {
        fields.push((name.trim().to_owned(), ty.trim().to_owned()));
    }
}

/// Remove every `/** ... */` block from a `ts-rs` body.
fn strip_doc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("/**") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        if let Some(end) = after.find("*/") {
            rest = &after[end + 2..];
        } else {
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// The OpenRPC document.
fn emit_openrpc() -> String {
    let mut schemas: Map<String, Value> = Map::new();

    let mut named: Vec<(String, Value)> = schema_fragments()
        .into_iter()
        .map(|(name, frag)| (name.to_owned(), frag))
        .collect();
    named.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, frag) in named {
        schemas.insert(name, frag);
    }

    let methods: Vec<Value> = COMMANDS
        .iter()
        .map(|cmd| {
            json!({
                "name": cmd.name,
                "summary": cmd.summary,
                "params": [{
                    "name": "params",
                    "schema": { "$ref": format!("#/components/schemas/{}", cmd.params) },
                }],
                "result": {
                    "name": "result",
                    "schema": { "$ref": format!("#/components/schemas/{}", cmd.result) },
                },
            })
        })
        .collect();

    let doc = json!({
        "openrpc": "1.3.2",
        "info": { "title": "Saffron Anima control DTOs", "version": "0.2.0" },
        "methods": methods,
        "components": { "schemas": Value::Object(schemas) },
    });
    pretty(&doc)
}

/// The command manifest. Each command carries exactly one of a fixture or a skip; neither is a
/// build error.
fn emit_manifest() -> String {
    let commands: Vec<Value> = COMMANDS
        .iter()
        .map(|cmd| {
            let mut entry = Map::new();
            entry.insert("name".into(), json!(cmd.name));
            entry.insert("params".into(), json!(cmd.params));
            entry.insert("result".into(), json!(cmd.result));
            entry.insert("status".into(), json!("typed"));
            match (fixture_for(cmd.name), skip_for(cmd.name)) {
                (Some(fixture), _) => {
                    entry.insert("fixture".into(), json!(fixture));
                }
                (None, Some(skip)) => {
                    entry.insert("skip".into(), json!(skip));
                }
                (None, None) => unreachable!(
                    "command '{}' has neither a fixture nor a skip (the table invariant a \
                     protocol test enforces)",
                    cmd.name
                ),
            }
            Value::Object(entry)
        })
        .collect();

    let doc = json!({
        "generatedBy": GENERATED_BY,
        "commands": commands,
        "skips": [{ "name": "help", "reason": "reflective registry" }],
    });
    pretty(&doc)
}

/// 2-space indent, a trailing newline, raw non-ASCII (the em-dash in command summaries stays a
/// literal UTF-8 byte).
fn pretty(value: &Value) -> String {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    serde::Serialize::serialize(value, &mut ser).expect("json value serializes");
    let mut text = String::from_utf8(buf).expect("serde_json emits utf-8");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> std::path::PathBuf {
        // `engine/xtask/` -> repo root is three parents up.
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .expect("xtask manifest dir resolves to the repo root")
    }

    #[test]
    fn sa_types_is_byte_identical_to_committed() {
        let committed =
            std::fs::read_to_string(repo_root().join("editor/src/protocol/sa-types.ts"))
                .expect("committed sa-types.ts");
        assert_eq!(emit().sa_types, committed);
    }

    #[test]
    fn envelope_schema_is_byte_identical_to_committed() {
        let committed =
            std::fs::read_to_string(repo_root().join("schemas/control/envelope.schema.json"))
                .expect("committed envelope.schema.json");
        assert_eq!(emit().envelope_schema, committed);
    }

    #[test]
    fn luau_defs_is_byte_identical_to_committed() {
        let committed =
            std::fs::read_to_string(repo_root().join("schemas/control/sa.generated.luau"))
                .expect("committed sa.generated.luau");
        assert_eq!(emit().luau_defs, committed);
    }

    /// The `sa.*` defs are generated from one source, so a hand-written `library/sa.lua` overlay,
    /// a components-only `.luau` artifact, and a `check-script-defs` drift tripwire must not exist
    /// anywhere in the tree.
    #[test]
    fn no_legacy_overlay_or_tripwire() {
        let root = repo_root();
        for absent in [
            "tools/check-script-defs",
            "schemas/control/sa-components.generated.luau",
            "editor/library/sa.lua",
        ] {
            assert!(
                !root.join(absent).exists(),
                "legacy artifact must not exist in the Rust tree: {absent}"
            );
        }
        // A generated `.luau` def file is the only Lua-type artifact: no `sa.lua` overlay is
        // committed under these trees.
        for tree in [
            "engine/crates",
            "engine/xtask",
            "editor/src",
            "schemas",
            "tools",
        ] {
            for entry in walk(&root.join(tree)) {
                let name = entry.file_name().and_then(|n| n.to_str()).unwrap_or("");
                assert_ne!(
                    name,
                    "sa.lua",
                    "a hand-written sa.lua overlay must not exist: {}",
                    entry.display()
                );
            }
        }
    }

    /// A small recursive file walk (no external crate), skipping `node_modules`/`target`.
    fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(read) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "node_modules" || name == "target" {
                continue;
            }
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    #[test]
    fn openrpc_is_byte_identical_to_committed() {
        let committed =
            std::fs::read_to_string(repo_root().join("schemas/control/openrpc.generated.json"))
                .expect("committed openrpc.generated.json");
        assert_eq!(emit().openrpc, committed);
    }

    #[test]
    fn manifest_is_byte_identical_to_committed() {
        let committed = std::fs::read_to_string(
            repo_root().join("schemas/control/command-manifest.generated.json"),
        )
        .expect("committed command-manifest.generated.json");
        assert_eq!(emit().manifest, committed);
    }
}
