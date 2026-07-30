//! The `sa-types.ts` emitter over the `ts-rs` decls.

use super::{Decl, DtoDecls};

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

/// The declaration emission order, matching the complete Rust DTO inventory.
fn interface_order(decls: &DtoDecls) -> Vec<String> {
    decls
        .ordered
        .iter()
        .filter(|name| name.as_str() != "Uuid")
        .cloned()
        .collect()
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

/// Maps a `ts-rs` type token to its TS spelling, returning `(type, optional)`. `T | null` is the
/// optional form; `bigint` narrows to `number` and `Uuid` to the `WireUuid` alias.
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
        other => (other.to_owned(), optional),
    }
}

fn map_alias_rhs(rhs: &str) -> String {
    rhs.replace("bigint", "number")
        .replace("Uuid", "WireUuid")
        .replace("JsonValue", "unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_struct_emits_blank_body() {
        let decls = DtoDecls::load();
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
        let decls = DtoDecls::load();
        assert_eq!(
            emit_declaration(&decls, "EntitySelector"),
            "export type EntitySelector = number | string;"
        );
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
    fn enum_field_uses_its_named_declaration() {
        let (mapped, optional) = ts_type("AaModeDto");
        assert_eq!(mapped, "AaModeDto");
        assert!(!optional);
    }

    #[test]
    fn tagged_enum_field_uses_its_named_declaration() {
        let (mapped, optional) = ts_type("EnvironmentProfileRefDto");
        assert_eq!(mapped, "EnvironmentProfileRefDto");
        assert!(!optional);
    }

    #[test]
    fn tagged_enum_references_emit_named_payload_declarations() {
        let decls = DtoDecls::load();
        let output = emit_sa_types(&decls);
        assert!(output.contains("export interface ThinSheetFoliageParametersDto"));
        assert!(output.contains("export interface PlantAssetSummaryDto"));
        assert!(output.contains("export interface BiomeAssetSummaryDto"));
        assert!(output.contains("export interface VegetationMapSummaryDto"));
    }

    #[test]
    fn complete_inventory_emits_opaque_vegetation_contracts() {
        let output = emit_sa_types(&DtoDecls::load());
        assert!(output.contains("export type PlantId = string;"));
        assert!(output.contains("export interface PlantPointDto"));
        assert!(output.contains("export interface ProvenanceDto"));
        assert!(output.contains("export interface VegetationBaseManifestDto"));
        assert!(output.contains("export type VegetationMutationDto ="));
    }

    #[test]
    fn tagged_enum_preserves_named_opaque_string_aliases() {
        let decls = DtoDecls::load();
        let mapped = emit_declaration(&decls, "VegetationLayerOperatorDto");
        assert!(mapped.contains("PlantId"));
        assert!(mapped.contains("VegetationGuid"));
        assert!(!mapped.contains("Wirestring"));
    }
}
