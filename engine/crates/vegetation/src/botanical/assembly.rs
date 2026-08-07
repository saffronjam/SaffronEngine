use saffron_spatial::{DecisionScalar, UnitInterval};

use crate::{ContentHash, PlantSourceSelector};

use super::operator::BotanicalElement;

/// Stable identity of one grown element.
///
/// Derived from the producing node, the parent element, and the ordinal within that parent — never
/// from a global counter, so a manual edit keyed to it survives any parameter change that leaves
/// its ancestry and ordinal intact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BotanicalElementId(u128);

impl BotanicalElementId {
    /// The root identity of a node's own output.
    #[must_use]
    pub fn root(node: u128, ordinal: u32) -> Self {
        Self(mix(mix(node, 0), u128::from(ordinal)))
    }

    /// The identity of `ordinal`'s child of `self`, produced by `node`.
    #[must_use]
    pub fn child(self, node: u128, ordinal: u32) -> Self {
        Self(mix(mix(self.0, node), u128::from(ordinal)))
    }

    /// The raw identity.
    #[must_use]
    pub const fn value(self) -> u128 {
        self.0
    }

    /// The identity a raw value names, as an authored edit or a wire payload spells it.
    #[must_use]
    pub const fn from_value(value: u128) -> Self {
        Self(value)
    }
}

/// A stable, order-independent mix of two identities.
fn mix(left: u128, right: u128) -> u128 {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(&left.to_be_bytes());
    bytes[16..].copy_from_slice(&right.to_be_bytes());
    let hash = ContentHash::of(&bytes).bytes();
    let mut leading = [0_u8; 16];
    leading.copy_from_slice(&hash[..16]);
    u128::from_be_bytes(leading)
}

/// One grown axis: a directed curve of rest points with a radius at each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalAxis {
    pub id: BotanicalElementId,
    /// Parent axis, absent for a trunk.
    pub parent: Option<BotanicalElementId>,
    /// Attachment frame it grew from, absent for a trunk, a drawn spine, or a root. A cut through
    /// the parent takes everything hanging above the frame.
    pub frame: Option<BotanicalElementId>,
    pub element: BotanicalElement,
    /// Rest points in family-local metres, base first.
    pub points: Vec<[DecisionScalar; 3]>,
    /// Radius at each rest point.
    pub radii: Vec<DecisionScalar>,
}

impl BotanicalAxis {
    /// Straight-line length from base to tip.
    #[must_use]
    pub fn length(&self) -> DecisionScalar {
        let (Some(base), Some(tip)) = (self.points.first(), self.points.last()) else {
            return DecisionScalar::from_bits(0);
        };
        let squared: i64 = (0..3)
            .map(|axis| {
                let delta = i64::from(tip[axis].bits() - base[axis].bits());
                delta * delta
            })
            .sum();
        DecisionScalar::from_bits(i32::try_from(isqrt(squared)).unwrap_or(i32::MAX))
    }

    /// Height of the axis base above the family origin.
    #[must_use]
    pub fn base_height(&self) -> DecisionScalar {
        self.points
            .first()
            .map_or(DecisionScalar::from_bits(0), |point| point[1])
    }
}

/// One oriented attachment frame on an axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotanicalFrame {
    pub id: BotanicalElementId,
    /// The axis it sits on.
    pub axis: BotanicalElementId,
    /// Position in family-local metres.
    pub position: [DecisionScalar; 3],
    /// Outward direction, unit-scaled in Q15.16.
    pub direction: [DecisionScalar; 3],
    /// Radius of the parent axis at this point, which sizes what attaches here.
    pub radius: DecisionScalar,
    /// Where along the axis it sits.
    pub along: UnitInterval,
}

/// One swept surface shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalShell {
    pub id: BotanicalElementId,
    /// The axis it sweeps.
    pub axis: BotanicalElementId,
    /// Element class of that axis.
    pub element: BotanicalElement,
    pub material_slot: u32,
    /// Cross-section sides.
    pub sides: u32,
}

/// One placed instanced element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotanicalPlacement {
    pub id: BotanicalElementId,
    /// The frame it sits on.
    pub frame: BotanicalElementId,
    pub element: BotanicalElement,
    pub material_slot: u32,
    /// Position in family-local metres.
    pub position: [DecisionScalar; 3],
    /// Size in metres.
    pub size: DecisionScalar,
    /// Roll about the frame direction, as a signed normalized half-turn.
    pub roll: UnitInterval,
}

/// One hand-modelled mesh standing in for a generated element, keeping that element's identity and
/// frame so only the surface differs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalGraft {
    /// Identity of the element it replaced.
    pub id: BotanicalElementId,
    /// The frame it stands on.
    pub frame: BotanicalElementId,
    /// The family graft source supplying the geometry.
    pub source: u128,
    /// Which of that source's elements to take.
    pub selector: PlantSourceSelector,
    /// Position in family-local metres.
    pub position: [DecisionScalar; 3],
    /// Roll about the frame direction, as a signed normalized half-turn.
    pub roll: UnitInterval,
}

/// Everything one graph grew, in canonical identity order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BotanicalAssembly {
    /// Axes, trunks first by construction order then canonical identity.
    pub axes: Vec<BotanicalAxis>,
    pub frames: Vec<BotanicalFrame>,
    /// Swept shells reaching the family output.
    pub shells: Vec<BotanicalShell>,
    /// Instanced elements reaching the family output.
    pub elements: Vec<BotanicalPlacement>,
    /// Hand-modelled meshes standing in for elements the edit layer grafted over.
    pub grafts: Vec<BotanicalGraft>,
}

impl BotanicalAssembly {
    /// Conservative local bounds over every axis point and placed element.
    #[must_use]
    pub fn local_bounds(&self) -> ([DecisionScalar; 3], [DecisionScalar; 3]) {
        let mut minimum = [i32::MAX; 3];
        let mut maximum = [i32::MIN; 3];
        let mut extend = |point: [DecisionScalar; 3], pad: i32| {
            for axis in 0..3 {
                minimum[axis] = minimum[axis].min(point[axis].bits().saturating_sub(pad));
                maximum[axis] = maximum[axis].max(point[axis].bits().saturating_add(pad));
            }
        };
        for axis in &self.axes {
            for (point, radius) in axis.points.iter().zip(&axis.radii) {
                extend(*point, radius.bits());
            }
        }
        for element in &self.elements {
            extend(element.position, element.size.bits());
        }
        if minimum[0] == i32::MAX {
            let zero = DecisionScalar::from_bits(0);
            return ([zero; 3], [zero; 3]);
        }
        (
            std::array::from_fn(|axis| DecisionScalar::from_bits(minimum[axis])),
            std::array::from_fn(|axis| DecisionScalar::from_bits(maximum[axis])),
        )
    }
}

/// Integer square root, so a length never depends on floating-point rounding.
pub(crate) fn isqrt(value: i64) -> i64 {
    value.max(0).isqrt()
}
