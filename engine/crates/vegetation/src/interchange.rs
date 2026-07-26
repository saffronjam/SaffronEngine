//! Point interchange with digital-content-creation tools.
//!
//! A studio's plant placement often starts somewhere else — a Houdini scatter, a layout published as
//! instanced points. Interchange brings those points in as authored anchors and writes them back out,
//! through the canonical point schema and nothing else. There is no alternate runtime object for an
//! imported point: it becomes an [`ExplicitPlantAnchor`] like any hand-placed plant, or it is
//! rejected.
//!
//! Two rules make a round trip safe. A source attribute this vocabulary cannot express is
//! **reported**, never silently kept — a retained blob would be a second truth the runtime cannot
//! read. And an instance's stable identity survives: re-importing an updated scatter re-addresses the
//! same plants rather than duplicating them, so authored overrides keyed to those identities hold.

use std::collections::BTreeMap;

use glam::DVec3;
use saffron_core::Uuid;
use saffron_json::{Value as JsonValue, json};
use saffron_spatial::{
    DecisionScalar, QuantizedOrientation, UnitInterval, WorldBounds, WorldPosition,
};

use crate::{
    Error, ExplicitPlantAnchor, InteractionPolicy, PlantFlags, PlantId, PlantLifecycle, PlantPoint,
    Result,
};

/// Instances one interchange payload may carry. A bound, not a budget: a scatter past it is a
/// mistake caught at the seam rather than a chunk commit that never finishes.
pub const MAX_INTERCHANGE_INSTANCES: usize = 1 << 20;

/// One prototype an interchange payload places.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointPrototype {
    /// Source-facing prototype name, which is how a DCC tool addresses it.
    pub name: String,
    /// The plant family it resolves to.
    pub family: Uuid,
}

/// One placed instance, in source units and axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointInstance {
    /// Index into the payload's prototypes.
    pub prototype: u32,
    /// World position in metres.
    pub position: [f64; 3],
    /// Orientation as an XYZW quaternion.
    pub orientation: [f64; 4],
    /// Non-uniform scale.
    pub scale: [f64; 3],
    /// Source-stable identity, which is what makes a re-import address the same plants.
    pub stable_id: u64,
    /// Whether the source marks the instance active. A sparse mask deactivates rather than deletes,
    /// so an inactive instance keeps its identity and comes back when the mask changes.
    pub active: bool,
}

/// What one interchange payload carries, and what it could not express.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PointInterchange {
    /// Prototypes in source order.
    pub prototypes: Vec<PointPrototype>,
    /// Instances in source order.
    pub instances: Vec<PointInstance>,
    /// Source attributes this vocabulary cannot express, in canonical order. Reported to the caller
    /// and never retained: an unreadable blob riding along would be a second truth.
    pub unsupported: Vec<String>,
}

impl PointInterchange {
    /// Active instances, which is what becomes an anchor.
    pub fn active(&self) -> impl Iterator<Item = &PointInstance> + '_ {
        self.instances.iter().filter(|instance| instance.active)
    }
}

/// The identity one source instance owns, stable across re-imports.
///
/// Derived from the authored layer and the source's own stable id, so the same scatter point is the
/// same plant every time it arrives — which is what lets an authored override keyed to it survive.
///
/// # Errors
///
/// [`Error::NumericOverflow`] when the derived payload is not a usable explicit identity.
pub fn interchange_plant_id(layer: u128, stable_id: u64) -> Result<PlantId> {
    let mut payload = [0_u8; 16];
    let mixed = crate::ContentHash::of(
        &[
            layer.to_be_bytes().as_slice(),
            stable_id.to_be_bytes().as_slice(),
        ]
        .concat(),
    );
    payload.copy_from_slice(&mixed.bytes()[..16]);
    // A hash can in principle be all zeroes, which is not a usable identity.
    if payload == [0; 16] {
        payload[15] = 1;
    }
    PlantId::explicit(payload).map_err(|_| Error::NumericOverflow)
}

/// Turns an interchange payload into authored anchors.
///
/// Every instance normalizes through the canonical point vocabulary: metres quantize to world ticks,
/// the quaternion quantizes to signed normalized lanes, and the bounds come from the family's own
/// dimensions rather than the source, because a source's idea of a bounding box is not the engine's.
///
/// # Errors
///
/// [`Error::ArtifactFormat`] when an instance names no declared prototype, when a value is not
/// finite, or when the payload exceeds [`MAX_INTERCHANGE_INSTANCES`].
pub fn interchange_to_anchors(
    payload: &PointInterchange,
    layer: u128,
    dimensions: &BTreeMap<u64, crate::PlantDimensions>,
) -> Result<Vec<ExplicitPlantAnchor>> {
    let field = |name: &str| Error::ArtifactFormat {
        format: "point interchange",
        field: name.to_owned(),
    };
    if payload.instances.len() > MAX_INTERCHANGE_INSTANCES {
        return Err(field("instances.count"));
    }
    let mut anchors = Vec::new();
    let mut seen = BTreeMap::new();
    for instance in payload.active() {
        let prototype = payload
            .prototypes
            .get(instance.prototype as usize)
            .ok_or_else(|| field("instances.prototype"))?;
        if !instance.position.iter().all(|value| value.is_finite())
            || !instance.orientation.iter().all(|value| value.is_finite())
            || !instance.scale.iter().all(|value| value.is_finite())
        {
            return Err(field("instances.nonFinite"));
        }
        let id = interchange_plant_id(layer, instance.stable_id)?;
        // Two instances claiming one identity would collapse into one plant, silently losing the
        // other; the source has to say which is which.
        if seen.insert(id, ()).is_some() {
            return Err(field("instances.duplicateId"));
        }
        let position = WorldPosition::from_world_meters(DVec3::from_array(instance.position))
            .map_err(|_| field("instances.position"))?;
        let orientation =
            quantize_orientation(instance.orientation).ok_or_else(|| Error::ArtifactFormat {
                format: "point interchange",
                field: "instances.orientation".to_owned(),
            })?;
        let scale = quantize_scale(instance.scale).ok_or_else(|| field("instances.scale"))?;
        let family = dimensions
            .get(&prototype.family.value())
            .ok_or_else(|| field("prototypes.family"))?;
        let bounds = anchor_bounds(&position, family, scale).ok_or_else(|| field("bounds"))?;
        anchors.push(ExplicitPlantAnchor {
            id,
            layer,
            family: prototype.family,
            point: PlantPoint {
                id,
                owner: position.cell(),
                position,
                orientation,
                scale,
                bounds,
                family: prototype.family,
                variation: 0,
                lifecycle: PlantLifecycle::Mature,
                phenotype: 0,
                representation_class: 0,
                // The identity is the deterministic key: an imported point is authored truth, not a
                // sampler draw, so nothing else can reproduce it.
                deterministic_key: u128::from_be_bytes(id.bytes()),
                candidate: instance.stable_id,
                parent: None,
                colony: None,
                ecology_tick: 0,
                health: UnitInterval::ONE,
                moisture: UnitInterval::from_bits(32_768),
                fuel: UnitInterval::from_bits(32_768),
                phenology: UnitInterval::ZERO,
                flags: PlantFlags::AUTHORED,
                interaction_policy: InteractionPolicy::Structural,
                provenance: 0,
                attachment: None,
                surface_projection: [DecisionScalar::from_bits(0); 3],
            },
        });
    }
    anchors.sort_by_key(|anchor| anchor.id);
    Ok(anchors)
}

/// Turns authored anchors back into an interchange payload for a round trip.
///
/// Prototypes come out in canonical family order with the names the caller supplies, and every
/// instance keeps the stable id it arrived with, so a DCC tool sees the same points it sent.
#[must_use]
pub fn anchors_to_interchange(
    anchors: &[ExplicitPlantAnchor],
    names: &BTreeMap<u64, String>,
) -> PointInterchange {
    let mut prototypes: Vec<PointPrototype> = Vec::new();
    let mut index_of: BTreeMap<u64, u32> = BTreeMap::new();
    for anchor in anchors {
        let family = anchor.family.value();
        if index_of.contains_key(&family) {
            continue;
        }
        index_of.insert(family, prototypes.len() as u32);
        prototypes.push(PointPrototype {
            name: names
                .get(&family)
                .cloned()
                .unwrap_or_else(|| format!("family_{family}")),
            family: anchor.family,
        });
    }
    let instances = anchors
        .iter()
        .map(|anchor| PointInstance {
            prototype: index_of[&anchor.family.value()],
            position: anchor.point.position.world_meters().to_array(),
            orientation: dequantize_orientation(anchor.point.orientation),
            scale: anchor.point.scale.map(|lane| lane.to_f64()),
            stable_id: anchor.point.candidate,
            active: true,
        })
        .collect();
    PointInterchange {
        prototypes,
        instances,
        unsupported: Vec::new(),
    }
}

/// Quantizes a source quaternion, normalizing it first: a DCC tool's quaternion is rarely exactly
/// unit, and the canonical form requires one.
fn quantize_orientation(quaternion: [f64; 4]) -> Option<QuantizedOrientation> {
    let length = quaternion
        .iter()
        .map(|lane| lane * lane)
        .sum::<f64>()
        .sqrt();
    if !length.is_finite() || length <= f64::EPSILON {
        return None;
    }
    let mut bits = [0_i16; 4];
    for (lane, value) in quaternion.iter().enumerate() {
        let scaled = (value / length * f64::from(i16::MAX)).round_ties_even();
        bits[lane] = i16::try_from(scaled as i64).ok()?;
    }
    QuantizedOrientation::new(bits).ok()
}

/// The source quaternion one quantized orientation describes.
fn dequantize_orientation(orientation: QuantizedOrientation) -> [f64; 4] {
    orientation
        .bits()
        .map(|lane| f64::from(lane) / f64::from(i16::MAX))
}

/// Quantizes a source scale, refusing a non-positive lane: a plant scaled to nothing is not a plant.
fn quantize_scale(scale: [f64; 3]) -> Option<[DecisionScalar; 3]> {
    let mut lanes = [DecisionScalar::from_bits(0); 3];
    for (lane, value) in scale.iter().enumerate() {
        if *value <= 0.0 {
            return None;
        }
        lanes[lane] = DecisionScalar::from_f64(*value).ok()?;
    }
    Some(lanes)
}

/// Conservative world bounds for one anchor, from the family's own dimensions scaled by the
/// instance.
fn anchor_bounds(
    position: &WorldPosition,
    dimensions: &crate::PlantDimensions,
    scale: [DecisionScalar; 3],
) -> Option<WorldBounds> {
    let center = position.world_meters();
    let extent = |lane: usize, value: DecisionScalar| value.to_f64() * scale[lane].to_f64();
    let width = extent(0, dimensions.crown_radius[0]).max(extent(0, dimensions.root_radius[0]));
    let depth = extent(2, dimensions.crown_radius[1]).max(extent(2, dimensions.root_radius[1]));
    let height = extent(1, dimensions.height);
    let root = extent(1, DecisionScalar::from_f64(1.0).ok()?);
    WorldBounds::from_world_meters(
        DVec3::new(center.x - width, center.y - root, center.z - depth),
        DVec3::new(center.x + width, center.y + height, center.z + depth),
    )
    .ok()
}

/// Reads a Houdini JSON `.geo` point cloud as an interchange payload.
///
/// Houdini writes a paired array rather than an object, so a key is followed by its value. The point
/// attributes this reads are `P` (position), `orient` (quaternion), `scale` or `pscale`, `id`, and
/// `name` or `variant` (the prototype). Every other attribute is reported as unsupported: the
/// canonical point vocabulary has no place to put it, and carrying it as an opaque blob would be a
/// second truth.
///
/// # Errors
///
/// [`Error::ArtifactFormat`] when the document is not a Houdini point cloud, when `P` is absent, or
/// when a named prototype resolves to no plant family.
pub fn read_houdini_points(
    document: &JsonValue,
    families: &BTreeMap<String, Uuid>,
) -> Result<PointInterchange> {
    let field = |name: &str| Error::ArtifactFormat {
        format: "houdini geo",
        field: name.to_owned(),
    };
    let root = document.as_array().ok_or_else(|| field("root"))?;
    let count = paired(root, "pointcount")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| field("pointcount"))? as usize;
    if count > MAX_INTERCHANGE_INSTANCES {
        return Err(field("pointcount.bound"));
    }
    let attributes = paired(root, "attributes")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| field("attributes"))?;
    let point_attributes = paired(attributes, "pointattributes")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| field("pointattributes"))?;

    let mut columns: BTreeMap<String, Vec<Vec<f64>>> = BTreeMap::new();
    let mut names: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut unsupported = Vec::new();
    for attribute in point_attributes {
        let pair = attribute.as_array().ok_or_else(|| field("attribute"))?;
        let header = pair.first().and_then(JsonValue::as_array);
        let body = pair.get(1).and_then(JsonValue::as_array);
        let (Some(header), Some(body)) = (header, body) else {
            return Err(field("attribute.shape"));
        };
        let name = paired(header, "name")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| field("attribute.name"))?
            .to_owned();
        if !matches!(
            name.as_str(),
            "P" | "orient" | "rot" | "scale" | "pscale" | "id" | "name" | "variant"
        ) {
            unsupported.push(name);
            continue;
        }
        if let Some(strings) = houdini_strings(body) {
            names.insert(name, strings);
        } else if let Some(tuples) = houdini_tuples(body) {
            columns.insert(name, tuples);
        } else {
            unsupported.push(name);
        }
    }
    unsupported.sort();
    unsupported.dedup();

    let positions = columns.get("P").ok_or_else(|| field("P"))?;
    if positions.len() < count {
        return Err(field("P.count"));
    }
    let prototype_names = names
        .get("name")
        .or_else(|| names.get("variant"))
        .cloned()
        .unwrap_or_default();

    // Prototypes come out in first-seen order, which is the order the source lists them.
    let mut prototypes: Vec<PointPrototype> = Vec::new();
    let mut index_of: BTreeMap<String, u32> = BTreeMap::new();
    let mut instances = Vec::with_capacity(count);
    for (index, point) in positions.iter().take(count).enumerate() {
        let name = prototype_names
            .get(index)
            .cloned()
            .or_else(|| families.keys().next().cloned())
            .ok_or_else(|| field("name"))?;
        let prototype = match index_of.get(&name) {
            Some(existing) => *existing,
            None => {
                let family = *families.get(&name).ok_or_else(|| field("name.family"))?;
                let slot = u32::try_from(prototypes.len()).map_err(|_| field("prototypes"))?;
                prototypes.push(PointPrototype {
                    name: name.clone(),
                    family,
                });
                index_of.insert(name, slot);
                slot
            }
        };
        let lane = |column: &str, index: usize, default: f64| -> f64 {
            columns
                .get(column)
                .and_then(|values| values.get(index))
                .and_then(|tuple| tuple.first())
                .copied()
                .unwrap_or(default)
        };
        let vector = |column: &str, default: f64| -> [f64; 3] {
            columns
                .get(column)
                .and_then(|values| values.get(index))
                .map(|tuple| {
                    std::array::from_fn(|axis| tuple.get(axis).copied().unwrap_or(default))
                })
                .unwrap_or([default; 3])
        };
        let orientation = columns
            .get("orient")
            .or_else(|| columns.get("rot"))
            .and_then(|values| values.get(index))
            .map(|tuple| std::array::from_fn(|lane| tuple.get(lane).copied().unwrap_or(0.0)))
            .unwrap_or([0.0, 0.0, 0.0, 1.0]);
        // Houdini spells uniform scale `pscale` and non-uniform scale `scale`; both may be present,
        // and the uniform one multiplies the other.
        let uniform = if columns.contains_key("pscale") {
            lane("pscale", index, 1.0)
        } else {
            1.0
        };
        let scale = if columns.contains_key("scale") {
            vector("scale", 1.0).map(|value| value * uniform)
        } else {
            [uniform; 3]
        };
        instances.push(PointInstance {
            prototype,
            position: point
                .iter()
                .copied()
                .chain(std::iter::repeat(0.0))
                .take(3)
                .collect::<Vec<_>>()
                .try_into()
                .map_err(|_| field("P.tuple"))?,
            orientation,
            scale,
            // Without an `id` attribute the point index is the stable identity, which is what
            // Houdini itself falls back to.
            stable_id: columns
                .get("id")
                .and_then(|values| values.get(index))
                .and_then(|tuple| tuple.first())
                .map_or(index as u64, |value| *value as u64),
            active: true,
        });
    }
    Ok(PointInterchange {
        prototypes,
        instances,
        unsupported,
    })
}

/// Writes one interchange payload as a Houdini JSON `.geo` point cloud.
///
/// The same attributes the reader understands, so a payload written here reads back identically.
#[must_use]
pub fn write_houdini_points(payload: &PointInterchange) -> JsonValue {
    let count = payload.instances.len();
    let numeric = |name: &str, size: usize, tuples: Vec<JsonValue>| -> JsonValue {
        json!([
            ["scope", "public", "type", "numeric", "name", name],
            [
                "size",
                size,
                "storage",
                "fpreal64",
                "values",
                ["size", size, "storage", "fpreal64", "tuples", tuples]
            ]
        ])
    };
    let strings = |name: &str, values: Vec<JsonValue>, indices: Vec<JsonValue>| -> JsonValue {
        json!([
            ["scope", "public", "type", "string", "name", name],
            [
                "size",
                1,
                "storage",
                "int32",
                "strings",
                values,
                "indices",
                ["size", 1, "storage", "int32", "arrays", [indices]]
            ]
        ])
    };
    let point_attributes = json!([
        numeric(
            "P",
            3,
            payload
                .instances
                .iter()
                .map(|instance| json!(instance.position))
                .collect()
        ),
        numeric(
            "orient",
            4,
            payload
                .instances
                .iter()
                .map(|instance| json!(instance.orientation))
                .collect()
        ),
        numeric(
            "scale",
            3,
            payload
                .instances
                .iter()
                .map(|instance| json!(instance.scale))
                .collect()
        ),
        numeric(
            "id",
            1,
            payload
                .instances
                .iter()
                .map(|instance| json!([instance.stable_id]))
                .collect()
        ),
        strings(
            "name",
            payload
                .prototypes
                .iter()
                .map(|prototype| json!(prototype.name))
                .collect(),
            payload
                .instances
                .iter()
                .map(|instance| json!(instance.prototype))
                .collect()
        ),
    ]);
    json!([
        "fileversion",
        "20.0.0",
        "pointcount",
        count,
        "vertexcount",
        0,
        "primitivecount",
        0,
        "topology",
        ["pointref", ["indices", []]],
        "attributes",
        ["pointattributes", point_attributes]
    ])
}

/// The value following `key` in a Houdini paired array.
fn paired<'a>(entries: &'a [JsonValue], key: &str) -> Option<&'a JsonValue> {
    entries
        .chunks(2)
        .find(|pair| pair.first().and_then(JsonValue::as_str) == Some(key))
        .and_then(|pair| pair.get(1))
}

/// The numeric tuples of one Houdini attribute body.
fn houdini_tuples(body: &[JsonValue]) -> Option<Vec<Vec<f64>>> {
    let values = paired(body, "values")?.as_array()?;
    let tuples = paired(values, "tuples")?.as_array()?;
    Some(
        tuples
            .iter()
            .map(|tuple| match tuple.as_array() {
                Some(lanes) => lanes
                    .iter()
                    .filter_map(JsonValue::as_f64)
                    .collect::<Vec<_>>(),
                None => tuple.as_f64().map(|value| vec![value]).unwrap_or_default(),
            })
            .collect(),
    )
}

/// The per-point strings of one Houdini string attribute, resolved through its index array.
fn houdini_strings(body: &[JsonValue]) -> Option<Vec<String>> {
    let table: Vec<String> = paired(body, "strings")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_owned())
        .collect();
    let indices = paired(body, "indices")?.as_array()?;
    let arrays = paired(indices, "arrays")?.as_array()?;
    let first = arrays.first()?.as_array()?;
    Some(
        first
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|index| table.get(index as usize))
                    .cloned()
                    .unwrap_or_default()
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_spatial::WorldCellKey;

    fn dimensions() -> BTreeMap<u64, crate::PlantDimensions> {
        let scalar = |value: f64| DecisionScalar::from_f64(value).unwrap();
        BTreeMap::from([(
            77,
            crate::PlantDimensions {
                height: scalar(6.0),
                trunk_radius: scalar(0.2),
                crown_radius: [scalar(2.0); 2],
                root_radius: [scalar(1.0); 2],
                local_bounds_min: [scalar(-2.0), scalar(0.0), scalar(-2.0)],
                local_bounds_max: [scalar(2.0), scalar(6.0), scalar(2.0)],
            },
        )])
    }

    fn payload() -> PointInterchange {
        PointInterchange {
            prototypes: vec![PointPrototype {
                name: "oak".to_owned(),
                family: Uuid(77),
            }],
            instances: vec![
                PointInstance {
                    prototype: 0,
                    position: [12.5, 3.25, -4.0],
                    orientation: [0.0, 0.0, 0.0, 1.0],
                    scale: [1.0, 1.0, 1.0],
                    stable_id: 41,
                    active: true,
                },
                PointInstance {
                    prototype: 0,
                    position: [1.0, 0.0, 2.0],
                    orientation: [0.0, 0.0, 0.0, 1.0],
                    scale: [2.0, 2.0, 2.0],
                    stable_id: 42,
                    active: false,
                },
            ],
            unsupported: Vec::new(),
        }
    }

    /// An imported point becomes an ordinary authored anchor: canonical position, canonical bounds,
    /// and an identity derived from the source's own stable id.
    #[test]
    fn an_instance_becomes_an_authored_anchor() {
        let anchors = interchange_to_anchors(&payload(), 9, &dimensions()).expect("anchors");
        // Only the active instance: a sparse mask deactivates rather than deletes.
        assert_eq!(anchors.len(), 1);
        let anchor = &anchors[0];
        assert_eq!(anchor.layer, 9);
        assert_eq!(anchor.family, Uuid(77));
        assert_eq!(anchor.point.candidate, 41);
        assert_eq!(anchor.point.flags, crate::PlantFlags::AUTHORED);
        assert_eq!(anchor.point.owner, anchor.point.position.cell());
        let metres = anchor.point.position.world_meters();
        assert!((metres.x - 12.5).abs() < 1.0e-4 && (metres.y - 3.25).abs() < 1.0e-4);
        let ticks = anchor.point.position.global_ticks();
        assert!(
            (0..3).all(|axis| {
                ticks[axis] >= anchor.point.bounds.min_ticks()[axis]
                    && ticks[axis] < anchor.point.bounds.max_ticks_exclusive()[axis]
            }),
            "the derived bounds contain the plant"
        );

        // The identity is a function of the layer and the source id, so a re-import addresses the
        // same plant and a different layer does not collide.
        let again = interchange_to_anchors(&payload(), 9, &dimensions()).expect("anchors");
        assert_eq!(again[0].id, anchor.id);
        let elsewhere = interchange_to_anchors(&payload(), 10, &dimensions()).expect("anchors");
        assert_ne!(elsewhere[0].id, anchor.id);
    }

    /// Two instances claiming one identity would collapse into one plant, so the source has to say
    /// which is which.
    #[test]
    fn a_duplicate_stable_id_is_refused() {
        let mut clashing = payload();
        clashing.instances[1].active = true;
        clashing.instances[1].stable_id = 41;
        assert!(interchange_to_anchors(&clashing, 9, &dimensions()).is_err());

        // So is a prototype the caller never declared, and a plant scaled to nothing.
        let mut missing = payload();
        missing.instances[0].prototype = 7;
        assert!(interchange_to_anchors(&missing, 9, &dimensions()).is_err());
        let mut flat = payload();
        flat.instances[0].scale = [1.0, 0.0, 1.0];
        assert!(interchange_to_anchors(&flat, 9, &dimensions()).is_err());
    }

    /// A payload written as Houdini geometry reads back as the same payload, which is what makes a
    /// DCC round trip safe.
    #[test]
    fn houdini_points_round_trip() {
        let source = payload();
        let document = write_houdini_points(&source);
        let families = BTreeMap::from([("oak".to_owned(), Uuid(77))]);
        let decoded = read_houdini_points(&document, &families).expect("read back");
        assert_eq!(decoded.prototypes, source.prototypes);
        assert_eq!(decoded.instances.len(), source.instances.len());
        for (before, after) in source.instances.iter().zip(&decoded.instances) {
            assert_eq!(before.prototype, after.prototype);
            assert_eq!(before.stable_id, after.stable_id);
            for lane in 0..3 {
                assert!((before.position[lane] - after.position[lane]).abs() < 1.0e-9);
                assert!((before.scale[lane] - after.scale[lane]).abs() < 1.0e-9);
            }
        }
        assert!(decoded.unsupported.is_empty());
    }

    /// An attribute the canonical vocabulary cannot express is reported, never carried along.
    #[test]
    fn an_unsupported_attribute_is_reported() {
        let mut document = write_houdini_points(&payload());
        let attributes = document
            .as_array_mut()
            .and_then(|root| {
                root.iter_mut()
                    .position(|value| value.as_str() == Some("attributes"))
                    .map(|index| index + 1)
                    .and_then(|index| root.get_mut(index))
            })
            .and_then(|value| value.as_array_mut())
            .and_then(|pair| pair.get_mut(1))
            .and_then(|value| value.as_array_mut())
            .expect("point attributes");
        attributes.push(saffron_json::json!([
            ["scope", "public", "type", "numeric", "name", "Cd"],
            [
                "size",
                3,
                "storage",
                "fpreal64",
                "values",
                [
                    "size",
                    3,
                    "storage",
                    "fpreal64",
                    "tuples",
                    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]
                ]
            ]
        ]));
        let families = BTreeMap::from([("oak".to_owned(), Uuid(77))]);
        let decoded = read_houdini_points(&document, &families).expect("read");
        assert_eq!(decoded.unsupported, vec!["Cd".to_owned()]);
        assert_eq!(decoded.instances.len(), 2, "the points still arrive");
    }

    /// Exporting keeps the stable ids and prototypes, so a round trip through a DCC tool comes back
    /// addressing the same plants.
    #[test]
    fn anchors_export_with_stable_ids_and_prototypes() {
        let anchors = interchange_to_anchors(&payload(), 9, &dimensions()).expect("anchors");
        let names = BTreeMap::from([(77, "oak".to_owned())]);
        let exported = anchors_to_interchange(&anchors, &names);
        assert_eq!(exported.prototypes.len(), 1);
        assert_eq!(exported.prototypes[0].name, "oak");
        assert_eq!(exported.instances.len(), 1);
        assert_eq!(exported.instances[0].stable_id, 41);
        // And the whole loop lands on the same identities.
        let reimported = interchange_to_anchors(&exported, 9, &dimensions()).expect("anchors");
        assert_eq!(reimported[0].id, anchors[0].id);
        assert_eq!(reimported[0].point.owner, anchors[0].point.owner);
        let _ = WorldCellKey::base(0, 0, 0);
    }
}

/// Turns glTF `EXT_mesh_gpu_instancing` placements into an interchange payload.
///
/// A prototype is one instanced node, addressed by the node's name. An instance without an `_ID`
/// attribute takes its index within the payload as its stable identity — the same fallback a point
/// cloud without an `id` attribute gets, and the reason a source that renumbers its nodes renumbers
/// its plants.
///
/// # Errors
///
/// [`Error::ArtifactFormat`] when a node's name resolves to no plant family.
pub fn gltf_instancing_to_interchange(
    instancing: &saffron_geometry::GltfInstancing,
    families: &BTreeMap<String, Uuid>,
) -> Result<PointInterchange> {
    let field = |name: &str| Error::ArtifactFormat {
        format: "gltf instancing",
        field: name.to_owned(),
    };
    let mut prototypes = Vec::new();
    let mut instances = Vec::new();
    for set in &instancing.sets {
        let family = *families
            .get(&set.name)
            .ok_or_else(|| field("node.family"))?;
        let prototype = u32::try_from(prototypes.len()).map_err(|_| field("prototypes"))?;
        prototypes.push(PointPrototype {
            name: set.name.clone(),
            family,
        });
        for instance in &set.instances {
            let stable_id = instance.id.unwrap_or(instances.len() as u64);
            instances.push(PointInstance {
                prototype,
                position: instance.translation,
                orientation: instance.rotation,
                scale: instance.scale,
                stable_id,
                active: true,
            });
        }
    }
    if instances.len() > MAX_INTERCHANGE_INSTANCES {
        return Err(field("instances.count"));
    }
    Ok(PointInterchange {
        prototypes,
        instances,
        unsupported: instancing.unsupported.clone(),
    })
}
