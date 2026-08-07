use std::collections::BTreeSet;

use saffron_spatial::{DecisionScalar, UnitInterval};

use crate::{BotanicalElementId, Error, PlantSourceSelector, Result};

/// Edits one document may carry.
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
    /// The graph does not grow that identity.
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

fn field(name: &str) -> Error {
    Error::ArtifactFormat {
        format: "botanical graph",
        field: name.to_owned(),
    }
}
