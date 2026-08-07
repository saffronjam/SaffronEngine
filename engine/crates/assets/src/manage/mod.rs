//! Asset-management operations the control plane drives: sub-asset extraction, model
//! reimport, the read-only dependency graph + cleanup analysis, and drag-a-folder
//! material import.

mod clean;
mod container;
mod extract;
mod material_import;
mod references;
mod reimport;

#[cfg(test)]
mod test_support;

pub use clean::{
    CleanCandidate, CleanCategory, CleanReportData, DeleteUnusedData, analyze_clean, delete_unused,
};
pub use extract::{clear_extraction, extract_sub_asset};
pub use material_import::{MaterialImportResult, import_material_folder};
pub use references::{
    DependencyGraph, RefEdge, RefEdgeKind, RefNode, asset_bytes, build_dependency_graph,
};
pub use reimport::{ReimportDelta, reimport_model};

pub(crate) use container::rewrite_material_chunk;
