//! The `wire-type -> Luau` mapper and the `.luau` defs emitters: [`emit_api_defs`] over the
//! [`saffron_script::BINDINGS`] table, [`emit_component_defs`] over the component wire shapes,
//! and [`emit_defs`] joining the two.

use std::collections::BTreeSet;
use std::collections::HashMap;

use saffron_script::{BINDINGS, Binding, BindingKind};

use super::{Decl, parse_decl};

/// The serialized built-in component names in registry order.
pub const REGISTERED: &[&str] = saffron_protocol::COMPONENT_NAMES;

/// One parsed interface field: the wire name, its wire-type token, and whether it is optional.
#[derive(Debug, Clone)]
pub struct Field {
    /// The wire (camelCase) field name.
    pub name: String,
    /// The Rust-generated wire-type token.
    pub ty: String,
    /// `true` when the TS field carries the `?` optional marker.
    pub optional: bool,
}

/// Maps a wire-type token to its Luau annotation. Ids cross as decimal strings, so `WireUuid`
/// becomes `string`; an unrecognized token is a nested interface, `sa.<Name>`.
#[must_use]
pub fn map_type(ty: &str) -> String {
    match ty {
        "number" | "boolean" | "string" => ty.to_owned(),
        "WireUuid" => "string".to_owned(),
        "Vec3" => "{ x: number, y: number, z: number }".to_owned(),
        "Vec4" => "{ x: number, y: number, z: number, w: number }".to_owned(),
        "Record<string, unknown>" => "table<string, any>".to_owned(),
        _ => {
            if let Some(inner) = ty.strip_suffix("[]") {
                return format!("{}[]", map_type(inner));
            }
            if ty.contains('|') {
                return ty.chars().filter(|c| !c.is_whitespace()).collect();
            }
            format!("sa.{ty}")
        }
    }
}

/// The interface a field type references, or `None` for primitives, vectors, unions, and
/// generics. The `---@class` set is the transitive closure of this over the registered roots.
fn referenced(ty: &str) -> Option<&str> {
    let base = ty.trim_end_matches("[]");
    match base {
        "number" | "boolean" | "string" | "WireUuid" | "Vec3" | "Vec4" => None,
        _ if base.contains('|') || base.contains('<') => None,
        _ => Some(base),
    }
}

/// Read the component and nested DTO interfaces from the Rust declarations.
fn interfaces() -> HashMap<String, Vec<Field>> {
    let declarations = saffron_protocol::ts_decls();
    let aliases: HashMap<String, String> = declarations
        .iter()
        .filter_map(|(name, declaration)| match parse_decl(declaration) {
            Decl::Alias(rhs) => Some(((*name).to_owned(), rhs)),
            Decl::Struct(_) => None,
        })
        .collect();
    let mut out = HashMap::new();
    for (name, declaration) in declarations {
        let Decl::Struct(fields) = parse_decl(&declaration) else {
            continue;
        };
        let fields = fields
            .into_iter()
            .map(|(name, ty)| {
                let (ty, optional) = ty
                    .strip_suffix("| null")
                    .map_or((ty.as_str(), false), |inner| (inner.trim(), true));
                Field {
                    name: name.trim_end_matches('?').to_owned(),
                    ty: canonical_type(ty, &aliases),
                    optional: optional || name.ends_with('?'),
                }
            })
            .collect();
        out.insert(name.to_owned(), fields);
    }
    out
}

fn canonical_type(ty: &str, aliases: &HashMap<String, String>) -> String {
    if let Some(item) = ty
        .strip_prefix("Array<")
        .and_then(|value| value.strip_suffix('>'))
    {
        return format!("{}[]", canonical_type(item, aliases));
    }
    match ty {
        "bigint" => "number".to_owned(),
        "Uuid" => "WireUuid".to_owned(),
        "JsonValue" => "Record<string, unknown>".to_owned(),
        other => aliases
            .get(other)
            .filter(|alias| alias.starts_with('"'))
            .cloned()
            .unwrap_or_else(|| other.to_owned()),
    }
}

/// The interface names reachable from [`REGISTERED`] through field references: the `---@class`
/// set.
fn reachable(interfaces: &HashMap<String, Vec<Field>>) -> BTreeSet<String> {
    let mut reach = BTreeSet::new();
    let mut queue: Vec<String> = REGISTERED.iter().map(|s| (*s).to_owned()).collect();
    while let Some(name) = queue.pop() {
        if reach.contains(&name) {
            continue;
        }
        let Some(fields) = interfaces.get(&name) else {
            continue;
        };
        reach.insert(name);
        for field in fields {
            if let Some(reference) = referenced(&field.ty)
                && interfaces.contains_key(reference)
                && !reach.contains(reference)
            {
                queue.push(reference.to_owned());
            }
        }
    }
    reach
}

/// Emits the component-snapshot defs: a name-sorted `---@class sa.<Component>` block per
/// reachable interface, the registered components' `---@overload` lines, and the
/// `Entity:get_component` stub. Byte-stable across re-runs.
#[must_use]
pub fn emit_component_defs() -> String {
    let interfaces = interfaces();
    let reach = reachable(&interfaces);

    let classes = reach
        .iter()
        .map(|name| {
            let body = interfaces[name]
                .iter()
                .map(|field| {
                    format!(
                        "---@field {} {}{}",
                        field.name,
                        map_type(&field.ty),
                        if field.optional { "?" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("---@class sa.{name}\n{body}").trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut overload_names: Vec<&str> = REGISTERED
        .iter()
        .copied()
        .filter(|name| reach.contains(*name))
        .collect();
    overload_names.sort_unstable();
    let overloads = overload_names
        .iter()
        .map(|name| format!("---@overload fun(self: sa.Entity, name: {name:?}): sa.{name}?"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "-- Typed component snapshots. get_component(name) returns the component as a read-only \
         table in\n-- its serialized wire shape (vectors as {{x,y,z}} tables, ids as decimal \
         strings); nil when absent.\n{classes}\n\n{overloads}\nfunction Entity:get_component(name) \
         end  ---@param name sa.ComponentName @return table?\n"
    )
}

/// Maps a binding-table type token to its Luau annotation. The API surface references the
/// value/handle classes by name (`sa.Vec3`, `sa.Entity`), where [`map_type`] expands `Vec3` to the
/// inline snapshot shape; primitives defer to [`map_type`] so the mapping has one owner.
fn map_api_type(ty: &str) -> String {
    match ty {
        "number" | "boolean" | "string" => map_type(ty),
        "table" | "any" => ty.to_owned(),
        _ => {
            if let Some(inner) = ty.strip_suffix("[]") {
                return format!("{}[]", map_api_type(inner));
            }
            format!("sa.{ty}")
        }
    }
}

/// The `---@param`/`@return` tail for one binding's stub, empty when it takes and returns
/// nothing.
fn doc_tail(binding: &Binding) -> String {
    let mut parts = Vec::new();
    for arg in binding.args {
        parts.push(format!("@param {} {}", arg.name, map_api_type(arg.ty)));
    }
    if let Some(ret) = binding.ret {
        parts.push(format!("@return {}", map_api_type(ret)));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ---{}", parts.join(" "))
    }
}

/// One stub, `function <owner><sep><name>(<args>) end` plus its doc tail.
fn stub(owner: &str, sep: &str, binding: &Binding) -> String {
    let params = binding
        .args
        .iter()
        .map(|arg| arg.name)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "function {owner}{sep}{}({params}) end{}",
        binding.name,
        doc_tail(binding)
    )
}

/// The `sa.Vec3` value-class block: `---@field`s, the arithmetic `---@operator` overloads, and
/// the method stubs. `__eq`/`__tostring` are not LuaLS operators, so they carry no annotation, and
/// scripts construct through `sa.vec3(...)` rather than the static `new`.
fn emit_vec3_class() -> String {
    let vec3 = |kind: BindingKind| {
        BINDINGS
            .iter()
            .filter(move |b| b.class == Some("Vec3") && b.kind == kind)
    };

    let mut lines = vec!["---@class sa.Vec3".to_owned()];
    for field in vec3(BindingKind::Field) {
        lines.push(format!(
            "---@field {} {}",
            field.name,
            map_api_type(field.ret.unwrap_or("any"))
        ));
    }
    for meta in vec3(BindingKind::Meta) {
        let op = match meta.name {
            "__add" => "add",
            "__sub" => "sub",
            "__mul" => "mul",
            "__unm" => "unm",
            _ => continue,
        };
        let ret = map_api_type(meta.ret.unwrap_or("any"));
        if meta.args.is_empty() {
            lines.push(format!("---@operator {op}: {ret}"));
        } else {
            let arg = map_api_type(meta.args[0].ty);
            lines.push(format!("---@operator {op}({arg}): {ret}"));
        }
    }
    lines.push("local Vec3 = {}".to_owned());
    for method in vec3(BindingKind::Method) {
        lines.push(stub("Vec3", ":", method));
    }
    lines.join("\n")
}

/// The `sa.Entity` handle block: one stub per `Entity` method binding, minus `get_component`,
/// which the component-snapshot tail declares with its typed per-component overloads.
fn emit_entity_class() -> String {
    let mut lines = vec![
        "---@class sa.Entity".to_owned(),
        "local Entity = {}".to_owned(),
    ];
    for method in BINDINGS
        .iter()
        .filter(|b| b.class == Some("Entity") && b.kind == BindingKind::Method)
        .filter(|b| b.name != "get_component")
    {
        lines.push(stub("Entity", ":", method));
    }
    lines.join("\n")
}

/// The `sa` namespace table: one stub per `Free` binding.
fn emit_namespace() -> String {
    let mut lines = vec!["sa = {}".to_owned()];
    for free in BINDINGS
        .iter()
        .filter(|b| b.class.is_none() && b.kind == BindingKind::Free)
    {
        lines.push(stub("sa", ".", free));
    }
    lines.join("\n")
}

/// The `sa.ComponentName` alias: the union of every [`REGISTERED`] name. `get_component` and
/// `has_component` accept all of them; the structural ones are rejected at runtime by
/// `set/add/remove_component`.
fn emit_component_name_alias() -> String {
    let union = REGISTERED
        .iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join("|");
    format!("---@alias sa.ComponentName {union}")
}

/// Emits the `sa.*` API defs from the [`BINDINGS`] table the runtime VM registers from.
/// Byte-stable across re-runs.
///
/// `RayHit`, `RagdollState`, and `ScriptSelf` are synthetic: only their return token appears in
/// the binding table, so their field and handler lists are spelled out here and must match the
/// `ScriptRayHit`/`ScriptRagdollState` PODs and the lifecycle handlers.
#[must_use]
pub fn emit_api_defs() -> String {
    let header = "---@meta\n-- Saffron Anima Lua API. Generated from the saffron-script binding \
                  table; do not edit by hand.\n-- Types only: the real bindings are the mlua \
                  registration walk over the same table.";

    let ray_hit = "---@class sa.RayHit\n---@field hit boolean\n---@field distance number\n---@field \
                   point sa.Vec3\n---@field normal sa.Vec3\n---@field entity sa.Entity?";

    let plant_hit = "---@class sa.PlantHit\n---@field hit boolean\n---@field plant                      string\n---@field position sa.Vec3\n---@field distance number\n---@field                      lifecycle string\n---@field health number\n---@field interaction_policy                      string";

    let ragdoll_state = "---@class sa.RagdollState\n---@field present boolean\n---@field active \
                         boolean\n---@field body_weight number\n---@field bones integer";

    let script_self = "---@class sa.ScriptSelf\n---@field entity sa.Entity\nlocal ScriptSelf = \
                       {}\nfunction ScriptSelf:on_create() end\nfunction ScriptSelf:on_update(dt) \
                       end ---@param dt number\nfunction ScriptSelf:on_destroy() end\nfunction \
                       ScriptSelf:on_trigger_enter(other) end ---@param other sa.Entity\nfunction \
                       ScriptSelf:on_trigger_exit(other) end ---@param other sa.Entity\nfunction \
                       ScriptSelf:on_contact(other, point, normal) end ---@param other sa.Entity \
                       @param point sa.Vec3 @param normal sa.Vec3";

    [
        header.to_owned(),
        emit_vec3_class(),
        ray_hit.to_owned(),
        plant_hit.to_owned(),
        ragdoll_state.to_owned(),
        emit_component_name_alias(),
        emit_entity_class(),
        script_self.to_owned(),
        emit_namespace(),
    ]
    .join("\n\n")
        + "\n"
}

/// The `.luau` defs file written into every project's `library/`: [`emit_api_defs`] followed by
/// [`emit_component_defs`].
#[must_use]
pub fn emit_defs() -> String {
    format!("{}\n{}", emit_api_defs(), emit_component_defs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_type_passes_through_primitives() {
        assert_eq!(map_type("number"), "number");
        assert_eq!(map_type("boolean"), "boolean");
        assert_eq!(map_type("string"), "string");
    }

    #[test]
    fn map_type_wire_uuid_is_string() {
        assert_eq!(map_type("WireUuid"), "string");
    }

    #[test]
    fn map_type_vectors_expand_to_xyz_tables() {
        assert_eq!(map_type("Vec3"), "{ x: number, y: number, z: number }");
        assert_eq!(
            map_type("Vec4"),
            "{ x: number, y: number, z: number, w: number }"
        );
    }

    #[test]
    fn map_type_nested_dto_prefixes_sa() {
        assert_eq!(map_type("PhysicsMaterial"), "sa.PhysicsMaterial");
        assert_eq!(map_type("BVec3"), "sa.BVec3");
    }

    #[test]
    fn map_type_array_recurses() {
        assert_eq!(map_type("WireUuid[]"), "string[]");
        assert_eq!(map_type("number[]"), "number[]");
        assert_eq!(map_type("number[][]"), "number[][]");
        assert_eq!(map_type("MaterialSlot[]"), "sa.MaterialSlot[]");
    }

    #[test]
    fn map_type_union_strips_whitespace() {
        assert_eq!(
            map_type("\"static\" | \"kinematic\" | \"dynamic\""),
            "\"static\"|\"kinematic\"|\"dynamic\""
        );
    }

    #[test]
    fn map_type_record_is_table_any() {
        assert_eq!(map_type("Record<string, unknown>"), "table<string, any>");
    }

    #[test]
    fn every_registered_component_is_reachable_and_classed() {
        let interfaces = interfaces();
        let reach = reachable(&interfaces);
        for name in REGISTERED {
            assert!(
                reach.contains(*name),
                "registered component {name} has no reachable wire shape"
            );
        }
        let defs = emit_component_defs();
        for name in REGISTERED {
            assert!(
                defs.contains(&format!("---@class sa.{name}\n"))
                    || defs.contains(&format!("---@class sa.{name}")),
                "missing ---@class block for {name}"
            );
            assert!(
                defs.contains(&format!(
                    "---@overload fun(self: sa.Entity, name: {name:?}): sa.{name}?"
                )),
                "missing ---@overload for {name}"
            );
        }
    }

    #[test]
    fn nested_dtos_are_pulled_in_unrelated_are_not() {
        let reach = reachable(&interfaces());
        for nested in ["BVec3", "PhysicsMaterial", "FootChainDto", "BonePhysicsDto"] {
            assert!(reach.contains(nested), "expected {nested} reachable");
        }
        assert!(!reach.contains("AtmosphereSettingsDto"));
    }

    #[test]
    fn generated_animation_and_morph_shapes_are_complete() {
        let defs = emit_component_defs();
        assert!(defs.contains("---@class sa.AnimationPlayer"));
        assert!(defs.contains("---@field autoplay boolean"));
        assert!(defs.contains("---@field wrap \"once\"|\"loop\"|\"pingpong\""));
        assert!(defs.contains("---@field transitionMode \"inertialize\"|\"crossfade\""));
        assert!(defs.contains("---@class sa.Morph"));
        assert!(defs.contains("---@field weights number[]"));
    }

    #[test]
    fn classes_are_sorted_by_name() {
        let defs = emit_component_defs();
        let order: Vec<&str> = defs
            .lines()
            .filter_map(|line| line.strip_prefix("---@class sa."))
            .collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "class blocks must be sorted by name");
    }

    #[test]
    fn defs_are_byte_stable_across_reruns() {
        assert_eq!(emit_component_defs(), emit_component_defs());
    }

    #[test]
    fn map_api_type_references_value_and_handle_classes() {
        assert_eq!(map_api_type("Vec3"), "sa.Vec3");
        assert_eq!(map_api_type("Entity"), "sa.Entity");
        assert_eq!(map_api_type("RayHit"), "sa.RayHit");
        assert_eq!(map_api_type("RagdollState"), "sa.RagdollState");
        assert_eq!(map_api_type("ComponentName"), "sa.ComponentName");
        assert_eq!(map_api_type("Entity[]"), "sa.Entity[]");
        assert_eq!(map_api_type("number"), "number");
        assert_eq!(map_api_type("boolean"), "boolean");
        assert_eq!(map_api_type("string"), "string");
        assert_eq!(map_api_type("table"), "table");
        assert_eq!(map_api_type("any"), "any");
    }

    #[test]
    fn api_defs_carry_the_vec3_value_class() {
        let defs = emit_api_defs();
        assert!(defs.contains("---@class sa.Vec3"));
        for field in [
            "---@field x number",
            "---@field y number",
            "---@field z number",
        ] {
            assert!(defs.contains(field), "missing Vec3 {field}");
        }
        for op in [
            "---@operator add(sa.Vec3): sa.Vec3",
            "---@operator sub(sa.Vec3): sa.Vec3",
            "---@operator mul(number): sa.Vec3",
            "---@operator unm: sa.Vec3",
        ] {
            assert!(defs.contains(op), "missing Vec3 operator {op}");
        }
        for method in [
            "function Vec3:length() end ---@return number",
            "function Vec3:normalized() end ---@return sa.Vec3",
            "function Vec3:dot(other) end ---@param other sa.Vec3 @return number",
            "function Vec3:cross(other) end ---@param other sa.Vec3 @return sa.Vec3",
            "function Vec3:lerp(other, t) end ---@param other sa.Vec3 @param t number @return sa.Vec3",
        ] {
            assert!(defs.contains(method), "missing Vec3 method {method}");
        }
    }

    #[test]
    fn api_defs_carry_every_entity_method_except_get_component() {
        let defs = emit_api_defs();
        assert!(defs.contains("---@class sa.Entity"));
        for binding in BINDINGS
            .iter()
            .filter(|b| b.class == Some("Entity") && b.kind == BindingKind::Method)
        {
            let head = format!("function Entity:{}(", binding.name);
            if binding.name == "get_component" {
                assert!(
                    !defs.contains(&head),
                    "Entity:get_component must be declared only in the component-snapshot tail"
                );
            } else {
                assert!(
                    defs.contains(&head),
                    "missing Entity method {}",
                    binding.name
                );
            }
        }
    }

    #[test]
    fn api_defs_carry_the_synthetic_classes() {
        let defs = emit_api_defs();
        assert!(defs.contains("---@class sa.RayHit\n---@field hit boolean"));
        assert!(defs.contains("---@field entity sa.Entity?"));
        assert!(defs.contains("---@class sa.RagdollState\n---@field present boolean"));
        assert!(defs.contains("---@field body_weight number"));
        assert!(defs.contains("---@class sa.ScriptSelf\n---@field entity sa.Entity"));
        for handler in [
            "function ScriptSelf:on_create() end",
            "function ScriptSelf:on_update(dt) end ---@param dt number",
            "function ScriptSelf:on_destroy() end",
            "function ScriptSelf:on_trigger_enter(other) end ---@param other sa.Entity",
            "function ScriptSelf:on_trigger_exit(other) end ---@param other sa.Entity",
            "function ScriptSelf:on_contact(other, point, normal) end ---@param other sa.Entity \
             @param point sa.Vec3 @param normal sa.Vec3",
        ] {
            assert!(
                defs.contains(handler),
                "missing ScriptSelf handler {handler}"
            );
        }
    }

    #[test]
    fn api_defs_carry_every_free_global() {
        let defs = emit_api_defs();
        assert!(defs.contains("\nsa = {}\n"));
        for binding in BINDINGS
            .iter()
            .filter(|b| b.class.is_none() && b.kind == BindingKind::Free)
        {
            let head = format!("function sa.{}(", binding.name);
            assert!(defs.contains(&head), "missing sa.{} global", binding.name);
        }
    }

    #[test]
    fn api_defs_carry_the_component_name_alias() {
        let defs = emit_api_defs();
        for name in REGISTERED {
            assert!(
                defs.contains(&format!("{name:?}")),
                "sa.ComponentName alias missing {name}"
            );
        }
        let alias_line = defs
            .lines()
            .find(|line| line.starts_with("---@alias sa.ComponentName "))
            .expect("the sa.ComponentName alias line");
        for name in REGISTERED {
            assert!(
                alias_line.contains(&format!("{name:?}")),
                "alias missing {name}"
            );
        }
    }

    #[test]
    fn api_defs_open_with_the_meta_header() {
        assert!(emit_api_defs().starts_with("---@meta\n"));
    }

    #[test]
    fn api_defs_are_byte_stable_across_reruns() {
        assert_eq!(emit_api_defs(), emit_api_defs());
    }

    #[test]
    fn combined_defs_are_api_then_components_and_byte_stable() {
        let defs = emit_defs();
        let api_at = defs.find("---@class sa.Vec3").expect("the Vec3 class");
        let snapshot_at = defs
            .find("-- Typed component snapshots.")
            .expect("the component-snapshot header");
        assert!(
            api_at < snapshot_at,
            "the API surface must precede the snapshots"
        );
        assert_eq!(
            defs.matches("function Entity:get_component(").count(),
            1,
            "Entity:get_component must be declared exactly once (the snapshot tail)"
        );
        assert_eq!(emit_defs(), emit_defs());
    }

    #[test]
    fn combined_defs_match_committed_artifact() {
        let committed = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/control/sa.generated.luau"
        ));
        assert_eq!(
            emit_defs(),
            committed,
            "run `cargo run -p xtask gen-protocol`"
        );
    }
}
