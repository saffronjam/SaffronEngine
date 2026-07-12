//! The shared `wire-type -> Luau` mapper + the `sa.*` API and component-snapshot `.luau` emitters.
//!
//! This is the single-source Luau type surface: the same single-source discipline as
//! `@saffron/protocol`, applied to the Lua-facing types. It emits one plain `.luau` defs file;
//! there is no `library/sa.lua` hand-written overlay and no `check-script-defs` drift tripwire —
//! the regen-freshness diff is the drift guard.
//!
//! Three pieces, one [`map_type`] mapper:
//!
//! - [`emit_api_defs`] walks the [`saffron_script::BINDINGS`] descriptor table — the single
//!   binding source the runtime VM registers from — to emit the `sa.Vec3` value class
//!   (fields + `---@operator` overloads + methods), the synthetic `sa.RayHit`/`sa.RagdollState`/
//!   `sa.ScriptSelf` classes, the `sa.Entity` method set, the `sa.*` free-function/global table,
//!   and the `sa.ComponentName` alias (the registered-name union from [`REGISTERED`]).
//! - [`emit_component_defs`] emits the typed `:get_component(name)` snapshots — the
//!   `---@class sa.<Component>` blocks + the `---@overload` lines — from the same component
//!   wire-shape catalog the registry knows.
//! - [`emit_defs`] concatenates the two into the single `.luau` defs file.
//!
//! The component wire shapes are the hand-authored catalog in `component_block.ts`, the
//! registered-name set is [`REGISTERED`], and the two shapes with no catalog entry
//! (`AnimationPlayer`, `MaterialAsset`) are supplied here.

use std::collections::BTreeSet;
use std::collections::HashMap;

use saffron_script::{BINDINGS, Binding, BindingKind};

/// The hand-authored component-interface catalog (the wire shapes the `:get_component` snapshots
/// reproduce). The component-snapshot defs are parsed from these interfaces, so there is one
/// source for the component wire shape.
const COMPONENT_BLOCK: &str = include_str!("component_block.ts");

/// The component names the registry registers, in registration order — the roots of the
/// reachability walk: the 21 serialized components plus `MaterialAsset` and `AnimationPlayer`,
/// which serialize through their own serde but have no `component_block.ts` interface (they are
/// the two synthetic shapes [`synthetic_shapes`] supplies).
pub const REGISTERED: &[&str] = &[
    "Name",
    "Transform",
    "Mesh",
    "Camera",
    "Material",
    "MaterialSet",
    "MaterialAsset",
    "ModelInstance",
    "Script",
    "AnimationPlayer",
    "DirectionalLight",
    "PointLight",
    "SpotLight",
    "ReflectionProbe",
    "FogVolume",
    "Relationship",
    "SkinnedMesh",
    "Bone",
    "FootIk",
    "BonePhysics",
    "Rigidbody",
    "Collider",
    "KinematicBones",
    "CharacterController",
];

/// One parsed interface field: the wire name, its wire-type token, and whether it is optional.
#[derive(Debug, Clone)]
pub struct Field {
    /// The wire (camelCase) field name.
    pub name: String,
    /// The wire-type token (the `component_block.ts` spelling: `Vec3`, `WireUuid`, a `T[]`
    /// array, a `"a" | "b"` union, a nested interface name, …).
    pub ty: String,
    /// `true` when the TS field carries the `?` optional marker.
    pub optional: bool,
}

/// Map a wire-type token to its Luau type annotation — the one helper both the component-snapshot
/// emitter and the `sa.*` API emitter call.
///
/// - `number` / `boolean` / `string` pass through.
/// - `WireUuid` -> `string` (ids cross as decimal strings).
/// - `Vec3` -> `{ x: number, y: number, z: number }`, `Vec4` adds `w`.
/// - `Record<string, unknown>` -> `table<string, any>`.
/// - `T[]` -> `<mapped T>[]` (nested, so `T[][]` works).
/// - a `"a" | "b"` string-literal union -> the union with whitespace stripped.
/// - any other token is a nested interface -> `sa.<Name>`.
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

/// The interface a field type references (the node the reachability walk follows), or `None` for
/// primitives, vectors, arrays-of-primitive, unions, and generics — so the emitted `---@class`
/// set grows transitively from the registered roots: nested DTOs are emitted, unrelated ones are
/// not.
fn referenced(ty: &str) -> Option<&str> {
    let base = ty.trim_end_matches("[]");
    match base {
        "number" | "boolean" | "string" | "WireUuid" | "Vec3" | "Vec4" => None,
        _ if base.contains('|') || base.contains('<') => None,
        _ => Some(base),
    }
}

/// The synthetic component shapes with no `component_block.ts` interface: they serialize through
/// their own serde, not a catalog interface, so their literal field lists are supplied here.
fn synthetic_shapes() -> Vec<(String, Vec<Field>)> {
    let field = |name: &str, ty: &str| Field {
        name: name.to_owned(),
        ty: ty.to_owned(),
        optional: false,
    };
    vec![
        (
            "AnimationPlayer".to_owned(),
            vec![
                field("clip", "WireUuid"),
                field("time", "number"),
                field("speed", "number"),
                field("wrap", "\"once\" | \"loop\" | \"pingpong\""),
                field("playing", "boolean"),
                field("transitionMode", "\"crossfade\" | \"inertialize\""),
                field("loopBlend", "number"),
            ],
        ),
        (
            "MaterialAsset".to_owned(),
            vec![field("material", "WireUuid")],
        ),
    ]
}

/// Parse `component_block.ts` into `interface name -> ordered fields`, then add the two synthetic
/// shapes (which have no catalog interface). `referenced` types not present here are leaf nodes
/// (`Vec3`/unions/primitives), so the walk simply stops at them.
fn interfaces() -> HashMap<String, Vec<Field>> {
    let mut out = parse_interfaces(COMPONENT_BLOCK);
    for (name, fields) in synthetic_shapes() {
        out.entry(name).or_insert(fields);
    }
    out
}

/// Parse every `export interface Name { ... }` block into ordered [`Field`]s. The catalog is
/// flat (no nested braces inside a body), so a brace-delimited scan over the field lines suffices.
fn parse_interfaces(text: &str) -> HashMap<String, Vec<Field>> {
    let mut out = HashMap::new();
    let mut rest = text;
    while let Some(idx) = rest.find("export interface ") {
        let after_kw = &rest[idx + "export interface ".len()..];
        let Some(brace) = after_kw.find('{') else {
            break;
        };
        let name = after_kw[..brace].trim().to_owned();
        let body_start = &after_kw[brace + 1..];
        let Some(close) = body_start.find('}') else {
            break;
        };
        let body = &body_start[..close];
        out.insert(name, parse_fields(body));
        rest = &body_start[close + 1..];
    }
    out
}

/// Split one interface body into ordered `(name, type, optional)` fields, one per line.
fn parse_fields(body: &str) -> Vec<Field> {
    let mut fields = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        let Some((lhs, rhs)) = line.split_once(':') else {
            continue;
        };
        let optional = lhs.ends_with('?');
        let name = lhs.trim_end_matches('?').trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let ty = rhs.trim().trim_end_matches(';').trim().to_owned();
        fields.push(Field {
            name: name.to_owned(),
            ty,
            optional,
        });
    }
    fields
}

/// The transitive set of interface names reachable from [`REGISTERED`] via field references — the
/// `---@class` set. Nested DTOs (`BVec3`, `PhysicsMaterial`, `FootChainDto`, …) are pulled in;
/// unrelated interfaces are not.
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
            if let Some(reference) = referenced(&field.ty) {
                if interfaces.contains_key(reference) && !reach.contains(reference) {
                    queue.push(reference.to_owned());
                }
            }
        }
    }
    reach
}

/// Emit the component-snapshot `.luau` defs: the `---@class sa.<Component>` blocks (sorted by
/// name) for every interface reachable from the registered set, plus the `---@overload` lines for
/// the registered components and the `Entity:get_component` stub. Byte-stable across re-runs (the
/// freshness gate).
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

/// Map an `sa.*` binding-table type token to its Luau type annotation — the API half of the
/// shared mapper. Unlike [`map_type`] (which expands `Vec3` to the inline `{x,y,z}` snapshot shape
/// for `:get_component`), the API surface references the value/handle classes by name: `sa.vec3`
/// returns the `sa.Vec3` userdata, `sa.spawn` an `sa.Entity`. The primitives still go through
/// [`map_type`], so the mapping has one owner.
///
/// - `number`/`boolean`/`string` pass through (via [`map_type`]).
/// - `Vec3`/`Entity`/`RayHit`/`RagdollState`/`ScriptSelf` -> `sa.<Name>` (the API classes).
/// - `ComponentName` -> `sa.ComponentName` (the registered-name alias).
/// - `table`/`any` pass through (an opaque wire snapshot / payload).
/// - `T[]` -> `<mapped T>[]` (nested).
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

/// The `---@param`/`@return` tail for one binding's stub: each argument's `@param name type`
/// then the `@return type` when the binding returns a value. Empty for a no-arg, no-return
/// binding (a bare `end`).
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

/// One method/function stub: `function <owner><sep><name>(<args>) end<doc tail>`. `owner`/`sep`
/// are `"Entity:"` for a method or `"sa."` for a free function; the parameter list is the bound
/// argument names.
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

/// The `sa.Vec3` value-class block: the `---@field`s (from the `Field` bindings), the
/// `---@operator` overloads (from the arithmetic `Meta` bindings), and the method stubs (the
/// `Method` bindings). The `Static` constructor (`sa.Vec3.new`) and the comparison/string
/// metamethods (`__eq`/`__tostring`, which are not LuaLS `---@operator`s) are not annotated —
/// scripts construct via `sa.vec3(...)` and `==`/`tostring` need no type hint. Generated from
/// [`BINDINGS`].
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
        // `__add`/`__sub`/`__mul`/`__unm` are LuaLS arithmetic operators; `__eq`/`__tostring`
        // are not annotated as `---@operator`.
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

/// The `sa.Entity` handle block: `---@class sa.Entity`, `local Entity = {}`, and a stub per
/// `Method` binding owned by `Entity` — minus `get_component`, which the component-snapshot tail
/// declares with its per-component typed overloads. Generated from [`BINDINGS`].
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

/// The `sa` namespace table: `sa = {}` then a stub per `Free` binding (`sa.vec3`, `sa.log`, the
/// input trio + mouse, the query/hierarchy helpers, `sa.raycast`/`sa.spherecast`, `sa.broadcast`,
/// and the scheduler `wait`/`delay`/`spawn_task`). Generated from [`BINDINGS`].
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

/// The `sa.ComponentName` alias: the union of every registered component name (the roots of the
/// snapshot reachability walk, [`REGISTERED`]). `get_component`/`has_component` accept all of
/// them; the structural ones are rejected by `set/add/remove_component` at runtime.
fn emit_component_name_alias() -> String {
    let union = REGISTERED
        .iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join("|");
    format!("---@alias sa.ComponentName {union}")
}

/// Emit the `sa.*` API surface as `.luau` type defs from the [`BINDINGS`] descriptor table — the
/// single binding source the runtime VM registers from (`saffron-script`'s `register_*` walks the
/// same table). Covers the `sa.Vec3` value class, the synthetic `sa.RayHit`/`sa.RagdollState`/
/// `sa.ScriptSelf` result/handler shapes, the `sa.Entity` method set, the `sa = {}` free-function
/// table, and the `sa.ComponentName` alias. Byte-stable across re-runs (the freshness gate).
///
/// The `RayHit`/`RagdollState`/`ScriptSelf` classes are synthetic: `RayHit`/`RagdollState` are the
/// POD result tables `sa.raycast`/`Entity:ragdoll_state` shape (their fields are not in the
/// descriptor table — only the return *token* is), and `ScriptSelf` is the handler-shape contract
/// the runtime calls into. Their literal field/handler lists are supplied here, matching the
/// `ScriptRayHit`/`ScriptRagdollState` POD and the lifecycle handlers.
#[must_use]
pub fn emit_api_defs() -> String {
    let header = "---@meta\n-- Saffron Anima Lua API. Generated from the saffron-script binding \
                  table; do not edit by hand.\n-- Types only: the real bindings are the mlua \
                  registration walk over the same table.";

    let ray_hit = "---@class sa.RayHit\n---@field hit boolean\n---@field distance number\n---@field \
                   point sa.Vec3\n---@field normal sa.Vec3\n---@field entity sa.Entity?";

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
        ragdoll_state.to_owned(),
        emit_component_name_alias(),
        emit_entity_class(),
        script_self.to_owned(),
        emit_namespace(),
    ]
    .join("\n\n")
        + "\n"
}

/// Assemble the single `.luau` defs file: the `sa.*` API surface ([`emit_api_defs`]) followed by
/// the component snapshots ([`emit_component_defs`]), generated from one source. This is the LuaLS
/// def file written into every project's `library/` and the committed artifact the freshness diff
/// guards.
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
        assert_eq!(map_type("Material[]"), "sa.Material[]");
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
        // Referenced nested shapes are emitted.
        for nested in ["BVec3", "PhysicsMaterial", "FootChainDto", "BonePhysicsDto"] {
            assert!(reach.contains(nested), "expected {nested} reachable");
        }
        // `AtmosphereSettingsDto` is in the catalog but referenced by no registered component, so
        // it is not in the `---@class` set.
        assert!(!reach.contains("AtmosphereSettingsDto"));
    }

    #[test]
    fn synthetic_shapes_carry_their_literal_fields() {
        let defs = emit_component_defs();
        assert!(defs.contains("---@class sa.AnimationPlayer"));
        assert!(defs.contains("---@field wrap \"once\"|\"loop\"|\"pingpong\""));
        assert!(defs.contains("---@field transitionMode \"crossfade\"|\"inertialize\""));
        assert!(defs.contains("---@class sa.MaterialAsset\n---@field material string"));
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
        // The API half references the value/handle classes by name (unlike `map_type`, which
        // expands Vec3 to the inline snapshot shape).
        assert_eq!(map_api_type("Vec3"), "sa.Vec3");
        assert_eq!(map_api_type("Entity"), "sa.Entity");
        assert_eq!(map_api_type("RayHit"), "sa.RayHit");
        assert_eq!(map_api_type("RagdollState"), "sa.RagdollState");
        assert_eq!(map_api_type("ComponentName"), "sa.ComponentName");
        assert_eq!(map_api_type("Entity[]"), "sa.Entity[]");
        // Primitives and the opaque tokens pass through.
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
        // The arithmetic operators are emitted; `__eq`/`__tostring` are not `---@operator`s.
        for op in [
            "---@operator add(sa.Vec3): sa.Vec3",
            "---@operator sub(sa.Vec3): sa.Vec3",
            "---@operator mul(number): sa.Vec3",
            "---@operator unm: sa.Vec3",
        ] {
            assert!(defs.contains(op), "missing Vec3 operator {op}");
        }
        // The value-class methods.
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
        // Every `Entity:` method binding has a stub.
        for binding in BINDINGS
            .iter()
            .filter(|b| b.class == Some("Entity") && b.kind == BindingKind::Method)
        {
            let head = format!("function Entity:{}(", binding.name);
            if binding.name == "get_component" {
                // get_component is declared in the component-snapshot tail (with typed
                // per-component overloads), never in the API surface.
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
        // The alias is one line listing the registered union.
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
        // LuaLS treats a `---@meta` file as type-only (never executed).
        assert!(emit_api_defs().starts_with("---@meta\n"));
    }

    #[test]
    fn api_defs_are_byte_stable_across_reruns() {
        assert_eq!(emit_api_defs(), emit_api_defs());
    }

    #[test]
    fn combined_defs_are_api_then_components_and_byte_stable() {
        let defs = emit_defs();
        // The API surface comes first, the snapshots second.
        let api_at = defs.find("---@class sa.Vec3").expect("the Vec3 class");
        let snapshot_at = defs
            .find("-- Typed component snapshots.")
            .expect("the component-snapshot header");
        assert!(
            api_at < snapshot_at,
            "the API surface must precede the snapshots"
        );
        // get_component is declared exactly once, in the snapshot tail.
        assert_eq!(
            defs.matches("function Entity:get_component(").count(),
            1,
            "Entity:get_component must be declared exactly once (the snapshot tail)"
        );
        // The freshness gate: byte-stable across re-runs.
        assert_eq!(emit_defs(), emit_defs());
    }

    #[test]
    fn combined_defs_match_committed_artifact() {
        // The committed `schemas/control/sa.generated.luau` (embedded by the host's
        // `sa_lua_defs`) must equal the live emit — the regen-freshness contract.
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
