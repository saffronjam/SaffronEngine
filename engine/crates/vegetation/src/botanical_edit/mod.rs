//! Manual edits laid over what the botanical graph grows.
//!
//! An edit is authored data on the document, addressed by the *semantic identity* of what it
//! changes rather than by an index into anything, so hand work survives an artist going back to the
//! graph and changing a parameter. Growing is untouched: it runs exactly as if no edit existed, and
//! the layer applies to its result. A change that regrows the same identity keeps its edit; a
//! change that does not gets an orphan diagnostic naming the edit and why it could not land.

mod apply;
mod model;

pub use apply::apply_manual_edits;
pub use model::*;
