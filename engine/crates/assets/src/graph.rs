//! The two pure-CPU node-graph functions: [`lower_graph_to_params`] (collapse a
//! constant/texture-only graph into the flat [`MaterialAsset`] factor/texture fields)
//! and [`emit_graph_surface`] (emit the Slang `evalSurface` body from the graph).
//!
//! Both are deterministic JSON-walkers over the graph's `nodes` / `edges` arrays. The
//! folding decision and the emitted Slang are a frozen contract with the editor's
//! node-graph model: the channel name strings (`baseColor`, `emissive`, `metallic`,
//! `roughness`, `emissiveStrength`, `normal`, `height`), the node type strings
//! (`constant`, `texture`, `textureSlot`, `materialOutput`, the math/utility types), the
//! pin names (`a` / `b` / `t`), and the `mesh`-vs-preview context differences must
//! reproduce byte-for-byte or a graph material silently miscompiles.
//!
//! There is no `slangc` here — this module produces the *strings* and the *fold decision*;
//! [`crate::codegen`] compiles them.

use std::collections::HashMap;

use saffron_core::Uuid;
use saffron_geometry::glam::{Vec3, Vec4};
use saffron_json::Value;

use crate::material::MaterialAsset;

/// Reads a [`Uuid`] from a node-prop value, accepting a decimal string *or* an unsigned
/// number. Anything else is `0`.
fn uuid_of(value: &Value) -> Uuid {
    match value {
        Value::String(s) => Uuid(s.parse::<u64>().unwrap_or(0)),
        Value::Number(n) => Uuid(n.as_u64().unwrap_or(0)),
        _ => Uuid(0),
    }
}

/// Coerces a constant-node `value` to a scalar: the first element of a non-empty array,
/// a bare number as-is, else `0`.
fn scalar(value: &Value) -> f32 {
    match value {
        Value::Array(elements) => elements.first().and_then(Value::as_f64).unwrap_or(0.0) as f32,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) as f32,
        _ => 0.0,
    }
}

/// Reads the `i`-th element of a JSON array as `f32`, returning `default` when the array
/// is shorter.
fn array_at(value: &Value, i: usize, default: f32) -> f32 {
    value
        .as_array()
        .and_then(|elements| elements.get(i))
        .and_then(Value::as_f64)
        .map_or(default, |v| v as f32)
}

/// Reads a string member of a JSON object, returning the empty string when absent or not
/// a string.
fn str_member<'a>(object: &'a Value, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Folds a constant/texture-only node graph into the flat [`MaterialAsset`] factor /
/// texture fields, returning `true` when the whole graph folded.
///
/// Walks `material.graph`-shaped JSON: the `materialOutput` node's incoming edges decide
/// each channel. A `constant` source folds into the matching factor field; a `texture`
/// source folds into the matching texture id. The function returns `false` the moment it
/// hits a procedural/math node, an unrecognized channel, or a source it cannot resolve —
/// that graph routes to the Slang codegen path instead.
///
/// Mutation is in place as the walk proceeds: a caller commits the folded result only
/// when this returns `true` (clone, fold, keep-if-folded).
pub fn lower_graph_to_params(graph: &Value, material: &mut MaterialAsset) -> bool {
    if !graph.is_object() {
        return false;
    }
    let (Some(nodes), Some(edges)) = (
        graph.get("nodes").and_then(Value::as_array),
        graph.get("edges").and_then(Value::as_array),
    ) else {
        return false;
    };

    let mut by_id: HashMap<&str, &Value> = HashMap::new();
    let mut output_id = "";
    for node in nodes {
        by_id.insert(str_member(node, "id"), node);
        if str_member(node, "type") == "materialOutput" {
            output_id = node.get("id").and_then(Value::as_str).unwrap_or("");
        }
    }
    if output_id.is_empty() {
        return false;
    }

    let mut foldable = true;
    for edge in edges {
        let (Some(to), Some(from)) = (
            edge.get("to").and_then(Value::as_array),
            edge.get("from").and_then(Value::as_array),
        ) else {
            continue;
        };
        if to.len() < 2 || from.is_empty() || to[0].as_str() != Some(output_id) {
            continue;
        }
        let Some(channel) = to[1].as_str() else {
            continue;
        };
        let Some(src) = from[0].as_str().and_then(|id| by_id.get(id).copied()) else {
            foldable = false;
            continue;
        };

        let node_type = str_member(src, "type");
        let empty = Value::Object(serde_json::Map::new());
        let props = src.get("props").filter(|v| v.is_object()).unwrap_or(&empty);
        match node_type {
            "constant" => {
                let empty_array = Value::Array(Vec::new());
                let value = props.get("value").unwrap_or(&empty_array);
                match channel {
                    "baseColor" if value.as_array().is_some_and(|a| a.len() >= 4) => {
                        material.base_color = Vec4::new(
                            array_at(value, 0, 0.0),
                            array_at(value, 1, 0.0),
                            array_at(value, 2, 0.0),
                            array_at(value, 3, 0.0),
                        );
                    }
                    "emissive" if value.as_array().is_some_and(|a| a.len() >= 3) => {
                        material.emissive = Vec3::new(
                            array_at(value, 0, 0.0),
                            array_at(value, 1, 0.0),
                            array_at(value, 2, 0.0),
                        );
                    }
                    "metallic" => material.metallic = scalar(value),
                    "roughness" => material.roughness = scalar(value),
                    "emissiveStrength" => material.emissive_strength = scalar(value),
                    _ => foldable = false,
                }
            }
            "texture" => {
                let empty_value = Value::Null;
                let asset = uuid_of(props.get("asset").unwrap_or(&empty_value));
                match channel {
                    "baseColor" => material.albedo_texture = asset,
                    "normal" => material.normal_texture = asset,
                    "emissive" => material.emissive_texture = asset,
                    "roughness" | "metallic" => material.orm_texture = asset,
                    "height" => material.height_texture = asset,
                    _ => foldable = false,
                }
            }
            // A procedural / math node forces the codegen path.
            _ => foldable = false,
        }
    }
    foldable
}

/// Emits the body of `evalSurface` for a node graph: one Slang statement per node (in
/// array order — inputs must precede consumers), then the `materialOutput` channel
/// assignments.
///
/// `mesh == false` targets the self-contained preview/shell shader (a `Mat mat` push +
/// `textures[]` + a `uv` param); `mesh == true` targets the
/// übershader's `evalSurface(MaterialInput m)` — `m.mat`, `albedoTextures[]`, a `uv` local
/// the splice template provides. Both emit the complete `SurfaceData` field contract.
/// An empty or non-object graph emits the default passthrough body.
#[must_use]
pub fn emit_graph_surface(graph: &Value, mesh: bool) -> String {
    let base_color = if mesh {
        "m.mat.baseColor"
    } else {
        "mat.baseColor"
    };
    let roughness = if mesh {
        "((m.mat.tex1.w & FEATURE_THIN_SHEET) != 0u ? m.mat.thinReflection.z : 1.0)"
    } else {
        "1.0"
    };
    let mut body = format!(
        "    s.albedo = {base_color}.rgb;\n    s.metallic = 0.0;\n    s.roughness = {roughness};\n    s.emissive = float3(0.0);\n"
    );
    if mesh {
        body += &format!(
            "    s.normal = normalize(m.worldNormal);\n    s.occlusion = 1.0;\n    s.opacity = {base_color}.a;\n"
        );
    } else {
        body += concat!(
            "    s.normal = float3(0.0, 0.0, 1.0);\n",
            "    s.sheetNormal = float3(0.0, 0.0, 1.0);\n",
            "    s.occlusion = 1.0;\n",
            "    s.opacity = mat.baseColor.a;\n",
            "    s.surfaceModel = 0u;\n",
            "    s.frontFace = 1.0;\n",
            "    s.frontResponse = 1.0;\n",
            "    s.backResponse = 1.0;\n",
            "    s.thickness = 0.0;\n",
            "    s.absorption = float3(0.0);\n",
            "    s.transmission = float3(0.0);\n",
            "    s.energyLimit = 1.0;\n",
        );
    }
    if !graph.is_object() {
        return body;
    }

    // "node:pin" -> source node id, from the edge list.
    let mut input_from: HashMap<String, String> = HashMap::new();
    if let Some(edges) = graph.get("edges").and_then(Value::as_array) {
        for edge in edges {
            let to = edge.get("to").and_then(Value::as_array);
            let from = edge.get("from").and_then(Value::as_array);
            if let (Some(to), Some(from)) = (to, from)
                && to.len() >= 2
                && !from.is_empty()
                && let (Some(to_node), Some(to_pin), Some(from_node)) =
                    (to[0].as_str(), to[1].as_str(), from[0].as_str())
            {
                input_from.insert(format!("{to_node}:{to_pin}"), from_node.to_owned());
            }
        }
    }

    let mut output_id = String::new();
    if let Some(nodes) = graph.get("nodes").and_then(Value::as_array) {
        for node in nodes {
            let id = str_member(node, "id");
            let node_type = str_member(node, "type");
            let empty = Value::Object(serde_json::Map::new());
            let props = node
                .get("props")
                .filter(|v| v.is_object())
                .unwrap_or(&empty);
            match node_type {
                "materialOutput" => output_id = id.to_owned(),
                "constant" => {
                    let empty_array = Value::Array(Vec::new());
                    let value = props.get("value").unwrap_or(&empty_array);
                    body += &format!(
                        "    float4 n_{id} = float4({}, {}, {}, {});\n",
                        array_at(value, 0, 0.0),
                        array_at(value, 1, 0.0),
                        array_at(value, 2, 0.0),
                        array_at(value, 3, 1.0),
                    );
                }
                "textureSlot" => {
                    let slot = props
                        .get("slot")
                        .and_then(Value::as_str)
                        .unwrap_or("albedo");
                    let arr = if mesh { "albedoTextures" } else { "textures" };
                    let idx = texture_slot_index(slot, mesh);
                    body += &format!(
                        "    float4 n_{id} = {arr}[NonUniformResourceIndex({idx})].Sample(uv);\n"
                    );
                }
                _ => {
                    // Math / utility nodes. Inputs wired by pin name (a/b/t); all float4.
                    let a = input_from
                        .get(&format!("{id}:a"))
                        .map_or("", String::as_str);
                    let b = input_from
                        .get(&format!("{id}:b"))
                        .map_or("", String::as_str);
                    let t = input_from
                        .get(&format!("{id}:t"))
                        .map_or("", String::as_str);
                    body += &emit_math_node(node_type, id, a, b, t);
                }
            }
        }
    }

    let src_for = |pin: &str| -> Option<&String> { input_from.get(&format!("{output_id}:{pin}")) };
    if let Some(s) = src_for("baseColor") {
        body += &format!("    s.albedo = n_{s}.rgb;\n");
    }
    if let Some(s) = src_for("metallic") {
        body += &format!("    s.metallic = n_{s}.r;\n");
    }
    if let Some(s) = src_for("roughness") {
        body += &format!("    s.roughness = n_{s}.r;\n");
    }
    if let Some(s) = src_for("emissive") {
        body += &format!("    s.emissive = n_{s}.rgb;\n");
    }
    if let Some(s) = src_for("occlusion") {
        body += &format!("    s.occlusion = saturate(n_{s}.r);\n");
    }
    if let Some(s) = src_for("normal") {
        body += &format!("    s.normal = normalize(n_{s}.rgb);\n");
    }
    if let Some(s) = src_for("opacity") {
        body += &format!("    s.opacity = saturate(n_{s}.r);\n");
    }
    if let Some(s) = src_for("frontAlbedoResponse") {
        body += &format!("    s.frontResponse = saturate(n_{s}.r);\n");
    }
    if let Some(s) = src_for("backAlbedoResponse") {
        body += &format!("    s.backResponse = saturate(n_{s}.r);\n");
    }
    if let Some(s) = src_for("thickness") {
        body += &format!("    s.thickness = max(n_{s}.r, 1e-5);\n");
    }
    if let Some(s) = src_for("absorption") {
        body += &format!("    s.absorption = max(n_{s}.rgb, float3(0.0));\n");
    }
    if let Some(s) = src_for("transmission") {
        body += &format!("    s.transmission = saturate(n_{s}.rgb);\n");
    }
    if let Some(s) = src_for("energyLimit") {
        body += &format!("    s.energyLimit = saturate(n_{s}.r);\n");
    }
    body
}

/// The push-constant index expression for a `textureSlot` node, by slot name and target
/// context — `m.mat.tex0/tex1` swizzles for the übershader (`mesh == true`) and `mat.tex`
/// swizzles for the self-contained preview/shell shader.
fn texture_slot_index(slot: &str, mesh: bool) -> &'static str {
    if mesh {
        match slot {
            "metallicRoughness" | "mr" => "m.mat.tex0.y",
            "normal" => "m.mat.tex0.z",
            "emissive" => "m.mat.tex0.w",
            "height" => "m.mat.tex1.x",
            "occlusion" => "m.mat.tex1.y",
            _ => "m.mat.tex0.x",
        }
    } else {
        match slot {
            "metallicRoughness" | "mr" => "mat.tex.y",
            "normal" => "mat.tex.z",
            "emissive" => "mat.tex.w",
            _ => "mat.tex.x",
        }
    }
}

/// Emits the Slang statement for a math / utility node. Inputs are the source node ids
/// wired to the `a` / `b` / `t` pins; all values are `float4` (an unwired pin yields the
/// empty-source `n_` reference).
fn emit_math_node(node_type: &str, id: &str, a: &str, b: &str, t: &str) -> String {
    match node_type {
        "multiply" => format!("    float4 n_{id} = n_{a} * n_{b};\n"),
        "add" => format!("    float4 n_{id} = n_{a} + n_{b};\n"),
        "subtract" => format!("    float4 n_{id} = n_{a} - n_{b};\n"),
        "divide" => format!("    float4 n_{id} = n_{a} / max(n_{b}, float4(1e-5));\n"),
        "lerp" => format!("    float4 n_{id} = lerp(n_{a}, n_{b}, n_{t});\n"),
        "saturate" | "clamp" => format!("    float4 n_{id} = saturate(n_{a});\n"),
        "oneMinus" => format!("    float4 n_{id} = 1.0 - n_{a};\n"),
        "dot" => format!("    float4 n_{id} = float4(dot(n_{a}.rgb, n_{b}.rgb));\n"),
        "uv" => format!("    float4 n_{id} = float4(uv, 0.0, 1.0);\n"),
        "sin" => format!("    float4 n_{id} = sin(n_{a});\n"),
        "cos" => format!("    float4 n_{id} = cos(n_{a});\n"),
        "frac" => format!("    float4 n_{id} = frac(n_{a});\n"),
        "step" => format!("    float4 n_{id} = step(n_{a}, n_{b});\n"),
        "smoothstep" => format!("    float4 n_{id} = smoothstep(n_{a}, n_{b}, n_{t});\n"),
        _ => format!("    float4 n_{id} = float4(0.0);  // unknown node '{node_type}'\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::default_material_asset;

    /// The complete default passthrough body for the self-contained preview shader.
    const PREVIEW_PASSTHROUGH: &str = concat!(
        "    s.albedo = mat.baseColor.rgb;\n",
        "    s.metallic = 0.0;\n",
        "    s.roughness = 1.0;\n",
        "    s.emissive = float3(0.0);\n",
        "    s.normal = float3(0.0, 0.0, 1.0);\n",
        "    s.sheetNormal = float3(0.0, 0.0, 1.0);\n",
        "    s.occlusion = 1.0;\n",
        "    s.opacity = mat.baseColor.a;\n",
        "    s.surfaceModel = 0u;\n",
        "    s.frontFace = 1.0;\n",
        "    s.frontResponse = 1.0;\n",
        "    s.backResponse = 1.0;\n",
        "    s.thickness = 0.0;\n",
        "    s.absorption = float3(0.0);\n",
        "    s.transmission = float3(0.0);\n",
        "    s.energyLimit = 1.0;\n",
    );

    /// The default graph-owned fields for the complete übershader surface.
    const MESH_PASSTHROUGH: &str = concat!(
        "    s.albedo = m.mat.baseColor.rgb;\n",
        "    s.metallic = 0.0;\n",
        "    s.roughness = ((m.mat.tex1.w & FEATURE_THIN_SHEET) != 0u ? m.mat.thinReflection.z : 1.0);\n",
        "    s.emissive = float3(0.0);\n",
        "    s.normal = normalize(m.worldNormal);\n",
        "    s.occlusion = 1.0;\n",
        "    s.opacity = m.mat.baseColor.a;\n",
    );

    #[test]
    fn absent_graph_emits_the_default_passthrough_body() {
        // A non-object graph (null) emits only the passthrough for each context.
        assert_eq!(emit_graph_surface(&Value::Null, false), PREVIEW_PASSTHROUGH);
        assert_eq!(emit_graph_surface(&Value::Null, true), MESH_PASSTHROUGH);
    }

    #[test]
    fn empty_graph_object_emits_the_default_passthrough_body() {
        // An object with no nodes/edges: the passthrough plus no statements and no
        // channel assignments (no materialOutput edges).
        let graph = serde_json::json!({ "nodes": [], "edges": [] });
        assert_eq!(emit_graph_surface(&graph, false), PREVIEW_PASSTHROUGH);
        assert_eq!(emit_graph_surface(&graph, true), MESH_PASSTHROUGH);
    }

    /// A small graph: a `constant`, a `textureSlot` (normal), a `multiply` wiring the two,
    /// and a `materialOutput` whose `baseColor` is the multiply (the math node forces
    /// codegen, exercising every emit branch).
    fn small_graph() -> Value {
        serde_json::json!({
            "nodes": [
                { "id": "c1", "type": "constant", "props": { "value": [0.5, 0.25, 1.0, 1.0] } },
                { "id": "tx", "type": "textureSlot", "props": { "slot": "normal" } },
                { "id": "mul", "type": "multiply" },
                { "id": "out", "type": "materialOutput" }
            ],
            "edges": [
                { "from": ["c1", "out"], "to": ["mul", "a"] },
                { "from": ["tx", "out"], "to": ["mul", "b"] },
                { "from": ["mul", "out"], "to": ["out", "baseColor"] }
            ]
        })
    }

    #[test]
    fn small_graph_preview_matches_golden() {
        let golden = concat!(
            "    s.albedo = mat.baseColor.rgb;\n",
            "    s.metallic = 0.0;\n",
            "    s.roughness = 1.0;\n",
            "    s.emissive = float3(0.0);\n",
            "    s.normal = float3(0.0, 0.0, 1.0);\n",
            "    s.sheetNormal = float3(0.0, 0.0, 1.0);\n",
            "    s.occlusion = 1.0;\n",
            "    s.opacity = mat.baseColor.a;\n",
            "    s.surfaceModel = 0u;\n",
            "    s.frontFace = 1.0;\n",
            "    s.frontResponse = 1.0;\n",
            "    s.backResponse = 1.0;\n",
            "    s.thickness = 0.0;\n",
            "    s.absorption = float3(0.0);\n",
            "    s.transmission = float3(0.0);\n",
            "    s.energyLimit = 1.0;\n",
            "    float4 n_c1 = float4(0.5, 0.25, 1, 1);\n",
            "    float4 n_tx = textures[NonUniformResourceIndex(mat.tex.z)].Sample(uv);\n",
            "    float4 n_mul = n_c1 * n_tx;\n",
            "    s.albedo = n_mul.rgb;\n",
        );
        assert_eq!(emit_graph_surface(&small_graph(), false), golden);
    }

    #[test]
    fn small_graph_mesh_matches_golden() {
        let golden = concat!(
            "    s.albedo = m.mat.baseColor.rgb;\n",
            "    s.metallic = 0.0;\n",
            "    s.roughness = ((m.mat.tex1.w & FEATURE_THIN_SHEET) != 0u ? m.mat.thinReflection.z : 1.0);\n",
            "    s.emissive = float3(0.0);\n",
            "    s.normal = normalize(m.worldNormal);\n",
            "    s.occlusion = 1.0;\n",
            "    s.opacity = m.mat.baseColor.a;\n",
            "    float4 n_c1 = float4(0.5, 0.25, 1, 1);\n",
            "    float4 n_tx = albedoTextures[NonUniformResourceIndex(m.mat.tex0.z)].Sample(uv);\n",
            "    float4 n_mul = n_c1 * n_tx;\n",
            "    s.albedo = n_mul.rgb;\n",
        );
        assert_eq!(emit_graph_surface(&small_graph(), true), golden);
    }

    #[test]
    fn all_output_channels_emit_in_fixed_order() {
        // Wire all four channels from distinct constant nodes; the assignments emit in the
        // baseColor/metallic/roughness/emissive order regardless of edge order.
        let graph = serde_json::json!({
            "nodes": [
                { "id": "e", "type": "constant", "props": { "value": [1, 1, 1, 1] } },
                { "id": "r", "type": "constant", "props": { "value": [1, 1, 1, 1] } },
                { "id": "m", "type": "constant", "props": { "value": [1, 1, 1, 1] } },
                { "id": "b", "type": "constant", "props": { "value": [1, 1, 1, 1] } },
                { "id": "out", "type": "materialOutput" }
            ],
            "edges": [
                { "from": ["e", "o"], "to": ["out", "emissive"] },
                { "from": ["r", "o"], "to": ["out", "roughness"] },
                { "from": ["m", "o"], "to": ["out", "metallic"] },
                { "from": ["b", "o"], "to": ["out", "baseColor"] }
            ]
        });
        let out = emit_graph_surface(&graph, false);
        let base = out.find("s.albedo = n_b.rgb").unwrap();
        let metallic = out.find("s.metallic = n_m.r").unwrap();
        let roughness = out.find("s.roughness = n_r.r").unwrap();
        let emissive = out.find("s.emissive = n_e.rgb").unwrap();
        assert!(base < metallic && metallic < roughness && roughness < emissive);
    }

    #[test]
    fn texture_slot_index_mapping_matches_per_slot() {
        // Preview context: only x/y/z/w on `mat.tex`.
        assert_eq!(texture_slot_index("albedo", false), "mat.tex.x");
        assert_eq!(texture_slot_index("metallicRoughness", false), "mat.tex.y");
        assert_eq!(texture_slot_index("mr", false), "mat.tex.y");
        assert_eq!(texture_slot_index("normal", false), "mat.tex.z");
        assert_eq!(texture_slot_index("emissive", false), "mat.tex.w");
        // An unknown slot falls back to albedo (x).
        assert_eq!(texture_slot_index("height", false), "mat.tex.x");
        assert_eq!(texture_slot_index("occlusion", false), "mat.tex.x");

        // Mesh context: tex0 + tex1, with height/occlusion on tex1.
        assert_eq!(texture_slot_index("albedo", true), "m.mat.tex0.x");
        assert_eq!(
            texture_slot_index("metallicRoughness", true),
            "m.mat.tex0.y"
        );
        assert_eq!(texture_slot_index("mr", true), "m.mat.tex0.y");
        assert_eq!(texture_slot_index("normal", true), "m.mat.tex0.z");
        assert_eq!(texture_slot_index("emissive", true), "m.mat.tex0.w");
        assert_eq!(texture_slot_index("height", true), "m.mat.tex1.x");
        assert_eq!(texture_slot_index("occlusion", true), "m.mat.tex1.y");
        assert_eq!(texture_slot_index("bogus", true), "m.mat.tex0.x");
    }

    #[test]
    fn every_math_node_emits_its_statement() {
        // The math/utility coverage: each emits its dedicated form; an unknown type emits
        // the zero-fill with its name in a comment.
        let cases: &[(&str, &str)] = &[
            ("multiply", "    float4 n_x = n_a * n_b;\n"),
            ("add", "    float4 n_x = n_a + n_b;\n"),
            ("subtract", "    float4 n_x = n_a - n_b;\n"),
            ("divide", "    float4 n_x = n_a / max(n_b, float4(1e-5));\n"),
            ("lerp", "    float4 n_x = lerp(n_a, n_b, n_t);\n"),
            ("saturate", "    float4 n_x = saturate(n_a);\n"),
            ("clamp", "    float4 n_x = saturate(n_a);\n"),
            ("oneMinus", "    float4 n_x = 1.0 - n_a;\n"),
            ("dot", "    float4 n_x = float4(dot(n_a.rgb, n_b.rgb));\n"),
            ("uv", "    float4 n_x = float4(uv, 0.0, 1.0);\n"),
            ("sin", "    float4 n_x = sin(n_a);\n"),
            ("cos", "    float4 n_x = cos(n_a);\n"),
            ("frac", "    float4 n_x = frac(n_a);\n"),
            ("step", "    float4 n_x = step(n_a, n_b);\n"),
            (
                "smoothstep",
                "    float4 n_x = smoothstep(n_a, n_b, n_t);\n",
            ),
            (
                "weird",
                "    float4 n_x = float4(0.0);  // unknown node 'weird'\n",
            ),
        ];
        for (ty, expected) in cases {
            assert_eq!(emit_math_node(ty, "x", "a", "b", "t"), *expected, "{ty}");
        }
    }

    #[test]
    fn constant_only_graph_folds_factors_and_returns_true() {
        let graph = serde_json::json!({
            "nodes": [
                { "id": "bc", "type": "constant", "props": { "value": [0.1, 0.2, 0.3, 0.4] } },
                { "id": "em", "type": "constant", "props": { "value": [1.0, 2.0, 3.0] } },
                { "id": "mt", "type": "constant", "props": { "value": 0.6 } },
                { "id": "rg", "type": "constant", "props": { "value": [0.25] } },
                { "id": "es", "type": "constant", "props": { "value": 4.0 } },
                { "id": "out", "type": "materialOutput" }
            ],
            "edges": [
                { "from": ["bc", "o"], "to": ["out", "baseColor"] },
                { "from": ["em", "o"], "to": ["out", "emissive"] },
                { "from": ["mt", "o"], "to": ["out", "metallic"] },
                { "from": ["rg", "o"], "to": ["out", "roughness"] },
                { "from": ["es", "o"], "to": ["out", "emissiveStrength"] }
            ]
        });
        let mut material = default_material_asset();
        assert!(lower_graph_to_params(&graph, &mut material));
        assert_eq!(material.base_color, Vec4::new(0.1, 0.2, 0.3, 0.4));
        assert_eq!(material.emissive, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(material.metallic, 0.6);
        assert_eq!(material.roughness, 0.25);
        assert_eq!(material.emissive_strength, 4.0);
    }

    #[test]
    fn texture_only_graph_folds_all_five_slots_and_returns_true() {
        let graph = serde_json::json!({
            "nodes": [
                { "id": "a", "type": "texture", "props": { "asset": "1001" } },
                { "id": "n", "type": "texture", "props": { "asset": 1002 } },
                { "id": "e", "type": "texture", "props": { "asset": "1003" } },
                { "id": "o", "type": "texture", "props": { "asset": "1004" } },
                { "id": "h", "type": "texture", "props": { "asset": "1005" } },
                { "id": "out", "type": "materialOutput" }
            ],
            "edges": [
                { "from": ["a", "o"], "to": ["out", "baseColor"] },
                { "from": ["n", "o"], "to": ["out", "normal"] },
                { "from": ["e", "o"], "to": ["out", "emissive"] },
                { "from": ["o", "o"], "to": ["out", "roughness"] },
                { "from": ["h", "o"], "to": ["out", "height"] }
            ]
        });
        let mut material = default_material_asset();
        assert!(lower_graph_to_params(&graph, &mut material));
        assert_eq!(material.albedo_texture, Uuid(1001));
        assert_eq!(material.normal_texture, Uuid(1002));
        assert_eq!(material.emissive_texture, Uuid(1003));
        // roughness (and metallic) fold to the ORM slot.
        assert_eq!(material.orm_texture, Uuid(1004));
        assert_eq!(material.height_texture, Uuid(1005));
    }

    #[test]
    fn texture_metallic_channel_also_folds_to_orm() {
        let graph = serde_json::json!({
            "nodes": [
                { "id": "o", "type": "texture", "props": { "asset": "55" } },
                { "id": "out", "type": "materialOutput" }
            ],
            "edges": [{ "from": ["o", "x"], "to": ["out", "metallic"] }]
        });
        let mut material = default_material_asset();
        assert!(lower_graph_to_params(&graph, &mut material));
        assert_eq!(material.orm_texture, Uuid(55));
    }

    #[test]
    fn graph_with_a_math_node_returns_false() {
        // The caller (clone, fold, keep-if-true) discards the partial mutation; the
        // function's contract is the false return.
        let mut material = default_material_asset();
        assert!(!lower_graph_to_params(&small_graph(), &mut material));
    }

    #[test]
    fn graph_with_no_material_output_returns_false() {
        let graph = serde_json::json!({
            "nodes": [{ "id": "c", "type": "constant", "props": { "value": [1, 1, 1, 1] } }],
            "edges": []
        });
        let mut material = default_material_asset();
        assert!(!lower_graph_to_params(&graph, &mut material));
    }

    #[test]
    fn non_object_graph_returns_false() {
        let mut material = default_material_asset();
        assert!(!lower_graph_to_params(&Value::Null, &mut material));
        assert!(!lower_graph_to_params(
            &serde_json::json!([1, 2, 3]),
            &mut material
        ));
        // An object missing nodes/edges arrays is not foldable.
        assert!(!lower_graph_to_params(
            &serde_json::json!({}),
            &mut material
        ));
    }

    #[test]
    fn unrecognized_constant_channel_forces_codegen() {
        // A constant wired to an unknown channel flips foldable false.
        let graph = serde_json::json!({
            "nodes": [
                { "id": "c", "type": "constant", "props": { "value": [1, 1, 1, 1] } },
                { "id": "out", "type": "materialOutput" }
            ],
            "edges": [{ "from": ["c", "o"], "to": ["out", "bizarre"] }]
        });
        let mut material = default_material_asset();
        assert!(!lower_graph_to_params(&graph, &mut material));
    }

    #[test]
    fn unresolved_edge_source_forces_codegen() {
        // An edge whose source node id is not in the graph flips foldable false.
        let graph = serde_json::json!({
            "nodes": [{ "id": "out", "type": "materialOutput" }],
            "edges": [{ "from": ["ghost", "o"], "to": ["out", "baseColor"] }]
        });
        let mut material = default_material_asset();
        assert!(!lower_graph_to_params(&graph, &mut material));
    }

    #[test]
    fn edges_not_targeting_output_are_ignored_by_fold() {
        // An edge between two non-output nodes does not affect foldability; a constant
        // baseColor edge to the output still folds.
        let graph = serde_json::json!({
            "nodes": [
                { "id": "c", "type": "constant", "props": { "value": [0.5, 0.5, 0.5, 1.0] } },
                { "id": "k", "type": "constant", "props": { "value": [1, 1, 1, 1] } },
                { "id": "out", "type": "materialOutput" }
            ],
            "edges": [
                { "from": ["k", "o"], "to": ["c", "ignored"] },
                { "from": ["c", "o"], "to": ["out", "baseColor"] }
            ]
        });
        let mut material = default_material_asset();
        assert!(lower_graph_to_params(&graph, &mut material));
        assert_eq!(material.base_color, Vec4::new(0.5, 0.5, 0.5, 1.0));
    }
}
