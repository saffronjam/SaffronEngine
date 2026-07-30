use std::collections::BTreeMap;

use saffron_spatial::UnitInterval;

use crate::{
    BotanicalAssembly, BotanicalAxis, BotanicalElement, BotanicalElementId, BotanicalFrame,
    BotanicalGraft, BotanicalPlacement, BotanicalShell, Error, NormalizedPlantJoint,
    NormalizedPlantMesh, NormalizedPlantSkin, NormalizedPlantSubmesh, NormalizedPlantVertex,
    PlantSourceRole, PlantSourceSelector, Result, isqrt, turn_sin_cos,
};

/// The vertex, skin, and index buffers one material slot accumulates.
type MeshBuffers = (
    Vec<NormalizedPlantVertex>,
    Vec<NormalizedPlantSkin>,
    Vec<u32>,
);

/// Vertex ceiling for one generated family.
pub const MAX_GENERATED_VERTICES: usize = 1 << 20;

/// One in Q15.16, the fixed-point scale every botanical position and direction uses.
const Q16: i64 = 1 << 16;

/// The compiled geometry one assembly generates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BotanicalGeometry {
    /// One mesh carrying every generated surface, with material-homogeneous submeshes.
    pub meshes: Vec<NormalizedPlantMesh>,
    /// Structural joints, one per axis, in canonical identity order.
    pub joints: Vec<NormalizedPlantJoint>,
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
        return Err(field("vertices"));
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

/// A vector scaled to unit length in Q15.16.
fn unit_q16(vector: [i64; 3]) -> [i64; 3] {
    let magnitude = isqrt(vector.iter().map(|lane| lane * lane).sum::<i64>()).max(1);
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

/// Stands one normalized graft mesh up on its frame: the frame's outward direction becomes the
/// mesh's up axis, so a hero branch modelled growing upward grows outward along the limb it
/// replaces. Every step is integer, because a graft that moved by a bit between targets would
/// break the family's content hash.
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
    let snorm = |value: i64| i16::try_from(value.clamp(-32_767, 32_767)).unwrap_or(0);

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
                normal_snorm: std::array::from_fn(|lane| snorm(normal[lane])),
                uv_bits: vertex.uv_bits,
                tangent_snorm: [
                    snorm(tangent[0]),
                    snorm(tangent[1]),
                    snorm(tangent[2]),
                    vertex.tangent_snorm[3],
                ],
            }
        })
        .collect::<Vec<_>>();
    if vertices.len() > MAX_GENERATED_VERTICES {
        return Err(field("graft.vertices"));
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
        i32::try_from(component * length / isqrt(magnitude).max(1)).unwrap_or(i32::MAX)
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

fn field(name: &str) -> Error {
    Error::ArtifactFormat {
        format: "botanical family",
        field: name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BotanicalBudget, BotanicalGraphDocument, NoBotanicalModules, grow};

    fn birch_geometry(document: &BotanicalGraphDocument) -> BotanicalGeometry {
        let assembly = grow(document, 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .expect("the birch grows")
            .assembly;
        normalize_botanical_geometry(7, &assembly, &BTreeMap::new()).expect("geometry")
    }

    /// The native family generates real geometry: a closed tube per swept axis, a quad per
    /// instanced element, one submesh per material slot, and one structural joint per axis.
    #[test]
    fn a_grown_plant_generates_geometry_and_joints() {
        let document = crate::botanical::tests_support::birch();
        let assembly = grow(&document, 0, &NoBotanicalModules, &BotanicalBudget::COOK)
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
    }

    /// Generation is a pure function of the assembly: the same graph compiles to the same bytes.
    #[test]
    fn generation_is_reproducible() {
        let document = crate::botanical::tests_support::birch();
        assert_eq!(birch_geometry(&document), birch_geometry(&document));
    }

    /// A different seed grows a different individual, and its geometry differs with it.
    #[test]
    fn a_different_individual_compiles_differently() {
        let document = crate::botanical::tests_support::birch();
        let mut other = document.clone();
        other.variations[0].seed += 1;
        assert_ne!(
            birch_geometry(&document).meshes,
            birch_geometry(&other).meshes
        );
    }

    /// An empty assembly compiles to nothing rather than to a family with no plant in it.
    #[test]
    fn an_empty_assembly_generates_nothing() {
        let empty =
            normalize_botanical_geometry(7, &BotanicalAssembly::default(), &BTreeMap::new())
                .unwrap();
        assert!(empty.meshes.is_empty() && empty.joints.is_empty());
    }
}
