//! Turning a grown botanical assembly into the compiled family every downstream system reads.
//!
//! A native family produces exactly the same normalized shapes an imported one does — quantized
//! meshes, structural joints, semantic parts, spines, and dimensions — so the cooker, renderer,
//! wind rig, collision, navigation, and lifecycle machinery cannot tell the two apart. There is no
//! native-only geometry path and no second mesh vocabulary.
//!
//! Generation is integer-exact. Positions are Q15.16 metres, normals and tangents are signed
//! normalized, and every trigonometric value comes from the graph's own integer table, so an
//! authored plant compiles to identical bytes on every target.

use std::collections::BTreeMap;

use saffron_spatial::{DecisionScalar, UnitInterval};

use crate::{
    BotanicalAssembly, BotanicalAxis, BotanicalElement, BotanicalElementId, BotanicalFrame,
    BotanicalGraft, BotanicalPlacement, BotanicalShell, Error, NormalizedPlantJoint,
    NormalizedPlantMesh, NormalizedPlantSkin, NormalizedPlantSubmesh, NormalizedPlantVertex,
    PlantDimensions, PlantPart, PlantSourceRole, PlantSourceSelector, Result, StructuralSpine,
    turn_sin_cos,
};

/// The vertex, skin, and index buffers one material slot accumulates.
type MeshBuffers = (
    Vec<NormalizedPlantVertex>,
    Vec<NormalizedPlantSkin>,
    Vec<u32>,
);

/// Vertex ceiling for one generated family. A bound, not a budget: a graph that would exceed it is
/// a mistake caught here rather than a multi-gigabyte artifact.
pub const MAX_GENERATED_VERTICES: usize = 1 << 20;

/// The compiled geometry one assembly generates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BotanicalGeometry {
    /// One mesh carrying every generated surface, with material-homogeneous submeshes.
    pub meshes: Vec<NormalizedPlantMesh>,
    /// Structural joints, one per axis, in canonical identity order.
    pub joints: Vec<NormalizedPlantJoint>,
}

/// The family structure one assembly declares: semantic parts, spines, and dimensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalStructure {
    /// Semantic parts, one per axis plus one per instanced element class.
    pub parts: Vec<PlantPart>,
    /// Structural spines, one per axis.
    pub spines: Vec<StructuralSpine>,
    /// Validated bounds and footprints.
    pub dimensions: PlantDimensions,
}

/// The stable part identity for one botanical element class.
///
/// Classes rather than individuals: a family declares "this is its leaves", and the thousands of
/// instanced leaves are micro transforms under that one part.
#[must_use]
pub fn class_part_id(element: BotanicalElement) -> u128 {
    (1_u128 << 120) | u128::from(element.tag())
}

/// Derives the semantic parts, spines, and dimensions the family declares.
///
/// # Errors
///
/// [`Error::ArtifactFormat`] when the assembly grew nothing, and [`Error::NumericOverflow`] on a
/// fixed-point overflow.
pub fn derive_family_structure(assembly: &BotanicalAssembly) -> Result<BotanicalStructure> {
    if assembly.axes.is_empty() {
        return Err(Error::ArtifactFormat {
            format: "botanical family",
            field: "axes".to_owned(),
        });
    }
    // One part per element class present, parented by the class hierarchy the axes describe.
    let mut classes: BTreeMap<BotanicalElement, Option<BotanicalElement>> = BTreeMap::new();
    let axis_class: BTreeMap<BotanicalElementId, BotanicalElement> = assembly
        .axes
        .iter()
        .map(|axis| (axis.id, axis.element))
        .collect();
    for axis in &assembly.axes {
        let parent = axis
            .parent
            .and_then(|parent| axis_class.get(&parent).copied())
            .filter(|parent| *parent != axis.element);
        classes.entry(axis.element).or_insert(parent);
    }
    let frame_axis: BTreeMap<BotanicalElementId, BotanicalElementId> = assembly
        .frames
        .iter()
        .map(|frame| (frame.id, frame.axis))
        .collect();
    let mut slots: BTreeMap<BotanicalElement, u32> = assembly
        .shells
        .iter()
        .map(|shell| (shell.element, shell.material_slot))
        .collect();
    for element in &assembly.elements {
        let parent = frame_axis
            .get(&element.frame)
            .and_then(|axis| axis_class.get(axis).copied());
        classes.entry(element.element).or_insert(parent);
        slots
            .entry(element.element)
            .or_insert(element.material_slot);
    }

    let parts = classes
        .iter()
        .map(|(element, parent)| PlantPart {
            id: class_part_id(*element),
            parent: parent.map(class_part_id),
            semantic: element.part_semantic(),
            material_slot: slots.get(element).copied().unwrap_or_default(),
            sources: Vec::new(),
        })
        .collect();

    let spines = assembly
        .axes
        .iter()
        .map(|axis| StructuralSpine {
            id: axis.id.value(),
            part: class_part_id(axis.element),
            parent: axis.parent.map(BotanicalElementId::value),
            rest_points: axis.points.clone(),
            radii: axis.radii.clone(),
        })
        .collect();

    let (minimum, maximum) = assembly.local_bounds();
    // A footprint is the widest reach of the relevant axes, including their own radius: a straight
    // trunk still occupies ground, and a zero footprint would be a lie the validator rejects.
    let horizontal = |root: bool| -> [DecisionScalar; 2] {
        let mut extent = [1_i32; 2];
        for axis in assembly
            .axes
            .iter()
            .filter(|axis| matches!(axis.element, BotanicalElement::Root) == root)
        {
            for (point, radius) in axis.points.iter().zip(&axis.radii) {
                extent[0] = extent[0].max(point[0].bits().abs().saturating_add(radius.bits()));
                extent[1] = extent[1].max(point[2].bits().abs().saturating_add(radius.bits()));
            }
        }
        [
            DecisionScalar::from_bits(extent[0]),
            DecisionScalar::from_bits(extent[1]),
        ]
    };
    let trunk_radius = assembly
        .axes
        .iter()
        .filter(|axis| matches!(axis.element, BotanicalElement::Trunk))
        .filter_map(|axis| axis.radii.first().copied())
        .max_by_key(|radius| radius.bits())
        .unwrap_or_else(|| DecisionScalar::from_bits(1));

    Ok(BotanicalStructure {
        parts,
        spines,
        dimensions: PlantDimensions {
            height: DecisionScalar::from_bits(maximum[1].bits().max(1)),
            trunk_radius,
            crown_radius: horizontal(false),
            root_radius: horizontal(true),
            local_bounds_min: minimum,
            local_bounds_max: maximum,
        },
    })
}

/// Generates the family's geometry and structural joints.
///
/// Shells sweep their axis into a tube of `sides` faces; instanced elements become one quad each,
/// standing on their frame. Both bind to the structural joint of the axis they belong to, so the
/// wind rig and the skinning prepass drive generated geometry exactly as they drive imported
/// geometry.
///
/// # Errors
///
/// [`Error::ArtifactFormat`] when the assembly would exceed [`MAX_GENERATED_VERTICES`], and
/// [`Error::NumericOverflow`] on a fixed-point overflow.
pub fn normalize_botanical_geometry(
    source: u128,
    assembly: &BotanicalAssembly,
    grafted: &BTreeMap<BotanicalElementId, Vec<NormalizedPlantMesh>>,
) -> Result<BotanicalGeometry> {
    let joint_index: BTreeMap<BotanicalElementId, u16> = assembly
        .axes
        .iter()
        .enumerate()
        .map(|(index, axis)| (axis.id, u16::try_from(index).unwrap_or(u16::MAX)))
        .collect();
    let axis_by_id: BTreeMap<BotanicalElementId, &BotanicalAxis> =
        assembly.axes.iter().map(|axis| (axis.id, axis)).collect();
    let frame_by_id: BTreeMap<BotanicalElementId, &BotanicalFrame> = assembly
        .frames
        .iter()
        .map(|frame| (frame.id, frame))
        .collect();

    // Grouped by material slot so each submesh is one homogeneous draw range.
    let mut per_slot: BTreeMap<u32, MeshBuffers> = BTreeMap::new();

    for shell in &assembly.shells {
        let Some(axis) = axis_by_id.get(&shell.axis) else {
            continue;
        };
        let joint = joint_index.get(&shell.axis).copied().unwrap_or_default();
        let entry = per_slot.entry(shell.material_slot).or_default();
        sweep_axis(axis, shell, joint, entry)?;
    }
    for placement in &assembly.elements {
        let Some(frame) = frame_by_id.get(&placement.frame) else {
            continue;
        };
        let joint = joint_index.get(&frame.axis).copied().unwrap_or_default();
        let entry = per_slot.entry(placement.material_slot).or_default();
        place_quad(frame, placement, joint, entry)?;
    }

    let total: usize = per_slot
        .values()
        .map(|(vertices, _, _)| vertices.len())
        .sum();
    if total > MAX_GENERATED_VERTICES {
        return Err(Error::ArtifactFormat {
            format: "botanical family",
            field: "vertices".to_owned(),
        });
    }

    let mut vertices = Vec::with_capacity(total);
    let mut skin = Vec::with_capacity(total);
    let mut indices = Vec::new();
    let mut submeshes = Vec::new();
    for (slot, (slot_vertices, slot_skin, slot_indices)) in per_slot {
        let base = u32::try_from(vertices.len()).map_err(|_| Error::NumericOverflow)?;
        let first_index = u32::try_from(indices.len()).map_err(|_| Error::NumericOverflow)?;
        vertices.extend(slot_vertices);
        skin.extend(slot_skin);
        indices.extend(slot_indices.iter().map(|index| index + base));
        submeshes.push(NormalizedPlantSubmesh {
            first_index,
            index_count: u32::try_from(slot_indices.len()).map_err(|_| Error::NumericOverflow)?,
            material_slot: slot,
        });
    }

    let joints = assembly
        .axes
        .iter()
        .map(|axis| {
            let base = axis.points.first().copied().unwrap_or_default();
            let mut transform = [0_i32; 16];
            for lane in 0..4 {
                transform[lane * 5] = 1 << 16;
            }
            transform[12] = base[0].bits();
            transform[13] = base[1].bits();
            transform[14] = base[2].bits();
            NormalizedPlantJoint {
                source,
                selector: axis_selector(axis),
                parent: axis
                    .parent
                    .and_then(|parent| axis_by_id.get(&parent))
                    .map(|parent| axis_selector(parent)),
                transform_bits: transform,
            }
        })
        .collect();

    let mut meshes = if vertices.is_empty() {
        Vec::new()
    } else {
        vec![NormalizedPlantMesh {
            source,
            role: PlantSourceRole::Geometry,
            selector: PlantSourceSelector::Whole,
            vertices,
            indices,
            submeshes,
            skin,
        }]
    };
    // Grafted geometry arrives already normalized by the imported-source path, standing at the
    // origin. Here it is stood up on the frame the element it replaced sat on, and bound to that
    // frame's axis joint, so the wind rig and the skinning prepass drive it like everything else.
    for graft in &assembly.grafts {
        let Some(frame) = frame_by_id.get(&graft.frame) else {
            continue;
        };
        let joint = joint_index.get(&frame.axis).copied().unwrap_or_default();
        for mesh in grafted.get(&graft.id).into_iter().flatten() {
            meshes.push(place_graft(graft, frame, joint, mesh)?);
        }
    }
    Ok(BotanicalGeometry { meshes, joints })
}

/// The family variation table one graph's declared individuals produce.
///
/// One row per declared variation, each naming its own grown geometry, so the runtime selects an
/// individual by variation exactly as it does for an imported family with several source meshes.
#[must_use]
pub fn native_variations(graph: &crate::BotanicalGraphDocument) -> Vec<crate::PlantVariation> {
    graph
        .variations
        .iter()
        .enumerate()
        .map(|(index, variation)| crate::PlantVariation {
            id: index as u32,
            name: variation.name.clone(),
            sources: vec![crate::native_variation_source_id(index)],
            active_parts: Vec::new(),
        })
        .collect()
}

/// Collision proxies one grown plant may derive. A bound, not a budget: a proxy per twig would be a
/// body-per-branch explosion in the runtime's batched collision residency.
pub const MAX_DERIVED_COLLISION_PROXIES: usize = 8;

/// The collision and navigation proxies one grown plant declares.
///
/// A native family's proxies are a result, like its dimensions — a stale authored value would be a
/// second truth about the same geometry. A capsule stands in for each axis thick enough for a
/// character to collide with, thickest first, and one octagonal footprint stands in for the plant on
/// the navigation seam.
#[must_use]
pub fn derive_family_proxies(
    assembly: &BotanicalAssembly,
    dimensions: &PlantDimensions,
) -> (
    Vec<crate::PlantCollisionProxy>,
    Vec<crate::PlantNavigationProxy>,
) {
    let thickest = assembly
        .axes
        .iter()
        .filter(|axis| matches!(axis.element, BotanicalElement::Trunk))
        .filter_map(|axis| axis.radii.first().map(|radius| radius.bits()))
        .max()
        .unwrap_or_else(|| dimensions.trunk_radius.bits())
        .max(1);
    // A quarter of the trunk is the thinnest thing worth colliding with: below that a character
    // brushes past a twig, and the body is cost without behaviour.
    let floor = (thickest / 4).max(1);
    let mut candidates: Vec<&BotanicalAxis> = assembly
        .axes
        .iter()
        .filter(|axis| {
            // Roots are below ground, so nothing walks into them.
            !matches!(axis.element, BotanicalElement::Root)
                && axis.radii.iter().any(|radius| radius.bits() >= floor)
        })
        .collect();
    candidates.sort_by_key(|axis| {
        (
            std::cmp::Reverse(
                axis.radii
                    .iter()
                    .map(|radius| radius.bits())
                    .max()
                    .unwrap_or_default(),
            ),
            axis.id,
        )
    });
    candidates.truncate(MAX_DERIVED_COLLISION_PROXIES);
    let mut collision: Vec<crate::PlantCollisionProxy> = candidates
        .iter()
        .filter_map(|axis| {
            let base = axis.points.first()?;
            let tip = axis.points.last()?;
            let radius = axis.radii.iter().map(|radius| radius.bits()).max()?.max(1);
            let center = std::array::from_fn(|lane| {
                DecisionScalar::from_bits((base[lane].bits() + tip[lane].bits()) / 2)
            });
            let half_height = (axis.length().bits() / 2).max(1);
            Some(crate::PlantCollisionProxy {
                id: axis.id.value(),
                shape: crate::PlantCollisionShape::Capsule,
                part: class_part_id(axis.element),
                center,
                dimensions: [
                    DecisionScalar::from_bits(radius),
                    DecisionScalar::from_bits(half_height),
                    DecisionScalar::from_bits(radius),
                ],
                // A trunk is the plant; break it and the plant is felled rather than pruned.
                breakable: !matches!(axis.element, BotanicalElement::Trunk),
            })
        })
        .collect();
    collision.sort_by_key(|proxy| proxy.id);

    // An octagon of the trunk radius: a character routes around the stem, not around the canopy.
    let footprint_radius = dimensions.trunk_radius.bits().max(1);
    let one = i64::from(UnitInterval::ONE.bits());
    let footprint = (0..8)
        .map(|corner| {
            let (sin, cos) = turn_sin_cos(one * i64::from(corner) / 8);
            [
                DecisionScalar::from_bits(
                    i32::try_from(i64::from(footprint_radius) * cos / one).unwrap_or(i32::MAX),
                ),
                DecisionScalar::from_bits(
                    i32::try_from(i64::from(footprint_radius) * sin / one).unwrap_or(i32::MAX),
                ),
            ]
        })
        .collect();
    let navigation = vec![crate::PlantNavigationProxy {
        id: NAVIGATION_PROXY_ID,
        footprint,
        height: DecisionScalar::from_bits(dimensions.height.bits().max(1)),
        // Neutral: the interaction policy decides whether this reads as an obstacle or a cost.
        cost: UnitInterval::ONE,
    }];
    (collision, navigation)
}

/// The identity of the one navigation proxy a native family derives.
const NAVIGATION_PROXY_ID: u128 = 1 << 125;

/// Element classes that fall off a plant, and so distinguish one appearance from another.
const PERISHABLE: [BotanicalElement; 7] = [
    BotanicalElement::Leaf,
    BotanicalElement::Needle,
    BotanicalElement::Blade,
    BotanicalElement::Frond,
    BotanicalElement::Flower,
    BotanicalElement::Fruit,
    BotanicalElement::Bud,
];

/// The phenotype table one graph's grown individuals can express.
///
/// A phenotype is a selection over the classes a variation actually grew: flowers without fruit,
/// fruit without flowers, neither once harvested, and only the woody structure once dead. Roles that
/// are purely a material change — senescent, damaged, burned, wet — need authored per-role materials
/// and are not invented here.
#[must_use]
pub fn native_phenotypes(grown: &[BotanicalAssembly]) -> Vec<crate::PlantPhenotype> {
    let mut phenotypes = Vec::new();
    let mut next = 0_u32;
    for (index, assembly) in grown.iter().enumerate() {
        let mut classes: BTreeMap<BotanicalElement, ()> = BTreeMap::new();
        for axis in &assembly.axes {
            classes.insert(axis.element, ());
        }
        for element in &assembly.elements {
            classes.insert(element.element, ());
        }
        let present: Vec<BotanicalElement> = classes.keys().copied().collect();
        let without = |excluded: &[BotanicalElement]| -> Vec<u128> {
            present
                .iter()
                .filter(|class| !excluded.contains(class))
                .map(|class| class_part_id(*class))
                .collect()
        };
        let mut push = |role: crate::PhenotypeRole, active_parts: Vec<u128>| {
            phenotypes.push(crate::PlantPhenotype {
                id: next,
                role,
                season_window: None,
                variation: index as u32,
                material_remap: Vec::new(),
                active_parts,
            });
            next += 1;
        };
        push(crate::PhenotypeRole::Healthy, Vec::new());
        let flowering = present.contains(&BotanicalElement::Flower);
        let fruiting = present.contains(&BotanicalElement::Fruit);
        if flowering {
            push(
                crate::PhenotypeRole::Flowering,
                without(&[BotanicalElement::Fruit]),
            );
        }
        if fruiting {
            push(
                crate::PhenotypeRole::Fruiting,
                without(&[BotanicalElement::Flower]),
            );
        }
        if flowering || fruiting {
            push(
                crate::PhenotypeRole::Harvested,
                without(&[BotanicalElement::Flower, BotanicalElement::Fruit]),
            );
        }
        if present.iter().any(|class| PERISHABLE.contains(class)) {
            push(crate::PhenotypeRole::Dead, without(&PERISHABLE));
        }
    }
    phenotypes
}

/// The structure a family declares over every variation it grows: the union of their parts and
/// spines, and dimensions wide enough to contain all of them.
///
/// # Errors
///
/// Propagates [`derive_family_structure`], and refuses an empty variation list.
pub fn widest_family_structure(grown: &[BotanicalAssembly]) -> Result<BotanicalStructure> {
    let mut structures = grown
        .iter()
        .map(derive_family_structure)
        .collect::<Result<Vec<_>>>()?
        .into_iter();
    let mut widest = structures.next().ok_or_else(|| Error::ArtifactFormat {
        format: "botanical family",
        field: "variations".to_owned(),
    })?;
    // The spines are the representative individual's, because element identities do not depend on
    // the seed or the age: every variation describes the same skeleton at a different size, and its
    // own rest transforms travel with its own compiled joints.
    for structure in structures {
        for part in structure.parts {
            if !widest.parts.iter().any(|existing| existing.id == part.id) {
                widest.parts.push(part);
            }
        }
        let dimensions = structure.dimensions;
        widest.dimensions.height = widest.dimensions.height.max(dimensions.height);
        widest.dimensions.trunk_radius =
            widest.dimensions.trunk_radius.max(dimensions.trunk_radius);
        for lane in 0..2 {
            widest.dimensions.crown_radius[lane] =
                widest.dimensions.crown_radius[lane].max(dimensions.crown_radius[lane]);
            widest.dimensions.root_radius[lane] =
                widest.dimensions.root_radius[lane].max(dimensions.root_radius[lane]);
        }
        for lane in 0..3 {
            widest.dimensions.local_bounds_min[lane] = DecisionScalar::from_bits(
                widest.dimensions.local_bounds_min[lane]
                    .bits()
                    .min(dimensions.local_bounds_min[lane].bits()),
            );
            widest.dimensions.local_bounds_max[lane] = DecisionScalar::from_bits(
                widest.dimensions.local_bounds_max[lane]
                    .bits()
                    .max(dimensions.local_bounds_max[lane].bits()),
            );
        }
    }
    widest.parts.sort_by_key(|part| part.id);
    Ok(widest)
}

/// One in Q15.16, the fixed-point scale every botanical position and direction uses.
const Q16: i64 = 1 << 16;

/// A vector scaled to unit length in Q15.16.
fn unit_q16(vector: [i64; 3]) -> [i64; 3] {
    let magnitude = isqrt64(vector.iter().map(|lane| lane * lane).sum::<i64>()).max(1);
    std::array::from_fn(|lane| vector[lane] * Q16 / magnitude)
}

/// The cross product of two Q15.16 vectors, in Q15.16.
fn cross_q16(first: [i64; 3], second: [i64; 3]) -> [i64; 3] {
    [
        (first[1] * second[2] - first[2] * second[1]) / Q16,
        (first[2] * second[0] - first[0] * second[2]) / Q16,
        (first[0] * second[1] - first[1] * second[0]) / Q16,
    ]
}

/// Stands one normalized graft mesh up on its frame.
///
/// The frame's outward direction becomes the mesh's up axis, so a hero branch modelled growing
/// upward grows outward along the limb it replaces. Every step is integer, because a graft that
/// moved by a bit between targets would break the family's content hash.
fn place_graft(
    graft: &BotanicalGraft,
    frame: &BotanicalFrame,
    joint: u16,
    mesh: &NormalizedPlantMesh,
) -> Result<NormalizedPlantMesh> {
    let forward = unit_q16(std::array::from_fn(|lane| {
        i64::from(frame.direction[lane].bits())
    }));
    // Any vector not parallel to the frame gives a stable second axis; near-vertical frames take
    // the world X axis instead, where world Y would degenerate.
    let helper = if forward[1].abs() < Q16 * 9 / 10 {
        [0, Q16, 0]
    } else {
        [Q16, 0, 0]
    };
    let right = unit_q16(cross_q16(helper, forward));
    let up = cross_q16(forward, right);
    let one = i64::from(UnitInterval::ONE.bits());
    let (sin, cos) = turn_sin_cos(i64::from(graft.roll.bits()) * 2);
    let rolled_right: [i64; 3] =
        std::array::from_fn(|lane| (right[lane] * cos + up[lane] * sin) / one);
    let rolled_up: [i64; 3] =
        std::array::from_fn(|lane| (up[lane] * cos - right[lane] * sin) / one);
    let rotate = |vector: [i64; 3]| -> [i64; 3] {
        std::array::from_fn(|lane| {
            (vector[0] * rolled_right[lane]
                + vector[1] * forward[lane]
                + vector[2] * rolled_up[lane])
                / Q16
        })
    };

    let vertices = mesh
        .vertices
        .iter()
        .map(|vertex| {
            let position = rotate(std::array::from_fn(|lane| {
                i64::from(vertex.position_bits[lane])
            }));
            let normal = rotate(std::array::from_fn(|lane| {
                i64::from(vertex.normal_snorm[lane])
            }));
            let tangent = rotate(std::array::from_fn(|lane| {
                i64::from(vertex.tangent_snorm[lane])
            }));
            NormalizedPlantVertex {
                position_bits: std::array::from_fn(|lane| {
                    graft.position[lane]
                        .bits()
                        .saturating_add(i32::try_from(position[lane]).unwrap_or(i32::MAX))
                }),
                normal_snorm: std::array::from_fn(|lane| {
                    i16::try_from(normal[lane].clamp(-32_767, 32_767)).unwrap_or(0)
                }),
                uv_bits: vertex.uv_bits,
                tangent_snorm: [
                    i16::try_from(tangent[0].clamp(-32_767, 32_767)).unwrap_or(0),
                    i16::try_from(tangent[1].clamp(-32_767, 32_767)).unwrap_or(0),
                    i16::try_from(tangent[2].clamp(-32_767, 32_767)).unwrap_or(0),
                    vertex.tangent_snorm[3],
                ],
            }
        })
        .collect::<Vec<_>>();
    if vertices.len() > MAX_GENERATED_VERTICES {
        return Err(Error::ArtifactFormat {
            format: "botanical family",
            field: "graft.vertices".to_owned(),
        });
    }
    // The graft is rigid on the limb it stands on: it has no spine of its own to weight against.
    let skin = vec![
        NormalizedPlantSkin {
            joints: [joint, 0, 0, 0],
            weights: [UnitInterval::ONE.bits(), 0, 0, 0],
        };
        vertices.len()
    ];
    Ok(NormalizedPlantMesh {
        source: graft.source,
        role: PlantSourceRole::Geometry,
        selector: PlantSourceSelector::Element {
            id: graft.id.value(),
            path: format!("graft/{:032x}", graft.id.value()),
        },
        vertices,
        indices: mesh.indices.clone(),
        submeshes: mesh.submeshes.clone(),
        skin,
    })
}

/// The stable selector one axis is addressed by, in geometry and in diagnostics.
fn axis_selector(axis: &BotanicalAxis) -> PlantSourceSelector {
    PlantSourceSelector::Element {
        id: axis.id.value(),
        path: format!("{}/{:032x}", element_path(axis.element), axis.id.value()),
    }
}

/// The canonical hierarchy path segment for an element class.
const fn element_path(element: BotanicalElement) -> &'static str {
    match element {
        BotanicalElement::Trunk => "trunk",
        BotanicalElement::Branch => "branch",
        BotanicalElement::Root => "root",
        BotanicalElement::Vine => "vine",
        BotanicalElement::Frond => "frond",
        BotanicalElement::Leaf => "leaf",
        BotanicalElement::Needle => "needle",
        BotanicalElement::Blade => "blade",
        BotanicalElement::Flower => "flower",
        BotanicalElement::Fruit => "fruit",
        BotanicalElement::Bud => "bud",
        BotanicalElement::Scar => "scar",
        BotanicalElement::DeadPart => "dead",
    }
}

/// Sweeps one axis into a closed tube: a ring of `sides` vertices at every rest point.
fn sweep_axis(
    axis: &BotanicalAxis,
    shell: &BotanicalShell,
    joint: u16,
    buffers: &mut MeshBuffers,
) -> Result<()> {
    let (vertices, skin, indices) = buffers;
    let sides = shell.sides.max(3);
    let rings = axis.points.len();
    if rings < 2 {
        return Ok(());
    }
    let base = u32::try_from(vertices.len()).map_err(|_| Error::NumericOverflow)?;
    let one = i64::from(UnitInterval::ONE.bits());
    for (ring, (point, radius)) in axis.points.iter().zip(&axis.radii).enumerate() {
        // v runs along the axis, u around it: one continuous shell of bark.
        let v = i32::try_from(
            i64::from(ring as i32) * i64::from(1_i32 << 16) / i64::from(rings as i32 - 1),
        )
        .unwrap_or(1 << 16);
        for side in 0..sides {
            let turn = one * i64::from(side) / i64::from(sides);
            let (sin, cos) = turn_sin_cos(turn * 2);
            let offset = |component: i64| -> i32 {
                i32::try_from(i64::from(radius.bits()) * component / one).unwrap_or(i32::MAX)
            };
            let normal = |component: i64| -> i16 {
                i16::try_from(component * 32_767 / one.max(1)).unwrap_or(0)
            };
            vertices.push(NormalizedPlantVertex {
                position_bits: [
                    point[0].bits().saturating_add(offset(cos)),
                    point[1].bits(),
                    point[2].bits().saturating_add(offset(sin)),
                ],
                normal_snorm: [normal(cos), 0, normal(sin)],
                uv_bits: [
                    i32::try_from(i64::from(1_i32 << 16) * i64::from(side) / i64::from(sides))
                        .unwrap_or(0),
                    v,
                ],
                tangent_snorm: [0, 32_767, 0, 32_767],
            });
            skin.push(NormalizedPlantSkin {
                joints: [joint, 0, 0, 0],
                weights: [UnitInterval::ONE.bits(), 0, 0, 0],
            });
        }
    }
    for ring in 0..rings - 1 {
        for side in 0..sides {
            let next_side = (side + 1) % sides;
            let ring_base = base + u32::try_from(ring).map_err(|_| Error::NumericOverflow)? * sides;
            let a = ring_base + side;
            let b = ring_base + next_side;
            let c = a + sides;
            let d = b + sides;
            // Counter-clockwise seen from outside.
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    Ok(())
}

/// Places one instanced element as a quad standing on its frame.
fn place_quad(
    frame: &BotanicalFrame,
    placement: &BotanicalPlacement,
    joint: u16,
    buffers: &mut MeshBuffers,
) -> Result<()> {
    let (vertices, skin, indices) = buffers;
    let base = u32::try_from(vertices.len()).map_err(|_| Error::NumericOverflow)?;
    let one = i64::from(UnitInterval::ONE.bits());
    let size = i64::from(placement.size.bits());
    let (roll_sin, roll_cos) = turn_sin_cos(i64::from(placement.roll.bits()) * 2);
    // Outward along the frame, half a width to each side, rolled about the outward direction.
    let outward: [i64; 3] = std::array::from_fn(|lane| i64::from(frame.direction[lane].bits()));
    let magnitude = outward
        .iter()
        .map(|component| component * component)
        .sum::<i64>()
        .max(1);
    let scale = |component: i64, length: i64| -> i32 {
        i32::try_from(component * length / isqrt64(magnitude).max(1)).unwrap_or(i32::MAX)
    };
    let lateral = [
        i32::try_from(size * roll_cos / (2 * one)).unwrap_or(0),
        0,
        i32::try_from(size * roll_sin / (2 * one)).unwrap_or(0),
    ];
    let tip = [
        scale(outward[0], size),
        scale(outward[1], size),
        scale(outward[2], size),
    ];
    let corner = |along: bool, side: i32| -> [i32; 3] {
        std::array::from_fn(|lane| {
            frame.position[lane]
                .bits()
                .saturating_add(if along { tip[lane] } else { 0 })
                .saturating_add(lateral[lane] * side)
        })
    };
    for (index, position) in [
        corner(false, -1),
        corner(false, 1),
        corner(true, 1),
        corner(true, -1),
    ]
    .into_iter()
    .enumerate()
    {
        vertices.push(NormalizedPlantVertex {
            position_bits: position,
            normal_snorm: [0, 32_767, 0],
            uv_bits: [
                i32::from(index as i16 & 1) << 16,
                i32::from((index as i16 >> 1) & 1) << 16,
            ],
            tangent_snorm: [32_767, 0, 0, 32_767],
        });
        skin.push(NormalizedPlantSkin {
            joints: [joint, 0, 0, 0],
            weights: [UnitInterval::ONE.bits(), 0, 0, 0],
        });
    }
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    Ok(())
}

/// Integer square root over `i64`, so a generated position never depends on floating-point
/// rounding.
fn isqrt64(value: i64) -> i64 {
    if value <= 0 {
        return 0;
    }
    let mut low = 0_i64;
    let mut high = 3_037_000_499_i64.min(value);
    while low < high {
        let mid = low + (high - low + 1) / 2;
        if mid.saturating_mul(mid) <= value {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low
}

/// Builds a complete native plant family from a botanical graph.
///
/// The graph is grown once so the family's parts, spines, and dimensions are what the plant
/// actually is rather than an authored guess that could drift from it. This is the authoring front
/// door: creating a plant means creating one of these.
///
/// # Errors
///
/// Propagates growing and generation, and [`Error::ArtifactFormat`] when `materials` does not cover
/// every material slot the graph binds.
pub fn native_plant_family(
    id: saffron_core::Uuid,
    name: &str,
    graph: crate::BotanicalGraphDocument,
    materials: Vec<saffron_core::Uuid>,
    modules: &dyn crate::BotanicalModuleResolver,
) -> Result<crate::PlantFamilyAsset> {
    // Every declared variation grows, because the family's parts, dimensions, and material slots
    // must contain all of them — a slot only the sapling binds is still a slot.
    let grown = (0..graph.variations.len())
        .map(|index| {
            crate::grow(&graph, index, modules, &crate::BotanicalBudget::COOK)
                .map(|growth| growth.assembly)
        })
        .collect::<Result<Vec<_>>>()?;
    let structure = widest_family_structure(&grown)?;
    let slots = grown
        .iter()
        .flat_map(|assembly| {
            assembly
                .shells
                .iter()
                .map(|shell| shell.material_slot)
                .chain(
                    assembly
                        .elements
                        .iter()
                        .map(|element| element.material_slot),
                )
        })
        .max()
        .map_or(0, |highest| highest as usize + 1);
    if materials.len() < slots {
        return Err(Error::ArtifactFormat {
            format: "botanical family",
            field: "materialSlots".to_owned(),
        });
    }
    let variations = native_variations(&graph);
    let phenotypes = native_phenotypes(&grown);
    // Proxies come from the representative individual: a variation is the same structure at another
    // size, and the runtime scales a proxy with the instance it belongs to.
    let (collision_proxies, navigation_proxies) =
        derive_family_proxies(&grown[0], &structure.dimensions);
    Ok(crate::PlantFamilyAsset {
        role: crate::PlantFamilyRole::Family,
        modules: Vec::new(),
        module_recursion_limit: crate::MAX_PLANT_MODULE_RECURSION,
        version: crate::PLANT_ASSET_VERSION,
        id,
        name: name.to_owned(),
        tags: Vec::new(),
        source: crate::PlantFamilySource::Native {
            graph,
            grafts: Vec::new(),
        },
        parts: structure.parts,
        dimensions: structure.dimensions,
        material_slots: materials,
        spines: structure.spines,
        mechanics: crate::MechanicalResponse {
            stiffness: DecisionScalar::from_bits(2 << 16),
            damping: UnitInterval::from_bits(12_000),
            drag: DecisionScalar::from_bits(1 << 16),
            flutter: DecisionScalar::from_bits(1 << 14),
            bend_limit: UnitInterval::from_bits(8_000),
            damage_threshold: DecisionScalar::from_bits(3 << 16),
            break_threshold: DecisionScalar::from_bits(6 << 16),
        },
        variations,
        phenotypes,
        collision_proxies,
        navigation_proxies,
        interaction_policy: crate::InteractionPolicy::Structural,
        habitat: None,
        ecology: crate::PlantEcologyDeclaration::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BotanicalGraphDocument, grow};

    /// The native family generates real geometry: a closed tube per swept axis, a quad per
    /// instanced element, one submesh per material slot, and one structural joint per axis.
    #[test]
    fn a_grown_plant_generates_geometry_joints_and_structure() {
        let document = crate::botanical::tests_support::birch();
        let assembly = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the birch grows")
        .assembly;
        let geometry =
            normalize_botanical_geometry(7, &assembly, &BTreeMap::new()).expect("geometry");

        assert_eq!(geometry.joints.len(), assembly.axes.len());
        let mesh = &geometry.meshes[0];
        // Two material slots: bark and leaves.
        assert_eq!(mesh.submeshes.len(), 2);
        assert_eq!(mesh.skin.len(), mesh.vertices.len());
        assert!(
            mesh.indices
                .iter()
                .all(|index| { (*index as usize) < mesh.vertices.len() })
        );
        assert_eq!(mesh.indices.len() % 3, 0);
        // Every submesh range is inside the index stream and homogeneous.
        for submesh in &mesh.submeshes {
            let end = submesh.first_index + submesh.index_count;
            assert!(end as usize <= mesh.indices.len());
            assert_eq!(submesh.index_count % 3, 0);
        }

        let structure = derive_family_structure(&assembly).expect("structure");
        assert_eq!(structure.spines.len(), assembly.axes.len());
        // Trunk, branch, root, leaf.
        assert_eq!(structure.parts.len(), 4);
        assert!(structure.dimensions.height.bits() > 0);
        assert!(structure.dimensions.root_radius[0].bits() > 0);
        // A branch part hangs off the trunk part, and the leaves off the branches.
        let part = |element: BotanicalElement| {
            structure
                .parts
                .iter()
                .find(|part| part.id == class_part_id(element))
                .expect("part")
        };
        assert_eq!(
            part(BotanicalElement::Branch).parent,
            Some(class_part_id(BotanicalElement::Trunk))
        );
        assert_eq!(
            part(BotanicalElement::Leaf).parent,
            Some(class_part_id(BotanicalElement::Branch))
        );
        assert_eq!(part(BotanicalElement::Trunk).parent, None);
    }

    /// Every declared variation becomes its own family variation naming its own geometry, and the
    /// family's dimensions contain all of them.
    #[test]
    fn variations_each_carry_their_own_geometry() {
        let mut document = crate::BotanicalGraphDocument::sapling(0x5a11);
        document.variations.push(crate::BotanicalVariation {
            seed: 0x5a11,
            age: UnitInterval::from_bits(20_000),
            name: "Sapling".to_owned(),
        });
        let rows = native_variations(&document);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Mature");
        assert_eq!(rows[1].name, "Sapling");
        assert_ne!(rows[0].sources, rows[1].sources, "one source each");

        let grown: Vec<BotanicalAssembly> = (0..2)
            .map(|index| {
                grow(
                    &document,
                    index,
                    &crate::NoBotanicalModules,
                    &crate::BotanicalBudget::COOK,
                )
                .unwrap()
                .assembly
            })
            .collect();
        let widest = widest_family_structure(&grown).expect("structure");
        let mature = derive_family_structure(&grown[0]).expect("mature structure");
        assert_eq!(
            widest.dimensions.height, mature.dimensions.height,
            "the tallest variation sets the family height"
        );
        assert_eq!(
            widest.spines.len(),
            mature.spines.len(),
            "one skeleton declaration: every variation is the same structure at another size"
        );
    }

    /// A native family's proxies are derived from what it grew: a capsule per axis thick enough to
    /// collide with, and one footprint on the navigation seam.
    #[test]
    fn proxies_are_derived_from_the_grown_plant() {
        let document = crate::BotanicalGraphDocument::sapling(0x5a11);
        let assembly = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .unwrap()
        .assembly;
        let structure = derive_family_structure(&assembly).unwrap();
        let (collision, navigation) = derive_family_proxies(&assembly, &structure.dimensions);

        assert!(!collision.is_empty());
        assert!(collision.len() <= MAX_DERIVED_COLLISION_PROXIES);
        assert!(
            collision
                .iter()
                .all(|proxy| proxy.dimensions.iter().all(|value| value.bits() > 0)),
            "every capsule has extent"
        );
        assert!(
            !collision
                .iter()
                .any(|proxy| proxy.part == class_part_id(BotanicalElement::Root)),
            "nothing walks into a root"
        );
        let trunk = collision
            .iter()
            .find(|proxy| proxy.part == class_part_id(BotanicalElement::Trunk))
            .expect("the trunk collides");
        assert!(!trunk.breakable, "breaking the trunk fells the plant");
        // Ids are the axis identities, so a proxy survives a parameter change like an edit does.
        let ids: BTreeMap<u128, ()> = collision.iter().map(|proxy| (proxy.id, ())).collect();
        assert_eq!(ids.len(), collision.len());

        assert_eq!(navigation.len(), 1);
        assert_eq!(navigation[0].footprint.len(), 8);
        assert_eq!(navigation[0].height, structure.dimensions.height);

        // A family built the usual way carries them, and the whole thing validates.
        let family = native_plant_family(
            saffron_core::Uuid(4_242),
            "Derived",
            document,
            vec![saffron_core::Uuid(1), saffron_core::Uuid(2)],
            &crate::NoBotanicalModules,
        )
        .expect("the family builds");
        assert_eq!(family.collision_proxies, collision);
        assert_eq!(family.navigation_proxies, navigation);
        crate::validate_plant_family(&family).expect("a derived family is valid");
    }

    /// A phenotype is a selection over the classes a variation actually grew, so a family without
    /// flowers has no flowering appearance to offer.
    #[test]
    fn phenotypes_follow_the_classes_a_variation_grew() {
        let document = crate::BotanicalGraphDocument::sapling(0x5a11);
        let grown = vec![
            grow(
                &document,
                0,
                &crate::NoBotanicalModules,
                &crate::BotanicalBudget::COOK,
            )
            .unwrap()
            .assembly,
        ];
        let roles: Vec<crate::PhenotypeRole> = native_phenotypes(&grown)
            .iter()
            .map(|phenotype| phenotype.role)
            .collect();
        assert_eq!(
            roles,
            vec![crate::PhenotypeRole::Healthy, crate::PhenotypeRole::Dead],
            "leaves can be lost; there are no flowers or fruit to gain"
        );

        // Add fruit and the harvest appearances appear with it.
        let mut fruiting = document.clone();
        for node in &mut fruiting.nodes {
            if let crate::BotanicalOperator::Instance { element, .. } = &mut node.operator {
                *element = BotanicalElement::Fruit;
            }
        }
        let grown = vec![
            grow(
                &fruiting,
                0,
                &crate::NoBotanicalModules,
                &crate::BotanicalBudget::COOK,
            )
            .unwrap()
            .assembly,
        ];
        let phenotypes = native_phenotypes(&grown);
        let roles: Vec<crate::PhenotypeRole> =
            phenotypes.iter().map(|phenotype| phenotype.role).collect();
        assert_eq!(
            roles,
            vec![
                crate::PhenotypeRole::Healthy,
                crate::PhenotypeRole::Fruiting,
                crate::PhenotypeRole::Harvested,
                crate::PhenotypeRole::Dead,
            ]
        );
        let harvested = phenotypes
            .iter()
            .find(|phenotype| phenotype.role == crate::PhenotypeRole::Harvested)
            .expect("a harvested appearance");
        assert!(
            !harvested
                .active_parts
                .contains(&class_part_id(BotanicalElement::Fruit)),
            "a harvested plant has had its fruit taken"
        );
        assert!(
            harvested
                .active_parts
                .contains(&class_part_id(BotanicalElement::Trunk)),
            "and keeps its trunk"
        );
        // Identities are unique family-wide across variations.
        let ids: BTreeMap<u32, ()> = phenotypes
            .iter()
            .map(|phenotype| (phenotype.id, ()))
            .collect();
        assert_eq!(ids.len(), phenotypes.len());
    }

    /// Generation is a pure function of the assembly: the same graph compiles to the same bytes.
    #[test]
    fn generation_is_reproducible() {
        let document = crate::botanical::tests_support::birch();
        let first = normalize_botanical_geometry(
            7,
            &grow(
                &document,
                0,
                &crate::NoBotanicalModules,
                &crate::BotanicalBudget::COOK,
            )
            .unwrap()
            .assembly,
            &BTreeMap::new(),
        )
        .unwrap();
        let again = normalize_botanical_geometry(
            7,
            &grow(
                &document,
                0,
                &crate::NoBotanicalModules,
                &crate::BotanicalBudget::COOK,
            )
            .unwrap()
            .assembly,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(first, again);
    }

    /// An empty assembly is refused rather than compiled into a family with no plant in it.
    #[test]
    fn an_empty_assembly_has_no_structure() {
        assert!(derive_family_structure(&BotanicalAssembly::default()).is_err());
        let empty =
            normalize_botanical_geometry(7, &BotanicalAssembly::default(), &BTreeMap::new())
                .unwrap();
        assert!(empty.meshes.is_empty() && empty.joints.is_empty());
    }

    /// A created native family is immediately valid, carries the plant's own structure, and
    /// compiles — the authoring front door produces something real, not a stub to fill in.
    #[test]
    fn a_created_native_family_is_valid_and_grown() {
        let materials = vec![saffron_core::Uuid(11), saffron_core::Uuid(12)];
        let family = native_plant_family(
            saffron_core::Uuid(41),
            "Sapling",
            crate::BotanicalGraphDocument::sapling(0x5a11),
            materials,
            &crate::NoBotanicalModules,
        )
        .expect("the sapling becomes a family");
        crate::validate_plant_family(&family).expect("a created family validates");
        assert!(!family.spines.is_empty());
        assert!(
            family
                .parts
                .iter()
                .any(|part| { part.semantic == crate::PlantPartSemantic::Trunk })
        );
        // Round-trips through the canonical container unchanged.
        let bytes = crate::write_plant_asset(&family).expect("write");
        let decoded = crate::read_plant_asset(&bytes).expect("read");
        assert_eq!(decoded, family);

        // Too few material slots is refused rather than silently binding slot zero twice.
        assert!(
            native_plant_family(
                saffron_core::Uuid(41),
                "Sapling",
                crate::BotanicalGraphDocument::sapling(0x5a11),
                vec![saffron_core::Uuid(11)],
                &crate::NoBotanicalModules,
            )
            .is_err()
        );
    }

    /// A different seed grows a different individual, and its geometry differs with it.
    #[test]
    fn a_different_individual_compiles_differently() {
        let document = crate::botanical::tests_support::birch();
        let mut other: BotanicalGraphDocument = document.clone();
        other.variations[0].seed += 1;
        let first = normalize_botanical_geometry(
            7,
            &grow(
                &document,
                0,
                &crate::NoBotanicalModules,
                &crate::BotanicalBudget::COOK,
            )
            .unwrap()
            .assembly,
            &BTreeMap::new(),
        )
        .unwrap();
        let second = normalize_botanical_geometry(
            7,
            &grow(
                &other,
                0,
                &crate::NoBotanicalModules,
                &crate::BotanicalBudget::COOK,
            )
            .unwrap()
            .assembly,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_ne!(first.meshes, second.meshes);
    }
}
