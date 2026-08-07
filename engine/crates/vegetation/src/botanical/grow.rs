use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::{DecisionScalar, RandomDomain, RandomStream, UnitInterval, WorldCellKey};

use crate::{BotanicalEditDiagnostics, Error, GraphCancellationToken, Result, apply_manual_edits};

use super::assembly::{
    BotanicalAssembly, BotanicalAxis, BotanicalElementId, BotanicalFrame, BotanicalPlacement,
    BotanicalShell,
};
use super::document::BotanicalGraphDocument;
use super::operator::{BotanicalElement, BotanicalOperator, MAX_AXES, PhyllotaxisPattern, field};
use super::shape::{
    AxisSeed, bend, cone_direction, interpolate, linear_taper, prune, sample_axis, straight_axis,
    turn_sin_cos,
};

/// Random channels, one per stochastic decision, so adding a rule cannot perturb another's stream.
mod channel {
    /// Jitter on a child axis's angle around its parent.
    pub const AZIMUTH: u32 = 1;
    /// Jitter on a child axis's declination from its parent.
    pub const DECLINATION: u32 = 2;
    /// Jitter on a child axis's length.
    pub const LENGTH: u32 = 3;
    /// Jitter on an instanced element's roll.
    pub const ROLL: u32 = 4;
}

/// Q15.16 one: the scale at which a module places at its authored size.
const DECISION_ONE: i32 = 1 << 16;

/// A grown plant and what its manual edit layer did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BotanicalGrowth {
    /// The plant, with every edit that found its target already applied.
    pub assembly: BotanicalAssembly,
    /// Edits that landed, and the ones that had nothing to land on.
    pub diagnostics: BotanicalEditDiagnostics,
    /// Whether a preview budget stopped the walk before the graph finished. A truncated growth is a
    /// picture, never a family; [`BotanicalBudget::COOK`] can never set it.
    pub truncated: bool,
}

/// How much of a graph one growth may build.
///
/// A bound only ever *stops* the walk early — it never changes an element that was built — so a
/// preview is a prefix of the cooked result rather than a different plant.
#[derive(Clone, Debug, Default)]
pub struct BotanicalBudget {
    /// Axes to build before stopping, or `None` for the authored graph's own ceiling.
    pub axes: Option<usize>,
    /// Placed elements to build before stopping, or `None` for no additional bound.
    pub elements: Option<usize>,
    /// Cooperative cancellation, checked once per node.
    pub cancellation: Option<GraphCancellationToken>,
}

impl BotanicalBudget {
    /// The cook budget: the authored graph in full, with nothing to cancel it.
    pub const COOK: Self = Self {
        axes: None,
        elements: None,
        cancellation: None,
    };

    /// Whether the walk should stop now.
    fn spent(&self, axes: usize, elements: usize) -> bool {
        self.axes.is_some_and(|bound| axes >= bound)
            || self.elements.is_some_and(|bound| elements >= bound)
            || self
                .cancellation
                .as_ref()
                .is_some_and(GraphCancellationToken::is_cancelled)
    }
}

/// Supplies what a [`BotanicalOperator::ModuleCall`] names.
///
/// Recursion lives here rather than in [`grow`], because the depth bound and the cycle check need
/// the chain of assets a call reaches through, and `grow` sees one document at a time.
pub trait BotanicalModuleResolver {
    /// Grows the module the call site names, in the module's own local frame.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the call GUID names no reference, the module is missing, its role
    /// is not a module, the chain revisits an asset, or the depth bound is reached.
    fn grow_module(&self, call_guid: u128) -> Result<BotanicalModuleGrowth>;
}

/// One resolved module call: what it grew and how large to place it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalModuleGrowth {
    /// The module's assembly in its own local frame.
    pub assembly: BotanicalAssembly,
    /// Uniform scale applied when placing it, where one is the module's authored size.
    pub scale: DecisionScalar,
}

/// The resolver for a graph that may not call modules.
pub struct NoBotanicalModules;

impl BotanicalModuleResolver for NoBotanicalModules {
    fn grow_module(&self, _call_guid: u128) -> Result<BotanicalModuleGrowth> {
        Err(field("moduleCall.unsupported"))
    }
}

/// Grows one of the plants a graph describes and lays its manual edits over the result.
///
/// `variation` indexes [`BotanicalGraphDocument::variations`]; zero is the representative
/// individual. The walk is a topological pass in canonical GUID order, so the assembly is a pure
/// function of the document and the index.
///
/// # Errors
///
/// Propagates validation, fails when `variation` names no declared individual, and fails when the
/// graph would grow past [`MAX_AXES`].
pub fn grow(
    document: &BotanicalGraphDocument,
    variation: usize,
    modules: &dyn BotanicalModuleResolver,
    budget: &BotanicalBudget,
) -> Result<BotanicalGrowth> {
    let (mut assembly, truncated) = grow_nodes(document, variation, modules, budget)?;
    let diagnostics = apply_manual_edits(&mut assembly, &document.edits);
    Ok(BotanicalGrowth {
        assembly,
        diagnostics,
        truncated,
    })
}

/// Grows what the nodes alone describe, before any manual edit.
fn grow_nodes(
    document: &BotanicalGraphDocument,
    variation: usize,
    modules: &dyn BotanicalModuleResolver,
    budget: &BotanicalBudget,
) -> Result<(BotanicalAssembly, bool)> {
    document.validate()?;
    let individual = document
        .variations
        .get(variation)
        .ok_or_else(|| field("variations.index"))?;
    let order = document.topological_order()?;
    let map = leading_u128(document.identity().bytes());
    // Age scales lengths and radii continuously; the structure the graph describes does not change.
    let scaled_by_age = |value: DecisionScalar| -> i32 {
        i32::try_from(
            (i64::from(value.bits()) * i64::from(individual.age.bits()))
                / i64::from(UnitInterval::ONE.bits()),
        )
        .unwrap_or(i32::MAX)
    };
    // A length or a radius never reaches zero — a plant with no extent is not a young plant.
    let aged = |value: DecisionScalar| DecisionScalar::from_bits(scaled_by_age(value).max(1));
    // A position scales about the origin and keeps its sign, so a drawn curve keeps its shape.
    let aged_position = |value: DecisionScalar| DecisionScalar::from_bits(scaled_by_age(value));

    let mut axes_out: BTreeMap<u128, Vec<BotanicalAxis>> = BTreeMap::new();
    let mut frames_out: BTreeMap<u128, Vec<BotanicalFrame>> = BTreeMap::new();
    let mut shells_out: BTreeMap<u128, Vec<BotanicalShell>> = BTreeMap::new();
    let mut elements_out: BTreeMap<u128, Vec<BotanicalPlacement>> = BTreeMap::new();
    // A node that transforms axes hands back the same identities carrying different geometry, so an
    // axis is collected only from the node that fed it into the family — a rescan of every node's
    // output could not tell the pre-transform copy apart.
    let mut family_axes: BTreeMap<BotanicalElementId, BotanicalAxis> = BTreeMap::new();
    let mut frame_axes: BTreeMap<BotanicalElementId, BotanicalAxis> = BTreeMap::new();
    let mut assembly = BotanicalAssembly::default();
    let mut truncated = false;

    for guid in order {
        let node = document.node(guid).ok_or_else(|| field("nodes.guid"))?;
        let incoming = |pin: &str| -> Vec<u128> {
            document
                .edges
                .iter()
                .filter(|edge| edge.to_node == guid && edge.to_pin == pin)
                .map(|edge| edge.from_node)
                .collect()
        };
        let input_axes = |pin: &str| -> Vec<BotanicalAxis> {
            incoming(pin)
                .iter()
                .filter_map(|from| axes_out.get(from))
                .flatten()
                .cloned()
                .collect()
        };
        let input_frames = |pin: &str| -> Vec<BotanicalFrame> {
            incoming(pin)
                .iter()
                .filter_map(|from| frames_out.get(from))
                .flatten()
                .copied()
                .collect()
        };

        match &node.operator {
            BotanicalOperator::Drawn { element, points } => {
                axes_out.insert(
                    guid,
                    vec![BotanicalAxis {
                        id: BotanicalElementId::root(guid, 0),
                        parent: None,
                        frame: None,
                        element: *element,
                        points: points
                            .iter()
                            .map(|point| point.position.map(aged_position))
                            .collect(),
                        radii: points.iter().map(|point| aged(point.radius)).collect(),
                    }],
                );
            }
            BotanicalOperator::Trunk {
                element,
                length,
                base_radius,
                taper,
                segments,
            } => {
                axes_out.insert(
                    guid,
                    vec![straight_axis(AxisSeed {
                        id: BotanicalElementId::root(guid, 0),
                        parent: None,
                        element: *element,
                        base: [DecisionScalar::from_bits(0); 3],
                        direction: [0, 1, 0],
                        length: aged(*length),
                        base_radius: aged(*base_radius),
                        taper,
                        segments: *segments,
                    })?],
                );
            }
            BotanicalOperator::Branch {
                element,
                length_ratio,
                radius_ratio,
                declination,
                jitter,
                segments,
            } => {
                let frames = input_frames("frames");
                let taper = linear_taper();
                let mut grown = Vec::new();
                for (ordinal, frame) in frames.iter().enumerate() {
                    let ordinal = u32::try_from(ordinal).map_err(|_| Error::NumericOverflow)?;
                    let stream = |ch: u32| {
                        RandomStream::new(RandomDomain {
                            map,
                            node_guid: guid,
                            node_semantic_revision: node.semantic_revision,
                            seed_namespace: individual.seed,
                            cell: WorldCellKey::base(0, 0, 0),
                            candidate: frame.id.value() as u64,
                            ancestor: 0,
                            species: 0,
                            channel: ch,
                        })
                    };
                    // Length and direction vary per child, keyed by the frame it grows from, so
                    // adding a sibling node cannot shift an existing branch.
                    let vary = |channel: u32, base: i64| -> i64 {
                        if jitter.bits() == 0 {
                            return base;
                        }
                        let sample = i64::from(stream(channel).lane(0, 0) % 2_048) - 1_024;
                        base + base * sample * i64::from(jitter.bits())
                            / (1_024 * i64::from(UnitInterval::ONE.bits()))
                    };
                    let parent_length = i64::from(frame.radius.bits()).max(1);
                    let length = DecisionScalar::from_bits(
                        i32::try_from(
                            vary(
                                channel::LENGTH,
                                i64::from(frame.radius.bits())
                                    * i64::from(length_ratio.bits())
                                    * 24
                                    / i64::from(UnitInterval::ONE.bits()),
                            )
                            .max(parent_length),
                        )
                        .unwrap_or(i32::MAX),
                    );
                    let declination_bits =
                        vary(channel::DECLINATION, i64::from(declination.bits()));
                    let azimuth = stream(channel::AZIMUTH).lane(0, 1) % 4_096;
                    let direction =
                        cone_direction(frame.direction, declination_bits, i64::from(azimuth));
                    let radius = DecisionScalar::from_bits(
                        (i64::from(frame.radius.bits()) * i64::from(radius_ratio.bits())
                            / i64::from(UnitInterval::ONE.bits())) as i32,
                    );
                    let mut axis = straight_axis(AxisSeed {
                        id: frame.id.child(guid, ordinal),
                        parent: Some(frame.axis),
                        element: *element,
                        base: frame.position,
                        direction,
                        length,
                        base_radius: radius.max(DecisionScalar::from_bits(1)),
                        taper: &taper,
                        segments: *segments,
                    })?;
                    axis.frame = Some(frame.id);
                    grown.push(axis);
                    if grown.len() > MAX_AXES {
                        return Err(field("branch.axes"));
                    }
                }
                axes_out.insert(guid, grown);
            }
            BotanicalOperator::Phyllotaxis {
                pattern,
                count,
                nodes,
                start,
                end,
                divergence,
            } => {
                let axes = input_axes("axes");
                for axis in &axes {
                    frame_axes.insert(axis.id, axis.clone());
                }
                let mut frames = Vec::new();
                for axis in &axes {
                    let per_node = match pattern {
                        PhyllotaxisPattern::Alternate | PhyllotaxisPattern::Spiral => 1,
                        PhyllotaxisPattern::Opposite => 2,
                        PhyllotaxisPattern::Whorled => *count,
                    };
                    let mut ordinal = 0_u32;
                    for step in 0..*nodes {
                        let along = interpolate(*start, *end, step, *nodes);
                        for lane in 0..per_node {
                            let turn = i64::from(divergence.bits()) * i64::from(ordinal)
                                + i64::from(UnitInterval::ONE.bits()) * i64::from(lane)
                                    / i64::from(per_node);
                            let (position, direction, radius) = sample_axis(axis, along, turn)?;
                            frames.push(BotanicalFrame {
                                id: axis.id.child(guid, ordinal),
                                axis: axis.id,
                                position,
                                direction,
                                radius,
                                along,
                            });
                            ordinal += 1;
                        }
                    }
                }
                frames_out.insert(guid, frames);
            }
            BotanicalOperator::Tropism {
                kind,
                strength,
                stimulus,
                plane_offset,
            } => {
                let mut axes = input_axes("axes");
                for axis in &mut axes {
                    bend(axis, *kind, *strength, *stimulus, *plane_offset);
                }
                axes_out.insert(guid, axes);
            }
            BotanicalOperator::Prune {
                rule,
                threshold,
                count,
            } => {
                let axes = input_axes("axes");
                axes_out.insert(guid, prune(axes, *rule, *threshold, *count));
            }
            BotanicalOperator::Roots {
                depth_ratio,
                spread_ratio,
                count,
            } => {
                let axes = input_axes("axes");
                let taper = linear_taper();
                let mut roots = Vec::new();
                for source in &axes {
                    let length = source.length();
                    let depth = DecisionScalar::from_bits(
                        (i64::from(length.bits()) * i64::from(depth_ratio.bits())
                            / i64::from(UnitInterval::ONE.bits())) as i32,
                    );
                    let spread = i64::from(depth.bits()) * i64::from(spread_ratio.bits())
                        / i64::from(UnitInterval::ONE.bits());
                    for ordinal in 0..*count {
                        let turn = i64::from(UnitInterval::ONE.bits()) * i64::from(ordinal)
                            / i64::from(*count);
                        let (sin, cos) = turn_sin_cos(turn);
                        let direction = [
                            (spread * cos / i64::from(UnitInterval::ONE.bits())) as i32,
                            -i32::try_from(i64::from(depth.bits()).min(i64::from(i32::MAX)))
                                .unwrap_or(i32::MAX),
                            (spread * sin / i64::from(UnitInterval::ONE.bits())) as i32,
                        ];
                        roots.push(straight_axis(AxisSeed {
                            id: source.id.child(guid, ordinal),
                            parent: Some(source.id),
                            element: BotanicalElement::Root,
                            base: source.points[0],
                            direction,
                            length: depth,
                            base_radius: source.radii[0],
                            taper: &taper,
                            segments: 4,
                        })?);
                    }
                }
                axes_out.insert(guid, roots);
            }
            BotanicalOperator::Shell {
                material_slot,
                sides,
            } => {
                let axes = input_axes("axes");
                let shells: Vec<BotanicalShell> = axes
                    .iter()
                    .map(|axis| BotanicalShell {
                        id: axis.id.child(guid, 0),
                        axis: axis.id,
                        element: axis.element,
                        material_slot: *material_slot,
                        sides: *sides,
                    })
                    .collect();
                // Shells describe axes, so the axes they sweep travel with them.
                for axis in axes {
                    family_axes.insert(axis.id, axis);
                }
                shells_out.insert(guid, shells);
            }
            BotanicalOperator::Instance {
                element,
                material_slot,
                size,
                jitter,
            } => {
                let frames = input_frames("frames");
                let placed: Vec<BotanicalPlacement> = frames
                    .iter()
                    .map(|frame| {
                        let roll = if jitter.bits() == 0 {
                            UnitInterval::ZERO
                        } else {
                            let sample = RandomStream::new(RandomDomain {
                                map,
                                node_guid: guid,
                                node_semantic_revision: node.semantic_revision,
                                seed_namespace: individual.seed,
                                cell: WorldCellKey::base(0, 0, 0),
                                candidate: frame.id.value() as u64,
                                ancestor: 0,
                                species: 0,
                                channel: channel::ROLL,
                            })
                            .lane(0, 0);
                            UnitInterval::from_bits(
                                (u32::from(jitter.bits()) * (sample % 65_536) / 65_535) as u16,
                            )
                        };
                        BotanicalPlacement {
                            id: frame.id.child(guid, 0),
                            frame: frame.id,
                            element: *element,
                            material_slot: *material_slot,
                            position: frame.position,
                            size: aged(*size),
                            roll,
                        }
                    })
                    .collect();
                elements_out.insert(guid, placed);
            }
            BotanicalOperator::ModuleCall { call_guid } => {
                let grown = modules.grow_module(*call_guid)?;
                // Identities rebase through the call GUID and the frame ordinal, so two call sites
                // of one module address different elements and an authored edit still names the
                // element it was made on.
                let mut shells = Vec::new();
                let mut elements = Vec::new();
                for (ordinal, frame) in input_frames("frames").iter().enumerate() {
                    let ordinal = u32::try_from(ordinal).map_err(|_| Error::NumericOverflow)?;
                    let rebase = |id: BotanicalElementId| id.child(*call_guid, ordinal);
                    let place = |value: DecisionScalar| -> DecisionScalar {
                        DecisionScalar::from_bits(
                            i32::try_from(
                                (i64::from(scaled_by_age(value)) * i64::from(grown.scale.bits()))
                                    / i64::from(DECISION_ONE),
                            )
                            .unwrap_or(i32::MAX),
                        )
                    };
                    let offset = |point: [DecisionScalar; 3]| -> [DecisionScalar; 3] {
                        [0, 1, 2].map(|axis| {
                            DecisionScalar::from_bits(
                                place(point[axis])
                                    .bits()
                                    .saturating_add(frame.position[axis].bits()),
                            )
                        })
                    };
                    for axis in &grown.assembly.axes {
                        family_axes.insert(
                            rebase(axis.id),
                            BotanicalAxis {
                                id: rebase(axis.id),
                                parent: axis.parent.map(rebase),
                                frame: axis.frame.map(rebase),
                                element: axis.element,
                                points: axis.points.iter().map(|point| offset(*point)).collect(),
                                radii: axis.radii.iter().map(|radius| place(*radius)).collect(),
                            },
                        );
                    }
                    for module_frame in &grown.assembly.frames {
                        frame_axes.remove(&rebase(module_frame.id));
                        assembly.frames.push(BotanicalFrame {
                            id: rebase(module_frame.id),
                            axis: rebase(module_frame.axis),
                            position: offset(module_frame.position),
                            direction: module_frame.direction,
                            radius: place(module_frame.radius),
                            along: module_frame.along,
                        });
                    }
                    shells.extend(grown.assembly.shells.iter().map(|shell| BotanicalShell {
                        id: rebase(shell.id),
                        axis: rebase(shell.axis),
                        element: shell.element,
                        material_slot: shell.material_slot,
                        sides: shell.sides,
                    }));
                    elements.extend(grown.assembly.elements.iter().map(|element| {
                        BotanicalPlacement {
                            id: rebase(element.id),
                            frame: rebase(element.frame),
                            element: element.element,
                            material_slot: element.material_slot,
                            position: offset(element.position),
                            size: place(element.size),
                            roll: element.roll,
                        }
                    }));
                }
                shells_out.insert(guid, shells);
                elements_out.insert(guid, elements);
            }
            BotanicalOperator::Family => {
                for from in incoming("shells") {
                    if let Some(shells) = shells_out.get(&from) {
                        assembly.shells.extend(shells.iter().cloned());
                    }
                }
                for from in incoming("elements") {
                    if let Some(elements) = elements_out.get(&from) {
                        assembly.elements.extend(elements.iter().copied());
                    }
                }
            }
        }
        if axes_out.values().map(Vec::len).sum::<usize>() > MAX_AXES {
            return Err(field("axes"));
        }
        // The budget stops the walk between nodes, never inside one, so a node that ran produced
        // exactly what it would have produced unbounded.
        if budget.spent(
            axes_out.values().map(Vec::len).sum::<usize>(),
            elements_out.values().map(Vec::len).sum::<usize>(),
        ) {
            truncated = true;
            break;
        }
    }

    // The frames a placed element sits on come along too, so a compiled part can find its parent.
    let placed_frames: BTreeSet<BotanicalElementId> = assembly
        .elements
        .iter()
        .map(|element| element.frame)
        .collect();
    assembly.frames = frames_out
        .values()
        .flatten()
        .filter(|frame| placed_frames.contains(&frame.id))
        .copied()
        .collect();
    for frame in &assembly.frames {
        if let Some(axis) = frame_axes.get(&frame.axis)
            && !family_axes.contains_key(&frame.axis)
        {
            family_axes.insert(frame.axis, axis.clone());
        }
    }
    assembly.axes = family_axes.into_values().collect();
    assembly.axes.sort_by_key(|axis| axis.id);
    assembly.frames.sort_by_key(|frame| frame.id);
    assembly.shells.sort_by_key(|shell| shell.id);
    assembly.elements.sort_by_key(|element| element.id);
    Ok((assembly, truncated))
}

/// The leading 16 bytes of a content hash, as the random domain's map identity.
fn leading_u128(bytes: [u8; 32]) -> u128 {
    let mut leading = [0_u8; 16];
    leading.copy_from_slice(&bytes[..16]);
    u128::from_be_bytes(leading)
}

#[cfg(test)]
mod tests {
    use super::super::document::BotanicalEdge;
    use super::super::operator::TropismKind;
    use super::super::tests_support::{birch, edge, node};
    use super::*;

    /// The graph grows a plant with the structure it describes, and the same graph grows it again
    /// identically — the property every downstream cache and every save file depends on.
    #[test]
    fn a_graph_grows_the_same_plant_every_time() {
        let document = birch();
        let first = grow(&document, 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .expect("the birch grows");
        let again = grow(&document, 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .expect("the birch grows again");
        assert_eq!(first, again, "growing is a pure function of the document");
        let first = first.assembly;

        // A trunk, six branch frames each carrying a branch, four roots.
        let count = |element: BotanicalElement| {
            first
                .axes
                .iter()
                .filter(|axis| axis.element == element)
                .count()
        };
        assert_eq!(
            (
                count(BotanicalElement::Trunk),
                count(BotanicalElement::Branch),
                count(BotanicalElement::Root)
            ),
            (1, 6, 4)
        );
        // Every axis reaching the family is swept, and every branch carries four leaves.
        assert_eq!(first.shells.len(), 11);
        assert_eq!(first.elements.len(), 24);
        assert!(
            first
                .elements
                .iter()
                .all(|element| element.element == BotanicalElement::Leaf)
        );

        // Identity is structural, so no two grown elements collide.
        let ids: BTreeSet<u128> = first
            .axes
            .iter()
            .map(|axis| axis.id.value())
            .chain(first.elements.iter().map(|element| element.id.value()))
            .collect();
        assert_eq!(ids.len(), first.axes.len() + first.elements.len());
    }

    /// A different seed regrows a different individual, and the same seed the same one — which is
    /// what makes family variations coherent rather than random.
    #[test]
    fn the_seed_selects_the_individual() {
        let document = birch();
        let mut other = document.clone();
        other.variations[0].seed = 0xb17c5;
        let first = grow(&document, 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .unwrap()
            .assembly;
        let second = grow(&other, 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .unwrap()
            .assembly;
        assert_ne!(
            first.axes, second.axes,
            "a different seed grows a different individual"
        );
        assert_eq!(
            first.axes.len(),
            second.axes.len(),
            "but the same species: the structure the graph describes is unchanged"
        );
    }

    /// An element's identity is derived from its ancestry and ordinal, not from a counter, so a
    /// parameter change elsewhere leaves it addressable.
    #[test]
    fn element_identity_survives_an_unrelated_parameter_change() {
        let document = birch();
        let before = grow(&document, 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .unwrap()
            .assembly;
        let mut edited = document.clone();
        // Change how big the leaves are: the branches it hangs them on are untouched.
        for node in &mut edited.nodes {
            if let BotanicalOperator::Instance { size, .. } = &mut node.operator {
                *size = DecisionScalar::from_bits(8_000);
            }
        }
        let after = grow(&edited, 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .unwrap()
            .assembly;
        let axis_ids = |assembly: &BotanicalAssembly| -> Vec<u128> {
            assembly.axes.iter().map(|axis| axis.id.value()).collect()
        };
        assert_eq!(
            axis_ids(&before),
            axis_ids(&after),
            "every axis kept its identity"
        );
        assert_ne!(before.elements, after.elements, "the leaves did change");
    }

    /// A tropism bends the axis it is given and leaves its base where it was.
    #[test]
    fn a_tropism_bends_the_tip_and_not_the_base() {
        let mut document = birch();
        document.nodes.push(node(
            9,
            BotanicalOperator::Tropism {
                kind: TropismKind::Gravitropism,
                strength: UnitInterval::from_bits(40_000),
                stimulus: [DecisionScalar::from_bits(0); 3],
                plane_offset: DecisionScalar::from_bits(0),
            },
        ));
        // Branches droop, then feed the same frames and shells.
        document
            .edges
            .retain(|edge| !(edge.from_node == 3 && (edge.to_node == 4 || edge.to_node == 6)));
        document.edges.push(edge(3, "axes", 9, "axes"));
        document.edges.push(edge(9, "axes", 4, "axes"));
        document.edges.push(edge(9, "axes", 6, "axes"));
        document.edges.sort();

        let straight = grow(&birch(), 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .unwrap()
            .assembly;
        let drooped = grow(&document, 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .unwrap()
            .assembly;
        let branch_lane = |assembly: &BotanicalAssembly, tip: bool| -> Vec<i32> {
            assembly
                .axes
                .iter()
                .filter(|axis| axis.element == BotanicalElement::Branch)
                .map(|axis| {
                    if tip {
                        axis.points.last().expect("tip")[1].bits()
                    } else {
                        axis.points[0][1].bits()
                    }
                })
                .collect()
        };
        assert_eq!(
            branch_lane(&straight, false),
            branch_lane(&drooped, false),
            "the bases do not move"
        );
        assert!(
            branch_lane(&drooped, true)
                .iter()
                .zip(branch_lane(&straight, true))
                .all(|(drooped, straight)| *drooped <= straight),
            "gravity never lifts a tip"
        );
    }

    /// A resolver standing in for the asset layer: it grows one fixed module for every call.
    struct OneModule {
        scale: DecisionScalar,
    }

    impl BotanicalModuleResolver for OneModule {
        fn grow_module(&self, _call_guid: u128) -> Result<BotanicalModuleGrowth> {
            Ok(BotanicalModuleGrowth {
                assembly: grow(&birch(), 0, &NoBotanicalModules, &BotanicalBudget::COOK)?.assembly,
                scale: self.scale,
            })
        }
    }

    /// Replaces the birch's leaf placement with a module call on the same frames.
    fn birch_calling_a_module(call_guid: u128) -> BotanicalGraphDocument {
        let mut document = birch();
        let leaves = document
            .nodes
            .iter()
            .position(|node| matches!(node.operator, BotanicalOperator::Instance { .. }))
            .expect("the birch places leaves");
        document.nodes[leaves].operator = BotanicalOperator::ModuleCall { call_guid };
        // The family output took the leaves on its `elements` pin; a module also feeds shells.
        let family = document
            .nodes
            .iter()
            .find(|node| matches!(node.operator, BotanicalOperator::Family))
            .expect("the birch has a family output")
            .guid;
        let module = document.nodes[leaves].guid;
        document.edges.push(BotanicalEdge {
            from_node: module,
            from_pin: "shells".to_owned(),
            to_node: family,
            to_pin: "shells".to_owned(),
        });
        document.edges.sort();
        document
    }

    fn unit_scale() -> OneModule {
        OneModule {
            scale: DecisionScalar::from_integer(1).unwrap(),
        }
    }

    #[test]
    fn a_module_call_grows_the_module_at_every_frame() {
        let document = birch_calling_a_module(0x11);
        let alone = grow(&birch(), 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .expect("the birch grows")
            .assembly;
        let composed = grow(&document, 0, &unit_scale(), &BotanicalBudget::COOK)
            .expect("the composed birch grows")
            .assembly;
        // Each frame the leaves used now carries a whole birch, so the composed family holds
        // strictly more surface than either the caller or one copy of the module.
        assert!(composed.shells.len() > alone.shells.len());
        assert!(!composed.shells.is_empty());
    }

    #[test]
    fn a_module_places_at_its_call_sites_scale() {
        let document = birch_calling_a_module(0x11);
        let grow_at = |scale: i32| {
            grow(
                &document,
                0,
                &OneModule {
                    scale: DecisionScalar::from_integer(scale).unwrap(),
                },
                &BotanicalBudget::COOK,
            )
            .expect("the composed birch grows")
            .assembly
            .local_bounds()
        };
        let (_, small) = grow_at(1);
        let (_, large) = grow_at(4);
        assert!(large[1].bits() > small[1].bits());
    }

    /// Two copies of a preset must be separately editable: identities rebase through the call GUID,
    /// so an authored edit on one never moves the other.
    #[test]
    fn two_call_sites_of_one_module_address_different_elements() {
        let first = grow(
            &birch_calling_a_module(0x11),
            0,
            &unit_scale(),
            &BotanicalBudget::COOK,
        )
        .expect("the first call grows")
        .assembly;
        let second = grow(
            &birch_calling_a_module(0x22),
            0,
            &unit_scale(),
            &BotanicalBudget::COOK,
        )
        .expect("the second call grows")
        .assembly;
        let ids = |assembly: &BotanicalAssembly| {
            assembly
                .shells
                .iter()
                .map(|shell| shell.id)
                .collect::<BTreeSet<_>>()
        };
        // The caller's own shells are the same in both, so the comparison is over what the module
        // contributed.
        let caller = ids(
            &grow(&birch(), 0, &NoBotanicalModules, &BotanicalBudget::COOK)
                .expect("the birch grows")
                .assembly,
        );
        let from_first: BTreeSet<_> = ids(&first).difference(&caller).copied().collect();
        let from_second: BTreeSet<_> = ids(&second).difference(&caller).copied().collect();
        assert!(!from_first.is_empty());
        assert_eq!(from_first.len(), from_second.len());
        assert!(from_first.is_disjoint(&from_second));
    }

    /// A preview may stop early but never changes what it built, so an artist tunes against the
    /// plant rather than a picture of something else.
    #[test]
    fn a_bounded_preview_is_a_prefix_of_the_cooked_plant() {
        let document = birch();
        let full = grow(&document, 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .expect("the birch grows");
        assert!(!full.truncated);
        let bounded = grow(
            &document,
            0,
            &NoBotanicalModules,
            &BotanicalBudget {
                axes: Some(1),
                ..BotanicalBudget::default()
            },
        )
        .expect("the bounded birch grows");
        assert!(bounded.truncated, "the bound stopped the walk");
        assert!(bounded.assembly.axes.len() < full.assembly.axes.len());
        let cooked: BTreeMap<_, _> = full
            .assembly
            .axes
            .iter()
            .map(|axis| (axis.id, axis))
            .collect();
        for axis in &bounded.assembly.axes {
            assert_eq!(
                cooked.get(&axis.id).copied(),
                Some(axis),
                "a previewed axis differs from the cooked one"
            );
        }
    }

    /// No preview bound can reach the cooked result, because the cook path names a budget that
    /// carries none.
    #[test]
    fn the_cook_budget_can_never_truncate() {
        assert!(BotanicalBudget::COOK.axes.is_none());
        assert!(BotanicalBudget::COOK.elements.is_none());
        assert!(BotanicalBudget::COOK.cancellation.is_none());
        let full = grow(&birch(), 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .expect("the birch grows");
        assert!(!full.truncated);
    }

    /// Cancellation stops the walk between nodes, so what was built is still what the cook builds.
    #[test]
    fn a_cancelled_preview_stops_and_says_so() {
        let token = GraphCancellationToken::default();
        token.cancel();
        let growth = grow(
            &birch(),
            0,
            &NoBotanicalModules,
            &BotanicalBudget {
                cancellation: Some(token),
                ..BotanicalBudget::default()
            },
        )
        .expect("a cancelled growth reports rather than fails");
        assert!(growth.truncated);
        let full = grow(&birch(), 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .expect("the birch grows");
        assert!(growth.assembly.shells.len() < full.assembly.shells.len());
    }

    /// Growing a module-calling graph without its modules would report a plant missing its presets
    /// and call it a success, so the resolver refuses instead.
    #[test]
    fn a_graph_grown_without_modules_refuses_a_module_call() {
        assert!(
            grow(
                &birch_calling_a_module(0x11),
                0,
                &NoBotanicalModules,
                &BotanicalBudget::COOK
            )
            .is_err()
        );
    }
}
