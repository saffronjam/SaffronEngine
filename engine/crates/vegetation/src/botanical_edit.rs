//! Manual edits laid over what the botanical graph grows.
//!
//! Generators get a plant most of the way there. The last part is hand work: a branch nudged out of
//! a silhouette, a leaf turned to catch light, a limb cut short. That work must survive the artist
//! going back to the graph and changing a parameter, which rules out baking it into geometry.
//!
//! So an edit is authored data on the document, addressed by the *semantic identity* of what it
//! changes rather than by an index into anything. Growing is untouched: it runs exactly as if no
//! edit existed, and the layer applies to its result. A change that regrows the same identity keeps
//! its edit; a change that does not gets an orphan diagnostic naming the edit and why it could not
//! land. Nothing is silently dropped, and nothing is silently applied to the wrong element.

use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::{DecisionScalar, UnitInterval};

use crate::{
    BotanicalAssembly, BotanicalElementId, BotanicalGraft, Error, PlantSourceSelector, Result,
    turn_sin_cos,
};

/// Edits one document may carry. A bound, not a budget: a document past it is a mistake caught at
/// validation rather than a compile that never finishes.
pub const MAX_MANUAL_EDITS: usize = 4_096;

/// What one manual edit does to its target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BotanicalEditAction {
    /// Moves, turns, and resizes the target and everything it carries.
    Transform {
        /// Translation in family-local metres.
        offset: [DecisionScalar; 3],
        /// Turn about the vertical through the target's base, as a signed normalized half-turn.
        roll: UnitInterval,
        /// Uniform scale in Q15.16, where one is unchanged.
        scale: DecisionScalar,
    },
    /// Cuts an axis at a fraction of its length. Everything attached above the cut goes with it.
    Trim {
        /// Where along the axis the cut falls.
        at: UnitInterval,
    },
    /// Deletes the target and everything it carries.
    Remove,
    /// Substitutes a hand-modelled mesh for the generated element, standing on the same frame.
    Graft {
        /// The family's graft source that supplies the geometry.
        source: u128,
        /// Which of that source's elements to take.
        selector: PlantSourceSelector,
    },
}

impl BotanicalEditAction {
    /// Canonical wire tag, which is also the order two actions on one target sort in.
    #[must_use]
    pub const fn tag(&self) -> u32 {
        match self {
            Self::Transform { .. } => 0,
            Self::Trim { .. } => 1,
            Self::Remove => 2,
            Self::Graft { .. } => 3,
        }
    }

    /// Stable action name, used in diagnostics.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Transform { .. } => "transform",
            Self::Trim { .. } => "trim",
            Self::Remove => "remove",
            Self::Graft { .. } => "graft",
        }
    }
}

/// One authored change and the element identity it changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalManualEdit {
    /// The element the edit addresses.
    pub target: BotanicalElementId,
    /// What it does.
    pub action: BotanicalEditAction,
}

/// Why an authored edit did not apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotanicalEditOrphanReason {
    /// The graph no longer grows that identity.
    TargetMissing,
    /// The identity exists, but not as something the action can change.
    TargetKind,
    /// Another edit took the target away, or the axis it hung from.
    TargetRemoved,
}

impl BotanicalEditOrphanReason {
    /// Stable reason name, used in diagnostics.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TargetMissing => "targetMissing",
            Self::TargetKind => "targetKind",
            Self::TargetRemoved => "targetRemoved",
        }
    }
}

/// One edit that could not land, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalEditOrphan {
    /// The edit's target.
    pub target: BotanicalElementId,
    /// The edit's action.
    pub action: BotanicalEditAction,
    /// Why it did not apply.
    pub reason: BotanicalEditOrphanReason,
}

/// What the edit layer did to one grown assembly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BotanicalEditDiagnostics {
    /// Edits that landed.
    pub applied: u32,
    /// Edits that did not, in canonical target order.
    pub orphans: Vec<BotanicalEditOrphan>,
}

impl BotanicalEditDiagnostics {
    /// Whether every authored edit found its target.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.orphans.is_empty()
    }
}

/// Validates an edit layer: canonical order, one action of a kind per target, no contradiction, and
/// parameters that describe a change.
///
/// # Errors
///
/// [`Error::ArtifactFormat`] naming the exact field that failed.
pub fn validate_manual_edits(edits: &[BotanicalManualEdit]) -> Result<()> {
    let field = |name: &str| Error::ArtifactFormat {
        format: "botanical graph",
        field: name.to_owned(),
    };
    if edits.len() > MAX_MANUAL_EDITS {
        return Err(field("edits.count"));
    }
    for pair in edits.windows(2) {
        let order = (pair[0].target, pair[0].action.tag());
        if order >= (pair[1].target, pair[1].action.tag()) {
            return Err(field("edits.order"));
        }
    }
    let removed: BTreeSet<BotanicalElementId> = edits
        .iter()
        .filter(|edit| matches!(edit.action, BotanicalEditAction::Remove))
        .map(|edit| edit.target)
        .collect();
    for edit in edits {
        // Removing an element and also moving it says two things at once, and picking one silently
        // is how an artist loses work they can see in the panel.
        if !matches!(edit.action, BotanicalEditAction::Remove) && removed.contains(&edit.target) {
            return Err(field("edits.contradiction"));
        }
        match &edit.action {
            BotanicalEditAction::Transform { scale, .. } => {
                if scale.bits() <= 0 {
                    return Err(field("edits.transform.scale"));
                }
            }
            BotanicalEditAction::Trim { at } => {
                // A cut at the base is a removal and a cut at the tip is nothing; both have an
                // honest spelling already.
                if at.bits() == 0 || *at == UnitInterval::ONE {
                    return Err(field("edits.trim.at"));
                }
            }
            BotanicalEditAction::Graft { source, .. } => {
                // Which graft source it names is checked against the family's own list, which the
                // graph document cannot see from here.
                if *source == 0 {
                    return Err(field("edits.graft.source"));
                }
            }
            BotanicalEditAction::Remove => {}
        }
    }
    Ok(())
}

/// What the layer decided about one target before anything moved.
enum TargetKind {
    Axis,
    Placement,
}

/// Applies an edit layer to a grown assembly and reports what could not land.
///
/// Removals and cuts settle first, so no transform lands on something that is about to go. Axis
/// transforms then apply shallowest first, each about its own base as it stands at that moment, so
/// a limb moved at the trunk carries a leaf offset further out rather than fighting it.
pub fn apply_manual_edits(
    assembly: &mut BotanicalAssembly,
    edits: &[BotanicalManualEdit],
) -> BotanicalEditDiagnostics {
    let mut diagnostics = BotanicalEditDiagnostics::default();
    if edits.is_empty() {
        return diagnostics;
    }

    let axis_index: BTreeMap<BotanicalElementId, usize> = assembly
        .axes
        .iter()
        .enumerate()
        .map(|(index, axis)| (axis.id, index))
        .collect();
    let placement_index: BTreeMap<BotanicalElementId, usize> = assembly
        .elements
        .iter()
        .enumerate()
        .map(|(index, element)| (element.id, index))
        .collect();

    let mut resolved: Vec<(&BotanicalManualEdit, TargetKind)> = Vec::with_capacity(edits.len());
    for edit in edits {
        let kind = if axis_index.contains_key(&edit.target) {
            Some(TargetKind::Axis)
        } else if placement_index.contains_key(&edit.target) {
            Some(TargetKind::Placement)
        } else {
            None
        };
        let allowed = match (&kind, &edit.action) {
            (None, _) => Err(BotanicalEditOrphanReason::TargetMissing),
            // Trimming and grafting each name a shape of thing: a cut needs a curve to cut, and a
            // graft needs a frame to stand on, which only a placement carries.
            (Some(TargetKind::Axis), BotanicalEditAction::Graft { .. }) => {
                Err(BotanicalEditOrphanReason::TargetKind)
            }
            (Some(TargetKind::Axis), _) => Ok(TargetKind::Axis),
            (Some(TargetKind::Placement), BotanicalEditAction::Trim { .. }) => {
                Err(BotanicalEditOrphanReason::TargetKind)
            }
            (Some(TargetKind::Placement), _) => Ok(TargetKind::Placement),
        };
        match allowed {
            Ok(kind) => resolved.push((edit, kind)),
            Err(reason) => diagnostics.orphans.push(BotanicalEditOrphan {
                target: edit.target,
                action: edit.action.clone(),
                reason,
            }),
        }
    }

    let topology = Topology::of(assembly);
    let mut gone = Removed::default();
    for (edit, kind) in &resolved {
        match (kind, &edit.action) {
            (TargetKind::Axis, BotanicalEditAction::Remove) => {
                topology.take_axis(edit.target, &mut gone);
                diagnostics.applied += 1;
            }
            (TargetKind::Placement, BotanicalEditAction::Remove) => {
                gone.placements.insert(edit.target);
                diagnostics.applied += 1;
            }
            (TargetKind::Axis, BotanicalEditAction::Trim { at }) => {
                let cut = trim_axis(assembly, axis_index[&edit.target], *at);
                topology.take_above(edit.target, cut, &mut gone);
                diagnostics.applied += 1;
            }
            _ => {}
        }
    }

    // A graft substitutes geometry for one element and keeps its identity and its frame, so the
    // generated quad goes and the hand-modelled mesh stands in exactly where it stood.
    for (edit, kind) in &resolved {
        let BotanicalEditAction::Graft { source, selector } = &edit.action else {
            continue;
        };
        if !matches!(kind, TargetKind::Placement) {
            continue;
        }
        if gone.placements.contains(&edit.target) {
            diagnostics.orphans.push(BotanicalEditOrphan {
                target: edit.target,
                action: edit.action.clone(),
                reason: BotanicalEditOrphanReason::TargetRemoved,
            });
            continue;
        }
        let element = &assembly.elements[placement_index[&edit.target]];
        assembly.grafts.push(BotanicalGraft {
            id: element.id,
            frame: element.frame,
            source: *source,
            selector: selector.clone(),
            position: element.position,
            roll: element.roll,
        });
        gone.placements.insert(edit.target);
        diagnostics.applied += 1;
    }

    // Shallowest first: a transform is defined about its target's base as it stands when the
    // transform runs, so an outer move carries an inner one rather than the two disagreeing.
    let mut transforms: Vec<AxisTransform> = Vec::new();
    for (edit, kind) in &resolved {
        let BotanicalEditAction::Transform {
            offset,
            roll,
            scale,
        } = edit.action
        else {
            continue;
        };
        let removed = match kind {
            TargetKind::Axis => gone.axes.contains(&edit.target),
            // A grafted placement is gone from `elements`, but the graft standing in its place is
            // the thing an offset should move.
            TargetKind::Placement => {
                gone.placements.contains(&edit.target)
                    && !assembly.grafts.iter().any(|graft| graft.id == edit.target)
            }
        };
        if removed {
            diagnostics.orphans.push(BotanicalEditOrphan {
                target: edit.target,
                action: edit.action.clone(),
                reason: BotanicalEditOrphanReason::TargetRemoved,
            });
            continue;
        }
        match kind {
            TargetKind::Axis => transforms.push(AxisTransform {
                depth: topology.depth(edit.target),
                axis: edit.target,
                offset,
                roll,
                scale,
            }),
            TargetKind::Placement => {
                if let Some(graft) = assembly
                    .grafts
                    .iter_mut()
                    .find(|graft| graft.id == edit.target)
                {
                    for (component, shift) in graft.position.iter_mut().zip(offset) {
                        *component = DecisionScalar::from_bits(
                            component.bits().saturating_add(shift.bits()),
                        );
                    }
                    graft.roll = UnitInterval::from_bits(
                        ((u32::from(graft.roll.bits()) + u32::from(roll.bits())) % 65_536) as u16,
                    );
                } else {
                    let element = &mut assembly.elements[placement_index[&edit.target]];
                    for (component, shift) in element.position.iter_mut().zip(offset) {
                        *component = DecisionScalar::from_bits(
                            component.bits().saturating_add(shift.bits()),
                        );
                    }
                    element.roll = UnitInterval::from_bits(
                        ((u32::from(element.roll.bits()) + u32::from(roll.bits())) % 65_536) as u16,
                    );
                    element.size = scaled(element.size, scale);
                }
                diagnostics.applied += 1;
            }
        }
    }
    transforms.sort_by_key(|transform| (transform.depth, transform.axis));
    for transform in transforms {
        let subtree = topology.subtree(transform.axis, &gone);
        transform_subtree(assembly, &subtree, transform);
        diagnostics.applied += 1;
    }

    if !gone.is_empty() {
        assembly.axes.retain(|axis| !gone.axes.contains(&axis.id));
        assembly
            .frames
            .retain(|frame| !gone.frames.contains(&frame.id));
        assembly
            .elements
            .retain(|element| !gone.placements.contains(&element.id));
        assembly
            .shells
            .retain(|shell| !gone.axes.contains(&shell.axis));
    }

    diagnostics
        .orphans
        .sort_by_key(|orphan| (orphan.target, orphan.action.tag()));
    diagnostics
}

/// One pending axis transform, and how deep in the plant its target sits.
struct AxisTransform {
    depth: u32,
    axis: BotanicalElementId,
    offset: [DecisionScalar; 3],
    roll: UnitInterval,
    scale: DecisionScalar,
}

/// Everything an edit took away.
#[derive(Default)]
struct Removed {
    axes: BTreeSet<BotanicalElementId>,
    frames: BTreeSet<BotanicalElementId>,
    placements: BTreeSet<BotanicalElementId>,
}

impl Removed {
    fn is_empty(&self) -> bool {
        self.axes.is_empty() && self.frames.is_empty() && self.placements.is_empty()
    }
}

/// Who hangs off whom, so a cut or a move reaches everything it should.
struct Topology {
    axis_children: BTreeMap<BotanicalElementId, Vec<BotanicalElementId>>,
    axis_depth: BTreeMap<BotanicalElementId, u32>,
    /// Frames on an axis, with where along it each sits.
    axis_frames: BTreeMap<BotanicalElementId, Vec<(BotanicalElementId, UnitInterval)>>,
    frame_placements: BTreeMap<BotanicalElementId, Vec<BotanicalElementId>>,
    /// Axes grown from a frame, so a cut through the frame takes them.
    frame_axes: BTreeMap<BotanicalElementId, Vec<BotanicalElementId>>,
}

impl Topology {
    fn of(assembly: &BotanicalAssembly) -> Self {
        let mut topology = Self {
            axis_children: BTreeMap::new(),
            axis_depth: BTreeMap::new(),
            axis_frames: BTreeMap::new(),
            frame_placements: BTreeMap::new(),
            frame_axes: BTreeMap::new(),
        };
        for axis in &assembly.axes {
            if let Some(parent) = axis.parent {
                topology
                    .axis_children
                    .entry(parent)
                    .or_default()
                    .push(axis.id);
            }
            if let Some(frame) = axis.frame {
                topology.frame_axes.entry(frame).or_default().push(axis.id);
            }
        }
        for frame in &assembly.frames {
            topology
                .axis_frames
                .entry(frame.axis)
                .or_default()
                .push((frame.id, frame.along));
        }
        for element in &assembly.elements {
            topology
                .frame_placements
                .entry(element.frame)
                .or_default()
                .push(element.id);
        }
        let parents: BTreeMap<BotanicalElementId, Option<BotanicalElementId>> = assembly
            .axes
            .iter()
            .map(|axis| (axis.id, axis.parent))
            .collect();
        for axis in &assembly.axes {
            let mut depth = 0;
            let mut walk = axis.parent;
            // The parent chain is a forest by construction; the bound stops a corrupted document
            // from spinning here rather than failing.
            while let Some(parent) = walk.filter(|_| depth <= assembly.axes.len() as u32) {
                depth += 1;
                walk = parents.get(&parent).copied().flatten();
            }
            topology.axis_depth.insert(axis.id, depth);
        }
        topology
    }

    fn depth(&self, axis: BotanicalElementId) -> u32 {
        self.axis_depth.get(&axis).copied().unwrap_or_default()
    }

    /// Takes one axis, everything grown from it, and everything sitting on it.
    fn take_axis(&self, axis: BotanicalElementId, gone: &mut Removed) {
        if !gone.axes.insert(axis) {
            return;
        }
        for (frame, _) in self.axis_frames.get(&axis).into_iter().flatten() {
            self.take_frame(*frame, gone);
        }
        for child in self.axis_children.get(&axis).into_iter().flatten() {
            self.take_axis(*child, gone);
        }
    }

    /// Takes everything on one axis above a cut.
    fn take_above(&self, axis: BotanicalElementId, cut: UnitInterval, gone: &mut Removed) {
        for (frame, along) in self.axis_frames.get(&axis).into_iter().flatten() {
            if along.bits() > cut.bits() {
                self.take_frame(*frame, gone);
            }
        }
    }

    fn take_frame(&self, frame: BotanicalElementId, gone: &mut Removed) {
        if !gone.frames.insert(frame) {
            return;
        }
        for placement in self.frame_placements.get(&frame).into_iter().flatten() {
            gone.placements.insert(*placement);
        }
        for axis in self.frame_axes.get(&frame).into_iter().flatten() {
            self.take_axis(*axis, gone);
        }
    }

    /// The axes, frames, and placements a transform on `axis` carries with it.
    fn subtree(&self, axis: BotanicalElementId, gone: &Removed) -> Subtree {
        let mut subtree = Subtree::default();
        self.collect(axis, gone, &mut subtree);
        subtree
    }

    fn collect(&self, axis: BotanicalElementId, gone: &Removed, subtree: &mut Subtree) {
        if gone.axes.contains(&axis) || !subtree.axes.insert(axis) {
            return;
        }
        for (frame, _) in self.axis_frames.get(&axis).into_iter().flatten() {
            if gone.frames.contains(frame) || !subtree.frames.insert(*frame) {
                continue;
            }
            for placement in self.frame_placements.get(frame).into_iter().flatten() {
                if !gone.placements.contains(placement) {
                    subtree.placements.insert(*placement);
                }
            }
            for grown in self.frame_axes.get(frame).into_iter().flatten() {
                self.collect(*grown, gone, subtree);
            }
        }
        for child in self.axis_children.get(&axis).into_iter().flatten() {
            self.collect(*child, gone, subtree);
        }
    }
}

/// What one axis transform moves.
#[derive(Default)]
struct Subtree {
    axes: BTreeSet<BotanicalElementId>,
    frames: BTreeSet<BotanicalElementId>,
    placements: BTreeSet<BotanicalElementId>,
}

/// Cuts one axis at `at` and reports where the cut actually landed.
///
/// The cut snaps to the last rest point at or below it, so a trimmed axis keeps the exact integer
/// geometry it grew with instead of gaining an interpolated tip.
fn trim_axis(assembly: &mut BotanicalAssembly, index: usize, at: UnitInterval) -> UnitInterval {
    let axis = &mut assembly.axes[index];
    let segments = axis.points.len().saturating_sub(1);
    if segments < 2 {
        return UnitInterval::ONE;
    }
    let keep = ((u64::from(at.bits()) * segments as u64) / u64::from(UnitInterval::ONE.bits()))
        .max(1) as usize;
    axis.points.truncate(keep + 1);
    axis.radii.truncate(keep + 1);
    UnitInterval::from_bits(
        ((keep as u64 * u64::from(UnitInterval::ONE.bits())) / segments as u64) as u16,
    )
}

/// Applies one affine change about `axis`'s base to everything the axis carries.
fn transform_subtree(assembly: &mut BotanicalAssembly, subtree: &Subtree, edit: AxisTransform) {
    let AxisTransform {
        axis,
        offset,
        roll,
        scale,
        ..
    } = edit;
    let Some(base) = assembly
        .axes
        .iter()
        .find(|candidate| candidate.id == axis)
        .and_then(|candidate| candidate.points.first().copied())
    else {
        return;
    };
    let (sin, cos) = turn_sin_cos(i64::from(roll.bits()) * 2);
    let one = i64::from(UnitInterval::ONE.bits());
    let place = |point: [DecisionScalar; 3]| -> [DecisionScalar; 3] {
        let local: [i64; 3] =
            std::array::from_fn(|lane| i64::from(point[lane].bits() - base[lane].bits()));
        let scaled: [i64; 3] =
            std::array::from_fn(|lane| (local[lane] * i64::from(scale.bits())) >> 16);
        let turned = [
            (scaled[0] * cos - scaled[2] * sin) / one,
            scaled[1],
            (scaled[0] * sin + scaled[2] * cos) / one,
        ];
        std::array::from_fn(|lane| {
            DecisionScalar::from_bits(
                base[lane].bits().saturating_add(
                    i32::try_from(turned[lane].saturating_add(i64::from(offset[lane].bits())))
                        .unwrap_or(i32::MAX),
                ),
            )
        })
    };
    let turn_direction = |direction: [DecisionScalar; 3]| -> [DecisionScalar; 3] {
        let lanes: [i64; 3] = std::array::from_fn(|lane| i64::from(direction[lane].bits()));
        [
            DecisionScalar::from_bits(
                i32::try_from((lanes[0] * cos - lanes[2] * sin) / one).unwrap_or(i32::MAX),
            ),
            direction[1],
            DecisionScalar::from_bits(
                i32::try_from((lanes[0] * sin + lanes[2] * cos) / one).unwrap_or(i32::MAX),
            ),
        ]
    };

    for candidate in &mut assembly.axes {
        if !subtree.axes.contains(&candidate.id) {
            continue;
        }
        for point in &mut candidate.points {
            *point = place(*point);
        }
        for radius in &mut candidate.radii {
            *radius = scaled(*radius, scale);
        }
    }
    for frame in &mut assembly.frames {
        if !subtree.frames.contains(&frame.id) {
            continue;
        }
        frame.position = place(frame.position);
        frame.direction = turn_direction(frame.direction);
        frame.radius = scaled(frame.radius, scale);
    }
    for element in &mut assembly.elements {
        if !subtree.placements.contains(&element.id) {
            continue;
        }
        element.position = place(element.position);
        element.size = scaled(element.size, scale);
        element.roll = UnitInterval::from_bits(
            ((u32::from(element.roll.bits()) + u32::from(roll.bits())) % 65_536) as u16,
        );
    }
}

/// A size multiplied by a Q15.16 scale, never falling to zero: a scaled element is smaller, not
/// absent.
fn scaled(value: DecisionScalar, scale: DecisionScalar) -> DecisionScalar {
    DecisionScalar::from_bits(
        i32::try_from((i64::from(value.bits()) * i64::from(scale.bits())) >> 16)
            .unwrap_or(i32::MAX)
            .max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BOTANICAL_NODE_VERSION, BotanicalDrawnPoint, BotanicalElement, BotanicalGraphDocument,
        BotanicalNode, BotanicalOperator, grow,
    };

    fn scalar(bits: i32) -> DecisionScalar {
        DecisionScalar::from_bits(bits)
    }

    /// One times, in Q15.16.
    const UNCHANGED: i32 = 1 << 16;

    fn offset(edit: BotanicalManualEdit) -> BotanicalGraphDocument {
        let mut document = BotanicalGraphDocument::sapling(0x5a11);
        document.edits = vec![edit];
        document
    }

    fn first_leaf(document: &BotanicalGraphDocument) -> BotanicalElementId {
        grow(
            document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the starter graph grows")
        .assembly
        .elements[0]
            .id
    }

    /// The point of the layer: an edit is keyed to a semantic identity, so changing a parameter that
    /// leaves the element's ancestry intact keeps the hand work.
    #[test]
    fn an_edit_survives_an_unrelated_parameter_change() {
        let plain = BotanicalGraphDocument::sapling(0x5a11);
        let leaf = first_leaf(&plain);
        let mut document = offset(BotanicalManualEdit {
            target: leaf,
            action: BotanicalEditAction::Transform {
                offset: [scalar(1 << 16), scalar(0), scalar(0)],
                roll: UnitInterval::ZERO,
                scale: scalar(UNCHANGED),
            },
        });
        let before = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the edited graph grows");
        assert_eq!(before.diagnostics.applied, 1);
        assert!(before.diagnostics.is_clean());

        // A longer trunk moves every leaf, and changes none of their identities.
        let node = document
            .nodes
            .iter_mut()
            .find(|node| matches!(node.operator, BotanicalOperator::Trunk { .. }))
            .expect("the starter graph has a trunk");
        let BotanicalOperator::Trunk { length, .. } = &mut node.operator else {
            unreachable!("the node matched a trunk");
        };
        *length = scalar(6 << 16);
        let after = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the longer graph grows");
        assert_eq!(
            after.diagnostics.applied, 1,
            "the edit still found its leaf"
        );
        assert!(after.diagnostics.is_clean());
    }

    /// A change that takes the element away reports the edit rather than dropping it.
    #[test]
    fn a_vanished_target_is_reported_as_an_orphan() {
        let plain = BotanicalGraphDocument::sapling(0x5a11);
        let last = grow(
            &plain,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the starter graph grows")
        .assembly
        .elements
        .last()
        .expect("the starter graph places leaves")
        .id;
        let mut document = offset(BotanicalManualEdit {
            target: last,
            action: BotanicalEditAction::Transform {
                offset: [scalar(1 << 16), scalar(0), scalar(0)],
                roll: UnitInterval::ZERO,
                scale: scalar(UNCHANGED),
            },
        });
        let node = document
            .nodes
            .iter_mut()
            .find(|node| matches!(node.operator, BotanicalOperator::Phyllotaxis { .. }))
            .expect("the starter graph has phyllotaxis");
        let BotanicalOperator::Phyllotaxis { nodes, .. } = &mut node.operator else {
            unreachable!("the node matched phyllotaxis");
        };
        *nodes = 2;
        let growth = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the sparser graph grows");
        assert_eq!(growth.diagnostics.applied, 0);
        assert_eq!(
            growth.diagnostics.orphans,
            vec![BotanicalEditOrphan {
                target: last,
                action: BotanicalEditAction::Transform {
                    offset: [scalar(1 << 16), scalar(0), scalar(0)],
                    roll: UnitInterval::ZERO,
                    scale: scalar(UNCHANGED),
                },
                reason: BotanicalEditOrphanReason::TargetMissing,
            }]
        );
    }

    /// Removing an axis takes its shell with it, and leaves the rest of the plant alone.
    #[test]
    fn removing_an_axis_takes_what_it_carries() {
        let plain = BotanicalGraphDocument::sapling(0x5a11);
        let grown = grow(
            &plain,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the starter graph grows")
        .assembly;
        let root = grown
            .axes
            .iter()
            .find(|axis| axis.element == BotanicalElement::Root)
            .expect("the starter graph grows roots")
            .id;
        let document = offset(BotanicalManualEdit {
            target: root,
            action: BotanicalEditAction::Remove,
        });
        let after = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the edited graph grows")
        .assembly;
        assert_eq!(after.axes.len(), grown.axes.len() - 1);
        assert_eq!(after.shells.len(), grown.shells.len() - 1);
        assert!(!after.axes.iter().any(|axis| axis.id == root));
        assert!(!after.shells.iter().any(|shell| shell.axis == root));
        assert_eq!(
            after.elements.len(),
            grown.elements.len(),
            "a root has no leaves on it"
        );
    }

    /// A cut takes everything attached above it, and keeps the geometry below it exactly.
    #[test]
    fn a_trim_takes_what_sat_above_the_cut() {
        let plain = BotanicalGraphDocument::sapling(0x5a11);
        let grown = grow(
            &plain,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the starter graph grows")
        .assembly;
        let trunk = grown
            .axes
            .iter()
            .find(|axis| axis.element == BotanicalElement::Trunk)
            .expect("the starter graph grows a trunk");
        let trunk_id = trunk.id;
        let segments = trunk.points.len() - 1;
        let document = offset(BotanicalManualEdit {
            target: trunk_id,
            action: BotanicalEditAction::Trim {
                at: UnitInterval::from_bits(32_768),
            },
        });
        let after = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the trimmed graph grows")
        .assembly;
        let cut = after
            .axes
            .iter()
            .find(|axis| axis.id == trunk_id)
            .expect("the trunk survives its own cut");
        assert_eq!(cut.points.len() - 1, segments / 2);
        assert_eq!(
            cut.points.as_slice(),
            &trunk.points[..cut.points.len()],
            "the kept part is the geometry it grew with"
        );
        assert!(
            after.elements.len() < grown.elements.len(),
            "leaves above the cut went with it"
        );
        assert!(
            after
                .frames
                .iter()
                .all(|frame| frame.along.bits() <= 32_767 || frame.axis != trunk_id)
        );
    }

    /// A transform on an axis carries everything hanging off it, so the plant stays connected.
    #[test]
    fn an_axis_transform_carries_what_hangs_off_it() {
        let plain = BotanicalGraphDocument::sapling(0x5a11);
        let grown = grow(
            &plain,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the starter graph grows")
        .assembly;
        let trunk = grown
            .axes
            .iter()
            .find(|axis| axis.element == BotanicalElement::Trunk)
            .expect("the starter graph grows a trunk")
            .id;
        let document = offset(BotanicalManualEdit {
            target: trunk,
            action: BotanicalEditAction::Transform {
                offset: [scalar(2 << 16), scalar(0), scalar(0)],
                roll: UnitInterval::ZERO,
                scale: scalar(UNCHANGED),
            },
        });
        let after = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the moved graph grows")
        .assembly;
        for (before, moved) in grown.elements.iter().zip(&after.elements) {
            assert_eq!(before.id, moved.id);
            assert_eq!(
                moved.position[0].bits() - before.position[0].bits(),
                2 << 16,
                "a leaf travelled with the trunk it sits on"
            );
        }
    }

    /// Trimming something that is not an axis is a kind mismatch, reported rather than guessed at.
    #[test]
    fn an_action_that_cannot_apply_to_its_target_is_reported() {
        let plain = BotanicalGraphDocument::sapling(0x5a11);
        let leaf = first_leaf(&plain);
        let document = offset(BotanicalManualEdit {
            target: leaf,
            action: BotanicalEditAction::Trim {
                at: UnitInterval::from_bits(32_767),
            },
        });
        let growth = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the graph grows");
        assert_eq!(growth.diagnostics.applied, 0);
        assert_eq!(
            growth.diagnostics.orphans[0].reason,
            BotanicalEditOrphanReason::TargetKind
        );
    }

    /// Two edits that say opposite things about one element are refused, not resolved silently.
    #[test]
    fn a_contradictory_layer_is_refused() {
        let plain = BotanicalGraphDocument::sapling(0x5a11);
        let leaf = first_leaf(&plain);
        let mut document = plain.clone();
        document.edits = vec![
            BotanicalManualEdit {
                target: leaf,
                action: BotanicalEditAction::Transform {
                    offset: [scalar(0); 3],
                    roll: UnitInterval::ZERO,
                    scale: scalar(UNCHANGED),
                },
            },
            BotanicalManualEdit {
                target: leaf,
                action: BotanicalEditAction::Remove,
            },
        ];
        assert!(document.validate().is_err());

        // So is a layer out of canonical order, which would make the wire form ambiguous.
        let mut reversed = plain.clone();
        reversed.edits = vec![
            BotanicalManualEdit {
                target: leaf,
                action: BotanicalEditAction::Remove,
            },
            BotanicalManualEdit {
                target: leaf,
                action: BotanicalEditAction::Remove,
            },
        ];
        assert!(reversed.validate().is_err());
    }

    /// An edit changes the compiled family, so it changes the graph's identity.
    #[test]
    fn edits_participate_in_the_graph_identity() {
        let plain = BotanicalGraphDocument::sapling(0x5a11);
        let leaf = first_leaf(&plain);
        let edited = offset(BotanicalManualEdit {
            target: leaf,
            action: BotanicalEditAction::Transform {
                offset: [scalar(1 << 16), scalar(0), scalar(0)],
                roll: UnitInterval::ZERO,
                scale: scalar(UNCHANGED),
            },
        });
        assert_ne!(plain.identity(), edited.identity());
    }

    /// Age scales a plant without changing what it is: same elements, same identities, smaller.
    #[test]
    fn age_scales_the_plant_without_changing_its_structure() {
        let mut document = BotanicalGraphDocument::sapling(0x5a11);
        document.variations.push(crate::BotanicalVariation {
            seed: 0x5a11,
            age: UnitInterval::from_bits(32_768),
            name: "Sapling".to_owned(),
        });
        let mature = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the mature tree grows")
        .assembly;
        let young = grow(
            &document,
            1,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the sapling grows")
        .assembly;

        assert_eq!(mature.axes.len(), young.axes.len());
        assert_eq!(mature.elements.len(), young.elements.len());
        assert_eq!(
            mature.axes.iter().map(|axis| axis.id).collect::<Vec<_>>(),
            young.axes.iter().map(|axis| axis.id).collect::<Vec<_>>(),
            "identities do not depend on age, so one edit layer fits every variation"
        );
        let trunk = |assembly: &BotanicalAssembly| -> i32 {
            assembly
                .axes
                .iter()
                .find(|axis| axis.element == BotanicalElement::Trunk)
                .expect("a trunk")
                .length()
                .bits()
        };
        let (grown, small) = (trunk(&mature), trunk(&young));
        assert!(
            small < grown && small * 3 > grown,
            "half the age is around half the height: {small} against {grown}"
        );
    }

    /// Two variations at the same age but different seeds are two individuals of one species.
    #[test]
    fn a_variation_seed_selects_the_individual() {
        let mut document = BotanicalGraphDocument::sapling(0x5a11);
        document.variations.push(crate::BotanicalVariation {
            seed: 0x5a12,
            age: UnitInterval::ONE,
            name: "Second".to_owned(),
        });
        let first = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the first grows")
        .assembly;
        let second = grow(
            &document,
            1,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the second grows")
        .assembly;
        assert_eq!(first.axes.len(), second.axes.len(), "the same species");
        assert_ne!(first.elements, second.elements, "a different individual");

        // The same seed at the same age twice is the same individual twice, and refused.
        let mut duplicate = BotanicalGraphDocument::sapling(0x5a11);
        duplicate.variations.push(duplicate.variations[0].clone());
        assert!(duplicate.validate().is_err());

        // So is an age of zero, which describes a plant with no extent.
        let mut nothing = BotanicalGraphDocument::sapling(0x5a11);
        nothing.variations[0].age = UnitInterval::ZERO;
        assert!(nothing.validate().is_err());
    }

    /// A graft keeps the element's identity and its frame, and takes the generated quad's place.
    #[test]
    fn a_graft_stands_in_for_the_element_it_replaces() {
        let plain = BotanicalGraphDocument::sapling(0x5a11);
        let grown = grow(
            &plain,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the starter graph grows")
        .assembly;
        let leaf = grown.elements[0].clone();
        let document = offset(BotanicalManualEdit {
            target: leaf.id,
            action: BotanicalEditAction::Graft {
                source: 0x9e_11,
                selector: crate::PlantSourceSelector::Whole,
            },
        });
        let growth = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the grafted graph grows");
        assert_eq!(growth.diagnostics.applied, 1);
        assert!(growth.diagnostics.is_clean());
        assert_eq!(growth.assembly.grafts.len(), 1);
        let graft = &growth.assembly.grafts[0];
        assert_eq!(graft.id, leaf.id);
        assert_eq!(graft.frame, leaf.frame);
        assert_eq!(graft.position, leaf.position);
        assert_eq!(graft.source, 0x9e_11);
        assert!(
            !growth
                .assembly
                .elements
                .iter()
                .any(|element| element.id == leaf.id),
            "the generated quad went with the substitution"
        );
        assert_eq!(
            growth.assembly.elements.len(),
            grown.elements.len() - 1,
            "and nothing else moved"
        );
    }

    /// A graft needs a frame to stand on, so naming an axis is a kind mismatch.
    #[test]
    fn a_graft_on_an_axis_is_reported() {
        let plain = BotanicalGraphDocument::sapling(0x5a11);
        let trunk = grow(
            &plain,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the starter graph grows")
        .assembly
        .axes
        .iter()
        .find(|axis| axis.element == BotanicalElement::Trunk)
        .expect("the starter graph grows a trunk")
        .id;
        let document = offset(BotanicalManualEdit {
            target: trunk,
            action: BotanicalEditAction::Graft {
                source: 0x9e_11,
                selector: crate::PlantSourceSelector::Whole,
            },
        });
        let growth = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the graph grows");
        assert_eq!(growth.diagnostics.applied, 0);
        assert_eq!(
            growth.diagnostics.orphans[0].reason,
            BotanicalEditOrphanReason::TargetKind
        );
    }

    /// A drawn spine grows exactly the curve that was drawn, point for point.
    #[test]
    fn a_drawn_spine_grows_what_was_drawn() {
        let point = |x: i32, y: i32, radius: i32| BotanicalDrawnPoint {
            position: [scalar(x), scalar(y), scalar(0)],
            radius: scalar(radius),
        };
        let drawn = vec![
            point(0, 0, 8_000),
            point(1 << 15, 1 << 16, 6_000),
            point(1 << 16, 2 << 16, 4_000),
        ];
        let node = |guid: u128, operator: BotanicalOperator| BotanicalNode {
            guid,
            version: BOTANICAL_NODE_VERSION,
            semantic_revision: 1,
            operator,
        };
        let edge = |from: u128, from_pin: &str, to: u128, to_pin: &str| crate::BotanicalEdge {
            from_node: from,
            from_pin: from_pin.to_owned(),
            to_node: to,
            to_pin: to_pin.to_owned(),
        };
        let document = BotanicalGraphDocument {
            variations: vec![crate::BotanicalVariation {
                seed: 7,
                age: UnitInterval::ONE,
                name: "Mature".to_owned(),
            }],
            edits: Vec::new(),
            nodes: vec![
                node(
                    1,
                    BotanicalOperator::Drawn {
                        element: BotanicalElement::Trunk,
                        points: drawn.clone(),
                    },
                ),
                node(
                    2,
                    BotanicalOperator::Shell {
                        material_slot: 0,
                        sides: 5,
                    },
                ),
                node(3, BotanicalOperator::Family),
            ],
            edges: vec![edge(1, "axes", 2, "axes"), edge(2, "shells", 3, "shells")],
        };
        let grown = grow(
            &document,
            0,
            &crate::NoBotanicalModules,
            &crate::BotanicalBudget::COOK,
        )
        .expect("the drawn graph grows")
        .assembly;
        assert_eq!(grown.axes.len(), 1);
        assert_eq!(
            grown.axes[0].points,
            drawn.iter().map(|point| point.position).collect::<Vec<_>>()
        );
        assert_eq!(
            grown.axes[0].radii,
            drawn.iter().map(|point| point.radius).collect::<Vec<_>>()
        );
    }
}
