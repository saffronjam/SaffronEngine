//! OpenUSD `PointInstancer` interchange, over the USDA text form.
//!
//! A `PointInstancer` is USD's answer to the same problem a Houdini scatter solves: many placements of
//! a few prototypes, held in parallel arrays rather than in the prim hierarchy. Reading it needs no
//! USD runtime — the text form states the arrays directly, and the subset a plant scatter uses is
//! small and stable.
//!
//! Two USD conventions are easy to get wrong and are handled explicitly. A `quath`/`quatf`
//! orientation is **WXYZ**, not the XYZW every other seam here uses. And `invisibleIds` is a sparse
//! mask over the instancer's own `ids`, not over array positions, so an instancer that reorders its
//! arrays keeps masking the same instances.

use std::collections::{BTreeMap, BTreeSet};

use saffron_core::Uuid;

use crate::{Error, PointInstance, PointInterchange, PointPrototype, Result};

/// Attributes of a `PointInstancer` this vocabulary expresses.
const KNOWN: [&str; 6] = [
    "positions",
    "orientations",
    "scales",
    "protoIndices",
    "ids",
    "invisibleIds",
];

/// Reads every `PointInstancer` in a USDA document as one interchange payload.
///
/// Prototypes are addressed by the last component of each relationship target path, which is the name
/// an artist sees in the stage tree. Every attribute outside the expressible set is reported.
///
/// # Errors
///
/// [`Error::ArtifactFormat`] when a prototype path resolves to no plant family, when `positions` is
/// absent from an instancer, or when the document declares no `PointInstancer` at all.
pub fn read_usd_point_instancers(
    text: &str,
    families: &BTreeMap<String, Uuid>,
) -> Result<PointInterchange> {
    let field = |name: &str| Error::ArtifactFormat {
        format: "usd point instancer",
        field: name.to_owned(),
    };
    let mut prototypes: Vec<PointPrototype> = Vec::new();
    let mut instances: Vec<PointInstance> = Vec::new();
    let mut unsupported = BTreeSet::new();
    let mut found = false;

    for body in point_instancer_bodies(text) {
        found = true;
        let statements = statements(&body);
        for name in statements.keys() {
            if !KNOWN.contains(&name.as_str()) && name != "prototypes" {
                unsupported.insert(name.clone());
            }
        }
        let positions = statements
            .get("positions")
            .map(|value| tuples(value))
            .ok_or_else(|| field("positions"))?;
        let orientations = statements.get("orientations").map(|value| tuples(value));
        let scales = statements.get("scales").map(|value| tuples(value));
        let proto_indices = statements
            .get("protoIndices")
            .map(|value| scalars(value))
            .unwrap_or_default();
        let ids = statements.get("ids").map(|value| scalars(value));
        let invisible: BTreeSet<i64> = statements
            .get("invisibleIds")
            .map(|value| {
                scalars(value)
                    .into_iter()
                    .map(|value| value as i64)
                    .collect()
            })
            .unwrap_or_default();

        // Each instancer contributes its own prototypes; indices shift by what came before so one
        // payload can carry several instancers.
        let base = u32::try_from(prototypes.len()).map_err(|_| field("prototypes"))?;
        let paths = statements
            .get("prototypes")
            .map(|value| relationship_targets(value))
            .unwrap_or_default();
        for path in &paths {
            let name = path.rsplit('/').next().unwrap_or(path).to_owned();
            let family = *families
                .get(&name)
                .ok_or_else(|| field("prototypes.family"))?;
            prototypes.push(PointPrototype { name, family });
        }
        if paths.is_empty() {
            return Err(field("prototypes"));
        }

        for (index, position) in positions.iter().enumerate() {
            let stable_id = ids
                .as_ref()
                .and_then(|values| values.get(index))
                .map_or(index as i64, |value| *value as i64);
            let local = proto_indices.get(index).map_or(0, |value| *value as u32);
            let prototype = base
                .checked_add(local)
                .filter(|slot| (*slot as usize) < prototypes.len())
                .ok_or_else(|| field("protoIndices"))?;
            instances.push(PointInstance {
                prototype,
                position: lanes3(position, 0.0),
                orientation: orientations
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .map_or([0.0, 0.0, 0.0, 1.0], |lanes| wxyz_to_xyzw(lanes)),
                scale: scales
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .map_or([1.0; 3], |lanes| lanes3(lanes, 1.0)),
                stable_id: stable_id.unsigned_abs(),
                // A masked instance is not a plant. It keeps its identity in the source, so
                // unmasking it later brings back the same plant rather than a new one.
                active: !invisible.contains(&stable_id),
            });
        }
    }
    if !found {
        return Err(field("PointInstancer"));
    }
    Ok(PointInterchange {
        prototypes,
        instances,
        unsupported: unsupported.into_iter().collect(),
    })
}

/// Attributes of a `Skeleton` this vocabulary expresses.
const KNOWN_SKELETON: [&str; 3] = ["joints", "bindTransforms", "restTransforms"];

/// One `UsdSkel` skeleton read from a USDA stage.
#[derive(Clone, Debug, PartialEq)]
pub struct UsdSkeleton {
    /// The `Skeleton` prim's name, which is what an artist sees in the stage tree.
    pub name: String,
    /// The enclosing `SkelRoot` prim's name, absent when the skeleton sits outside one.
    ///
    /// USD requires a `SkelRoot` ancestor for a bound skeleton, so its absence is worth reporting
    /// rather than silently accepting: a skeleton outside one binds nothing.
    pub skel_root: Option<String>,
    /// Joints in the order `joints` declares, which is the order every parallel array uses.
    pub joints: Vec<UsdJoint>,
}

/// One joint of a `UsdSkel` skeleton.
#[derive(Clone, Debug, PartialEq)]
pub struct UsdJoint {
    /// The joint's full path token, as authored.
    pub path: String,
    /// Index of the parent joint in the same skeleton, absent for a root.
    ///
    /// USD states the hierarchy in the path tokens rather than in a parent array, so this is
    /// derived: a joint's parent is the longest declared path that is a proper prefix of its own.
    pub parent: Option<usize>,
    /// Rest transform, row-major, or the identity when the skeleton declares none.
    pub rest: [f64; 16],
    /// World-space bind transform, row-major, or the identity when the skeleton declares none.
    pub bind: [f64; 16],
}

/// Reads every `UsdSkel` skeleton in a USDA document.
///
/// The structural half of a plant import: a `PointInstancer` says where copies stand, and a
/// skeleton says how one bends. Both are read from the same text form, because the arrays a plant
/// needs are stated directly and a second reader over a USD runtime would be a second truth about
/// the same file.
///
/// Attributes outside the expressible set are reported per skeleton rather than dropped.
///
/// # Errors
///
/// [`Error::ArtifactFormat`] when a skeleton's parallel arrays disagree in length, or when a joint
/// path is empty.
pub fn read_usd_skeletons(text: &str) -> Result<(Vec<UsdSkeleton>, Vec<String>)> {
    let field = |name: &str| Error::ArtifactFormat {
        format: "usd skeleton",
        field: name.to_owned(),
    };
    // A skeleton's `SkelRoot` is whichever root's body contains it; USD requires the ancestor for
    // a bound skeleton, so the enclosing name is worth carrying.
    let roots = prim_bodies(text, "SkelRoot");
    let mut skeletons = Vec::new();
    let mut unsupported = BTreeSet::new();
    for (name, body) in prim_bodies(text, "Skeleton") {
        let statements = statements(&body);
        for attribute in statements.keys() {
            if !KNOWN_SKELETON.contains(&attribute.as_str()) {
                unsupported.insert(attribute.clone());
            }
        }
        let paths: Vec<String> = statements
            .get("joints")
            .map(|value| quoted_tokens(value))
            .unwrap_or_default();
        if paths.iter().any(|path| path.trim().is_empty()) {
            return Err(field("joints"));
        }
        let rest = statements
            .get("restTransforms")
            .map(|value| matrices(value));
        let bind = statements
            .get("bindTransforms")
            .map(|value| matrices(value));
        for (attribute, values) in [("restTransforms", &rest), ("bindTransforms", &bind)] {
            if values
                .as_ref()
                .is_some_and(|rows| rows.len() != paths.len())
            {
                return Err(field(attribute));
            }
        }
        let joints = paths
            .iter()
            .enumerate()
            .map(|(index, path)| UsdJoint {
                path: path.clone(),
                parent: parent_joint(&paths, index),
                rest: rest
                    .as_ref()
                    .and_then(|rows| rows.get(index).copied())
                    .unwrap_or(IDENTITY4),
                bind: bind
                    .as_ref()
                    .and_then(|rows| rows.get(index).copied())
                    .unwrap_or(IDENTITY4),
            })
            .collect();
        skeletons.push(UsdSkeleton {
            name: name.clone(),
            skel_root: roots
                .iter()
                .find(|(_, root_body)| root_body.contains(&format!("\"{name}\"")))
                .map(|(root, _)| root.clone()),
            joints,
        });
    }
    Ok((skeletons, unsupported.into_iter().collect()))
}

/// The row-major identity, the transform a skeleton that declares none implies.
const IDENTITY4: [f64; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

/// The index of `index`'s parent: the longest declared path that is a proper prefix of its own.
///
/// USD states a skeleton's hierarchy in the path tokens rather than in a parent array, and a
/// prefix comparison alone would make `/Root/Hip` the parent of `/Root/HipGuard`, so the boundary
/// must fall on a path separator.
fn parent_joint(paths: &[String], index: usize) -> Option<usize> {
    let own = paths.get(index)?.trim_end_matches('/');
    paths
        .iter()
        .enumerate()
        .filter(|(other, path)| {
            let path = path.trim_end_matches('/');
            *other != index
                && path.len() < own.len()
                && own.starts_with(path)
                && own[path.len()..].starts_with('/')
        })
        .max_by_key(|(_, path)| path.len())
        .map(|(other, _)| other)
}

/// The contents of every double-quoted token in a value, in order.
fn quoted_tokens(value: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = value;
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else {
            break;
        };
        found.push(after[..close].to_owned());
        rest = &after[close + 1..];
    }
    found
}

/// The scalars of every innermost parenthesised group, in order.
///
/// The tuple scanner above stops at the first `)`, which is correct for a flat list of points and
/// wrong for a `matrix4d`, whose rows nest inside an outer pair. This one tracks the most recent
/// `(` and emits a group only when nothing nested inside it.
fn leaf_rows(value: &str) -> Vec<Vec<f64>> {
    let mut rows = Vec::new();
    let mut start: Option<usize> = None;
    for (offset, character) in value.char_indices() {
        match character {
            '(' => start = Some(offset + 1),
            ')' => {
                if let Some(open) = start.take() {
                    rows.push(
                        value[open..offset]
                            .split(',')
                            .filter_map(|entry| entry.trim().parse::<f64>().ok())
                            .collect(),
                    );
                }
            }
            _ => {}
        }
    }
    rows
}

/// Every `matrix4d` in a value, row-major as USD writes them.
///
/// USD nests a matrix as four parenthesised rows inside one outer pair, so the row scanner yields
/// the rows and every four consecutive ones are a matrix. A partial trailing group is dropped
/// rather than padded — a half-read transform is worse than a missing one.
fn matrices(value: &str) -> Vec<[f64; 16]> {
    let rows = leaf_rows(value);
    let mut found = Vec::new();
    for chunk in rows.chunks(4) {
        if chunk.len() != 4 || chunk.iter().any(|row| row.len() != 4) {
            break;
        }
        let mut matrix = [0.0; 16];
        for (index, row) in chunk.iter().enumerate() {
            matrix[index * 4..index * 4 + 4].copy_from_slice(&row[..4]);
        }
        found.push(matrix);
    }
    found
}

/// Writes one interchange payload as a USDA `PointInstancer`.
///
/// The arrays a reader needs and nothing else: prototypes as relationship targets under the
/// instancer, then positions, orientations in USD's WXYZ order, scales, prototype indices, and ids.
#[must_use]
pub fn write_usd_point_instancer(payload: &PointInterchange) -> String {
    let tuple3 = |lanes: [f64; 3]| format!("({}, {}, {})", lanes[0], lanes[1], lanes[2]);
    let mut text = String::from("#usda 1.0\n(\n    upAxis = \"Y\"\n)\n\n");
    text.push_str("def PointInstancer \"Plants\"\n{\n");
    text.push_str("    rel prototypes = [\n");
    for prototype in &payload.prototypes {
        text.push_str(&format!("        </Plants/{}>,\n", prototype.name));
    }
    text.push_str("    ]\n");
    for prototype in &payload.prototypes {
        text.push_str(&format!(
            "    def Xform \"{}\"\n    {{\n    }}\n",
            prototype.name
        ));
    }
    let join = |values: Vec<String>| values.join(", ");
    text.push_str(&format!(
        "    point3f[] positions = [{}]\n",
        join(
            payload
                .instances
                .iter()
                .map(|instance| tuple3(instance.position))
                .collect()
        )
    ));
    text.push_str(&format!(
        "    quatf[] orientations = [{}]\n",
        join(
            payload
                .instances
                .iter()
                .map(|instance| {
                    let lanes = instance.orientation;
                    format!("({}, {}, {}, {})", lanes[3], lanes[0], lanes[1], lanes[2])
                })
                .collect()
        )
    ));
    text.push_str(&format!(
        "    float3[] scales = [{}]\n",
        join(
            payload
                .instances
                .iter()
                .map(|instance| tuple3(instance.scale))
                .collect()
        )
    ));
    text.push_str(&format!(
        "    int[] protoIndices = [{}]\n",
        join(
            payload
                .instances
                .iter()
                .map(|instance| instance.prototype.to_string())
                .collect()
        )
    ));
    text.push_str(&format!(
        "    int64[] ids = [{}]\n",
        join(
            payload
                .instances
                .iter()
                .map(|instance| instance.stable_id.to_string())
                .collect()
        )
    ));
    let masked: Vec<String> = payload
        .instances
        .iter()
        .filter(|instance| !instance.active)
        .map(|instance| instance.stable_id.to_string())
        .collect();
    if !masked.is_empty() {
        text.push_str(&format!("    int64[] invisibleIds = [{}]\n", join(masked)));
    }
    text.push_str("}\n");
    text
}

/// The brace-balanced body of every `def PointInstancer` in the document.
fn point_instancer_bodies(text: &str) -> Vec<String> {
    prim_bodies(text, "PointInstancer")
        .into_iter()
        .map(|(_, body)| body)
        .collect()
}

/// The `(name, body)` of every `def <kind>` prim in a USDA document, outermost first.
fn prim_bodies(text: &str, kind: &str) -> Vec<(String, String)> {
    let needle = format!("def {kind}");
    let mut bodies = Vec::new();
    let mut cursor = 0;
    while let Some(found) = text[cursor..].find(needle.as_str()) {
        let start = cursor + found;
        let Some(open) = text[start..].find('{') else {
            break;
        };
        let open = start + open;
        let mut depth = 0_i32;
        let mut end = open;
        for (offset, character) in text[open..].char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        if end <= open {
            break;
        }
        // The prim's name is the quoted token between `def <kind>` and its body.
        let header = &text[start + needle.len()..open];
        let name = header
            .split('"')
            .nth(1)
            .unwrap_or_default()
            .trim()
            .to_owned();
        bodies.push((name, text[open + 1..end].to_owned()));
        cursor = end;
    }
    bodies
}

/// The `name = value` statements of one prim body, skipping nested prims.
fn statements(body: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let mut depth = 0_i32;
    let mut statement = String::new();
    let mut brackets = 0_i32;
    for character in body.chars() {
        match character {
            '{' => {
                depth += 1;
                statement.clear();
                continue;
            }
            '}' => {
                depth -= 1;
                statement.clear();
                continue;
            }
            _ => {}
        }
        if depth > 0 {
            continue;
        }
        if character == '[' {
            brackets += 1;
        }
        if character == ']' {
            brackets -= 1;
        }
        if character == '\n' && brackets <= 0 {
            record(&statement, &mut found);
            statement.clear();
        } else {
            statement.push(character);
        }
    }
    record(&statement, &mut found);
    found
}

/// Records one statement under its attribute name, dropping the declared type prefix.
fn record(statement: &str, into: &mut BTreeMap<String, String>) {
    let Some((left, right)) = statement.split_once('=') else {
        return;
    };
    // `point3f[] positions`, `rel prototypes`, `uniform token foo` — the attribute is the last word.
    let Some(name) = left.split_whitespace().next_back() else {
        return;
    };
    if name.is_empty() {
        return;
    }
    into.insert(name.to_owned(), right.trim().to_owned());
}

/// Parenthesized tuples in one array value.
fn tuples(value: &str) -> Vec<Vec<f64>> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(open) = value[cursor..].find('(') {
        let open = cursor + open;
        let Some(close) = value[open..].find(')') else {
            break;
        };
        let close = open + close;
        found.push(
            value[open + 1..close]
                .split(',')
                .filter_map(|lane| lane.trim().parse::<f64>().ok())
                .collect(),
        );
        cursor = close + 1;
    }
    found
}

/// Bare numbers in one array value.
fn scalars(value: &str) -> Vec<f64> {
    value
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(|entry| entry.trim().parse::<f64>().ok())
        .collect()
}

/// Prim paths inside `</…>` relationship targets.
fn relationship_targets(value: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(open) = value[cursor..].find('<') {
        let open = cursor + open;
        let Some(close) = value[open..].find('>') else {
            break;
        };
        let close = open + close;
        found.push(value[open + 1..close].trim().to_owned());
        cursor = close + 1;
    }
    found
}

/// Three lanes of a tuple, padded with `fallback`.
fn lanes3(lanes: &[f64], fallback: f64) -> [f64; 3] {
    std::array::from_fn(|axis| lanes.get(axis).copied().unwrap_or(fallback))
}

/// A USD WXYZ quaternion as the XYZW form every other seam here uses.
fn wxyz_to_xyzw(lanes: &[f64]) -> [f64; 4] {
    [
        lanes.get(1).copied().unwrap_or(0.0),
        lanes.get(2).copied().unwrap_or(0.0),
        lanes.get(3).copied().unwrap_or(0.0),
        lanes.first().copied().unwrap_or(1.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAGE: &str = r#"#usda 1.0
(
    upAxis = "Y"
)

def PointInstancer "Trees"
{
    rel prototypes = [
        </Trees/oak>,
        </Trees/birch>,
    ]
    def Xform "oak"
    {
        double3 xformOp:translate = (0, 0, 0)
    }
    point3f[] positions = [(0, 0, 0), (4, 0, 2), (-3, 0, 6)]
    quatf[] orientations = [(1, 0, 0, 0), (0.70710678, 0, 0.70710678, 0), (1, 0, 0, 0)]
    float3[] scales = [(1, 1, 1), (2, 2, 2), (1, 1, 1)]
    int[] protoIndices = [0, 1, 0]
    int64[] ids = [11, 22, 33]
    int64[] invisibleIds = [22]
    color3f[] primvars:displayColor = [(1, 0, 0)]
}
"#;

    fn families() -> BTreeMap<String, Uuid> {
        BTreeMap::from([("oak".to_owned(), Uuid(77)), ("birch".to_owned(), Uuid(78))])
    }

    /// The arrays a plant scatter uses read exactly, including USD's WXYZ orientation order and its
    /// id-keyed sparse mask.
    #[test]
    fn a_point_instancer_reads_its_arrays() {
        let payload = read_usd_point_instancers(STAGE, &families()).expect("the stage reads");
        assert_eq!(
            payload
                .prototypes
                .iter()
                .map(|prototype| prototype.name.as_str())
                .collect::<Vec<_>>(),
            vec!["oak", "birch"]
        );
        assert_eq!(payload.instances.len(), 3);
        assert_eq!(payload.instances[1].prototype, 1);
        assert!((payload.instances[1].position[0] - 4.0).abs() < 1.0e-9);
        assert!((payload.instances[1].scale[2] - 2.0).abs() < 1.0e-9);
        // WXYZ in, XYZW out: the source's leading w lane ends up last.
        let turned = payload.instances[1].orientation;
        assert!(turned[0].abs() < 1.0e-9 && turned[2].abs() < 1.0e-9);
        assert!((turned[1] - 0.70710678).abs() < 1.0e-6);
        assert!((turned[3] - 0.70710678).abs() < 1.0e-6);
        // The mask is keyed by id, not by array position.
        assert_eq!(
            payload
                .instances
                .iter()
                .map(|instance| instance.active)
                .collect::<Vec<_>>(),
            vec![true, false, true]
        );
        assert_eq!(payload.instances[2].stable_id, 33);
        // A display colour has nowhere to go in the canonical vocabulary.
        assert_eq!(
            payload.unsupported,
            vec!["primvars:displayColor".to_owned()]
        );
    }

    /// A payload written as a `PointInstancer` reads back as the same payload, mask included.
    #[test]
    fn a_point_instancer_round_trips() {
        let source = read_usd_point_instancers(STAGE, &families()).expect("read");
        let text = write_usd_point_instancer(&source);
        let decoded = read_usd_point_instancers(&text, &families()).expect("read back");
        assert_eq!(decoded.prototypes, source.prototypes);
        assert_eq!(decoded.instances.len(), source.instances.len());
        for (before, after) in source.instances.iter().zip(&decoded.instances) {
            assert_eq!(before.prototype, after.prototype);
            assert_eq!(before.stable_id, after.stable_id);
            assert_eq!(before.active, after.active);
            for lane in 0..3 {
                assert!((before.position[lane] - after.position[lane]).abs() < 1.0e-9);
                assert!((before.scale[lane] - after.scale[lane]).abs() < 1.0e-9);
            }
            for lane in 0..4 {
                assert!((before.orientation[lane] - after.orientation[lane]).abs() < 1.0e-9);
            }
        }
        assert!(decoded.unsupported.is_empty());
    }

    /// Masking one instance takes exactly that plant away and leaves every other plant's identity
    /// untouched: identities come from the source's ids, not from a slot, so nothing renumbers.
    #[test]
    fn masking_one_instance_leaks_no_other_identity() {
        let scalar = |value: f64| saffron_spatial::DecisionScalar::from_f64(value).unwrap();
        let dimensions = BTreeMap::from([
            (
                77,
                crate::PlantDimensions {
                    height: scalar(6.0),
                    trunk_radius: scalar(0.2),
                    crown_radius: [scalar(2.0); 2],
                    root_radius: [scalar(1.0); 2],
                    local_bounds_min: [scalar(-2.0), scalar(0.0), scalar(-2.0)],
                    local_bounds_max: [scalar(2.0), scalar(6.0), scalar(2.0)],
                },
            ),
            (
                78,
                crate::PlantDimensions {
                    height: scalar(4.0),
                    trunk_radius: scalar(0.1),
                    crown_radius: [scalar(1.0); 2],
                    root_radius: [scalar(1.0); 2],
                    local_bounds_min: [scalar(-1.0), scalar(0.0), scalar(-1.0)],
                    local_bounds_max: [scalar(1.0), scalar(4.0), scalar(1.0)],
                },
            ),
        ]);
        // The stage masks id 22. Unmasking it must add exactly one plant, changing no other.
        let masked = read_usd_point_instancers(STAGE, &families()).expect("read");
        let unmasked_text = STAGE.replace("int64[] invisibleIds = [22]", "");
        let unmasked = read_usd_point_instancers(&unmasked_text, &families()).expect("read");

        let of = |payload: &PointInterchange| -> Vec<(u64, u128)> {
            crate::interchange_to_anchors(payload, 9, &dimensions)
                .expect("anchors")
                .into_iter()
                .map(|anchor| {
                    (
                        anchor.point.candidate,
                        u128::from_be_bytes(anchor.id.bytes()),
                    )
                })
                .collect()
        };
        let before = of(&masked);
        let after = of(&unmasked);
        assert_eq!(before.len(), 2);
        assert_eq!(after.len(), 3);
        // Every identity the masked read produced is present, unchanged, in the unmasked one.
        for entry in &before {
            assert!(after.contains(entry), "identity {entry:?} moved");
        }
        // And the one that appeared is the masked instance's own.
        let appeared: Vec<u64> = after
            .iter()
            .filter(|entry| !before.contains(entry))
            .map(|entry| entry.0)
            .collect();
        assert_eq!(appeared, vec![22]);
    }

    /// A document with no instancer, an unbound prototype, or no positions is refused rather than
    /// read as an empty scatter.
    #[test]
    fn a_malformed_stage_is_refused() {
        assert!(read_usd_point_instancers("#usda 1.0\n", &families()).is_err());
        assert!(read_usd_point_instancers(STAGE, &BTreeMap::new()).is_err());
        let without = STAGE.replace("point3f[] positions", "point3f[] elsewhere");
        assert!(read_usd_point_instancers(&without, &families()).is_err());
    }

    /// A `SkelRoot` holding one skeleton whose joints span three levels, with a sibling whose name
    /// begins with another joint's — the case a plain prefix test gets wrong.
    const SKEL_STAGE: &str = r#"#usda 1.0
(
    upAxis = "Y"
)

def SkelRoot "BirchRig"
{
    def Skeleton "Birch"
    {
        uniform token[] joints = ["Root", "Root/Trunk", "Root/TrunkGuard", "Root/Trunk/Branch"]
        uniform matrix4d[] restTransforms = [
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1) ),
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 2, 0, 1) ),
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (1, 2, 0, 1) ),
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 4, 0, 1) )
        ]
        uniform token[] blendShapes = ["wilt"]
    }
}
"#;

    #[test]
    fn a_usd_skeleton_reads_its_joints_transforms_and_enclosing_root() {
        let (skeletons, unsupported) = read_usd_skeletons(SKEL_STAGE).expect("the stage reads");
        assert_eq!(skeletons.len(), 1);
        let skeleton = &skeletons[0];
        assert_eq!(skeleton.name, "Birch");
        assert_eq!(skeleton.skel_root.as_deref(), Some("BirchRig"));
        assert_eq!(skeleton.joints.len(), 4);
        // The rest transform is row-major as USD writes it, so the translation is the last row.
        assert_eq!(skeleton.joints[1].rest[13], 2.0);
        assert_eq!(skeleton.joints[3].rest[13], 4.0);
        // A skeleton that declares no bind transforms implies the identity rather than zeros.
        assert_eq!(skeleton.joints[0].bind, IDENTITY4);
        // Every attribute outside the expressible set is reported rather than dropped.
        assert_eq!(unsupported, vec!["blendShapes".to_owned()]);
    }

    #[test]
    fn a_joint_parent_is_a_path_boundary_not_a_string_prefix() {
        // `Root/Trunk` is a string prefix of `Root/TrunkGuard`, and treating it as the parent
        // would hang a sibling limb off the wrong joint — an error that survives every count and
        // length check and only shows up as geometry bending the wrong way.
        let (skeletons, _) = read_usd_skeletons(SKEL_STAGE).expect("the stage reads");
        let joints = &skeletons[0].joints;
        assert_eq!(joints[0].parent, None, "the root has no parent");
        assert_eq!(joints[1].parent, Some(0), "Root/Trunk hangs off Root");
        assert_eq!(joints[2].parent, Some(0), "Root/TrunkGuard hangs off Root");
        assert_eq!(joints[3].parent, Some(1), "the branch hangs off the trunk");
    }

    #[test]
    fn a_skeleton_with_mismatched_arrays_is_refused() {
        // The parallel arrays are the whole contract. A short one read positionally would bind
        // joints to transforms that belong to other joints.
        let short = SKEL_STAGE.replace(
            "            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 4, 0, 1) )\n",
            "",
        );
        assert!(read_usd_skeletons(&short).is_err());
    }

    #[test]
    fn a_stage_with_no_skeleton_reads_as_empty_rather_than_failing() {
        // A point scatter with no rig is ordinary, not broken.
        let (skeletons, unsupported) = read_usd_skeletons(STAGE).expect("a rigless stage reads");
        assert!(skeletons.is_empty());
        assert!(unsupported.is_empty());
    }
}
