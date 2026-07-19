//! The `sa-types.ts` emitter over the `ts-rs` decls.

use std::collections::HashSet;

use super::{Decl, DtoDecls, command_type_names};

/// Build `editor/src/protocol/sa-types.ts` from the Rust DTO declarations.
pub fn emit_sa_types(decls: &DtoDecls) -> String {
    let names = interface_order(decls);
    let interfaces = names
        .iter()
        .map(|name| emit_declaration(decls, name))
        .collect::<Vec<_>>()
        .join("\n\n");

    let params_map = super::COMMANDS
        .iter()
        .map(|cmd| format!("  {:?}: {};", cmd.name, cmd.params))
        .collect::<Vec<_>>()
        .join("\n");
    let result_map = super::COMMANDS
        .iter()
        .map(|cmd| format!("  {:?}: {};", cmd.name, cmd.result))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "/**\n * GENERATED - do not edit.\n *\n * Produced by cargo run -p xtask -- \
         gen-protocol.\n */\n\nexport type WireUuid = string;\n\n{}\n\nexport \
         interface CommandParamsMap {{\n{}\n}}\n\nexport interface CommandResultMap \
         {{\n{}\n}}\n",
        interfaces, params_map, result_map,
    )
}

/// The declaration emission order, rooted at every command and shared wire helper.
fn interface_order(decls: &DtoDecls) -> Vec<String> {
    let mut roots = command_type_names();
    roots.extend(["Vec3", "Vec4", "ProbeRef", "ComponentBody"]);

    let mut seen = HashSet::new();
    let mut order = Vec::new();
    seen.insert("EntityRef".to_owned());
    order.push("EntityRef".to_owned());
    for root in roots {
        for dep in declaration_deps(decls, root) {
            if seen.insert(dep.clone()) {
                order.push(dep);
            }
        }
    }
    order
}

/// A DFS pre-order of the emitted declarations reachable from `ty`.
fn declaration_deps(decls: &DtoDecls, ty: &str) -> Vec<String> {
    let mut out = Vec::new();
    let inner = unwrap_array(strip_nullable(ty));
    if let Some(Decl::Struct(fields)) = decls.get(inner) {
        out.push(inner.to_owned());
        for (_, field_ty) in fields {
            out.extend(declaration_deps(decls, field_ty));
        }
    } else if inner == "ComponentBody" && matches!(decls.get(inner), Some(Decl::Alias(_))) {
        out.push(inner.to_owned());
    }
    out
}

/// Emit one Rust-derived TypeScript interface or named union.
fn emit_declaration(decls: &DtoDecls, name: &str) -> String {
    match decls.get(name) {
        Some(Decl::Struct(fields)) => {
            let body = fields
                .iter()
                .map(|(field, ty)| {
                    let (mapped, optional) = ts_type(ty);
                    format!("  {field}{}: {mapped};", if optional { "?" } else { "" })
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("export interface {name} {{\n{body}\n}}")
        }
        Some(Decl::Alias(rhs)) => format!("export type {name} = {};", map_alias_rhs(rhs)),
        None => panic!("interface-order type {name} has no declaration"),
    }
}

/// Map a `ts-rs` type token to its TS spelling, returning `(type, optional)`. `T | null` is
/// optional; `Array<T>` -> `T[]`; `bigint` -> `number`; `Uuid` -> `WireUuid`; `JsonValue` -> a
/// `unknown`; a Rust alias inlines its union; structs and primitives pass through.
fn ts_type(ty: &str) -> (String, bool) {
    let (core, optional) = match ty.strip_suffix("| null") {
        Some(inner) => (inner.trim(), true),
        None => (ty.trim(), false),
    };

    if let Some(item) = core
        .strip_prefix("Array<")
        .and_then(|s| s.strip_suffix('>'))
    {
        let (mapped, _) = ts_type(item);
        return (format!("{mapped}[]"), optional);
    }
    match core {
        "bigint" => ("number".to_owned(), optional),
        "Uuid" => ("WireUuid".to_owned(), optional),
        "JsonValue" => ("unknown".to_owned(), optional),
        other => (resolve_alias_or_passthrough(other), optional),
    }
}

/// Inline Rust-derived primitive and string-literal unions.
fn resolve_alias_or_passthrough(name: &str) -> String {
    if name != "ComponentBody"
        && let Some(union) = TYPE_ALIASES.with(|aliases| aliases.get(name).cloned())
    {
        return union;
    }
    name.to_owned()
}

thread_local! {
    static TYPE_ALIASES: std::collections::HashMap<String, String> = type_aliases();
}

fn type_aliases() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for (ident, decl) in saffron_protocol::ts_decls() {
        if let Decl::Alias(rhs) = super::parse_decl(&decl) {
            map.insert(ident.to_owned(), map_alias_rhs(&rhs));
        }
    }
    map
}

fn map_alias_rhs(rhs: &str) -> String {
    rhs.replace("bigint", "number").replace("Uuid", "WireUuid")
}

/// `T | null` -> `T`; a bare `T` passes through (the nullable marker the TS walk strips before
/// resolving a dependency).
fn strip_nullable(ty: &str) -> &str {
    ty.strip_suffix("| null").map_or(ty, str::trim)
}

/// `Array<T>` -> `T` (the element type the dependency walk recurses into); a bare `T` passes
/// through.
fn unwrap_array(ty: &str) -> &str {
    ty.strip_prefix("Array<")
        .and_then(|s| s.strip_suffix('>'))
        .map_or(ty, str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_struct_emits_blank_body() {
        let decls = DtoDecls::load();
        // `PingParams`/`EmptyParams` are `Record<string, never>` -> `{\n\n}`.
        assert_eq!(
            emit_declaration(&decls, "PingParams"),
            "export interface PingParams {\n\n}"
        );
    }

    #[test]
    fn bigint_field_maps_to_number() {
        let (mapped, optional) = ts_type("bigint");
        assert_eq!(mapped, "number");
        assert!(!optional);
    }

    #[test]
    fn selector_alias_maps_to_union() {
        let mapped = resolve_alias_or_passthrough("EntitySelector");
        assert!(mapped.contains("string"));
        assert!(mapped.contains("number"));
    }

    #[test]
    fn opaque_json_field_maps_to_unknown() {
        let (mapped, _) = ts_type("JsonValue");
        assert_eq!(mapped, "unknown");
    }

    #[test]
    fn nullable_field_is_optional() {
        let (mapped, optional) = ts_type("Vec3 | null");
        assert_eq!(mapped, "Vec3");
        assert!(optional);
    }

    #[test]
    fn enum_field_inlines_union() {
        let (mapped, optional) = ts_type("AaModeDto");
        assert_eq!(
            mapped,
            "\"off\" | \"fxaa\" | \"taa\" | \"msaa2\" | \"msaa4\" | \"msaa8\""
        );
        assert!(!optional);
    }
}
