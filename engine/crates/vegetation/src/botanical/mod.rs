//! The native botanical graph: a typed authoring IR that grows a plant family.
//!
//! A `.splant` carries exactly one source. An imported family is a recipe of references to external
//! geometry; a native family is this graph. Both normalize to the same compiled family. The type
//! system is botanical and shares no pin domain, operator name, or JSON shape with the biome graph:
//! a biome graph decides *where plants go*, this one decides *what one plant is*.
//!
//! Positions and radii are Q15.16 metres, angles are signed normalized half-turns, and every
//! stochastic choice draws from a counter-based stream keyed by (graph, node, element ordinal,
//! channel) — so the same graph grows the same plant on any machine, in any order, and adding a
//! node cannot perturb an unrelated one's variation.

mod assembly;
mod document;
mod grow;
mod operator;
mod shape;

#[cfg(test)]
pub(crate) mod tests_support;

pub use assembly::*;
pub use document::*;
pub use grow::*;
pub use operator::*;

pub(crate) use shape::turn_sin_cos;
