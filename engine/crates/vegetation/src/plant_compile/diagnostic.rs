//! Typed compile diagnostics and reimport conflicts.

use crate::{
    Error, PlantManualSemanticTarget, PlantSemanticDestination, PlantSourceSelector, Result,
};

use super::*;

/// Severity of one source-compile diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlantCompileDiagnosticSeverity {
    /// Informational state that does not block publication.
    Info,
    /// Actionable quality warning that does not invalidate content.
    Warning,
    /// Invalid input or output that blocks publication.
    Error,
}

/// Stable diagnostic category used by control/editor routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlantCompileDiagnosticCode {
    /// A recipe source has no resolved snapshot.
    MissingSource,
    /// More than one snapshot carries the same source identity.
    DuplicateSource,
    /// A selected source contribution contains no matching payload.
    EmptySelection,
    /// Geometry contains malformed, non-finite, degenerate, or out-of-range data.
    InvalidGeometry,
    /// A material slot cannot be resolved exactly.
    MissingMaterial,
    /// A resolved material or coverage contract is invalid.
    InvalidMaterial,
    /// A structural joint hierarchy or vertex-weight stream is invalid.
    InvalidSkeleton,
    /// A leaf-like coverage source requires usable UV area.
    MissingCoverageUv,
    /// Leaf-like source normals oppose their geometric front face.
    InvalidLeafOrientation,
    /// Authored dimensions or crown/root footprints do not contain normalized geometry.
    BoundsMismatch,
    /// The compile request exceeded a declared hard bound.
    LimitExceeded,
    /// A resolved source hash differs from the last accepted source identity.
    SourceChanged,
    /// An authored manual edit has no surviving element to change.
    OrphanedEdit,
}

/// One typed source-compile diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantCompileDiagnostic {
    /// Severity and publication effect.
    pub severity: PlantCompileDiagnosticSeverity,
    /// Stable diagnostic category.
    pub code: PlantCompileDiagnosticCode,
    /// Recipe source identity when source-specific.
    pub source: Option<u128>,
    /// Stable source selector when element-specific.
    pub selector: Option<PlantSourceSelector>,
    /// Canonical family/source field path.
    pub path: String,
    /// Concise user-facing explanation.
    pub message: String,
}

/// Why a manual semantic target cannot be applied after reimport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlantReimportConflictReason {
    /// The referenced source is absent.
    MissingSource,
    /// The referenced stable element or submesh disappeared.
    MissingElement,
}

/// One manual semantic binding that reimport cannot preserve automatically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantReimportConflict {
    /// Stable manual binding identity.
    pub target: u128,
    pub source: u128,
    /// Missing stable element or submesh.
    pub selector: PlantSourceSelector,
    /// Authored destination that remains untouched.
    pub destination: PlantSemanticDestination,
    /// Typed reason publication was refused.
    pub reason: PlantReimportConflictReason,
}

/// Complete deterministic reimport-conflict report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlantReimportConflictReport {
    /// Sorted manual conflicts. Any entry blocks publication.
    pub conflicts: Vec<PlantReimportConflict>,
}

/// Accepted observed hash for one recipe source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlantSourceHashUpdate {
    /// Recipe source identity.
    pub source: u128,
    /// Previously authored source content hash.
    pub previous: [u8; 32],
    /// Current resolved source content hash.
    pub current: [u8; 32],
}

pub(super) fn conflict(
    target: &PlantManualSemanticTarget,
    reason: PlantReimportConflictReason,
) -> PlantReimportConflict {
    PlantReimportConflict {
        target: target.id,
        source: target.source,
        selector: target.selector.clone(),
        destination: target.destination,
        reason,
    }
}

pub(super) fn diagnostic(
    severity: PlantCompileDiagnosticSeverity,
    code: PlantCompileDiagnosticCode,
    source: Option<u128>,
    selector: Option<PlantSourceSelector>,
    path: &str,
    message: &str,
) -> PlantCompileDiagnostic {
    PlantCompileDiagnostic {
        severity,
        code,
        source,
        selector,
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

pub(super) fn push_limit(
    diagnostics: &mut Vec<PlantCompileDiagnostic>,
    limits: PlantCompileLimits,
    path: &str,
    message: &str,
) -> Result<()> {
    push_diagnostic(
        diagnostics,
        limits,
        diagnostic(
            PlantCompileDiagnosticSeverity::Error,
            PlantCompileDiagnosticCode::LimitExceeded,
            None,
            None,
            path,
            message,
        ),
    )
}

/// Canonical diagnostic order, so two compiles of the same input report identically.
pub(super) fn sort_diagnostics(diagnostics: &mut [PlantCompileDiagnostic]) {
    diagnostics.sort_by(|first, second| {
        (
            first.severity,
            first.code,
            first.source,
            &first.selector,
            &first.path,
            &first.message,
        )
            .cmp(&(
                second.severity,
                second.code,
                second.source,
                &second.selector,
                &second.path,
                &second.message,
            ))
    });
}

pub(super) fn push_diagnostic(
    diagnostics: &mut Vec<PlantCompileDiagnostic>,
    limits: PlantCompileLimits,
    diagnostic: PlantCompileDiagnostic,
) -> Result<()> {
    if diagnostics.len() >= limits.diagnostics as usize {
        return Err(Error::InvalidFormat {
            format: ".splant",
            field: "compile.diagnosticLimit".to_owned(),
        });
    }
    diagnostics.push(diagnostic);
    Ok(())
}
