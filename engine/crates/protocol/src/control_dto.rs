//! Shared failures and structured diagnostics for the control-plane wire.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    PlantReimportConflictReasonDto, PlantSemanticDestinationDto, PlantSourceSelectorDto,
    VegetationGuid,
};

/// One failure carried by a control reply or the editor's native bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "code",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(
    export,
    tag = "code",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum ControlFailureDto {
    /// A command was unknown or rejected by its domain handler.
    Command { message: String },
    /// Request parameters did not match the command DTO.
    Params { message: String },
    /// A command was discarded while a project load owned the engine state.
    BusyLoading { message: String },
    /// The request envelope was not valid JSON.
    InvalidRequest { message: String },
    /// The client could not complete the socket round-trip.
    Transport { message: String },
    /// A peer returned a reply that did not match the control contract.
    MalformedReply { message: String },
    /// A native editor command failed outside the engine control socket.
    Bridge { message: String },
    /// A domain operation produced a machine-readable diagnostic.
    Diagnostic {
        message: String,
        diagnostic: ControlDiagnosticDto,
    },
}

impl ControlFailureDto {
    /// The human-readable explanation carried by every failure variant.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Command { message }
            | Self::Params { message }
            | Self::BusyLoading { message }
            | Self::InvalidRequest { message }
            | Self::Transport { message }
            | Self::MalformedReply { message }
            | Self::Bridge { message }
            | Self::Diagnostic { message, .. } => message,
        }
    }

    /// The stable discriminator serialized in the wire `code` field.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Command { .. } => "command",
            Self::Params { .. } => "params",
            Self::BusyLoading { .. } => "busy-loading",
            Self::InvalidRequest { .. } => "invalid-request",
            Self::Transport { .. } => "transport",
            Self::MalformedReply { .. } => "malformed-reply",
            Self::Bridge { .. } => "bridge",
            Self::Diagnostic { .. } => "diagnostic",
        }
    }
}

impl std::fmt::Display for ControlFailureDto {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

/// Domain-specific diagnostic payload carried by [`ControlFailureDto::Diagnostic`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "domain",
    content = "detail",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
#[ts(export, tag = "domain", content = "detail", rename_all = "kebab-case")]
pub enum ControlDiagnosticDto {
    /// A vegetation graph failed compilation, admission, or execution.
    VegetationGraph(VegetationGraphDiagnosticDto),
    /// A content-addressed vegetation artifact failed strict validation or publication.
    VegetationArtifact(VegetationArtifactDiagnosticDto),
    /// Plant reimport would discard one or more authored semantic targets.
    ReimportConflict(ReimportConflictDiagnosticDto),
}

/// Exact structured fields for strict vegetation artifact failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "category",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(
    export,
    tag = "category",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum VegetationArtifactDiagnosticDto {
    /// The artifact ended before its declared structure was complete.
    Truncated { format: String },
    /// The authored or derived format version is not accepted by this build.
    Version {
        format: String,
        found: u32,
        expected: u32,
    },
    /// The artifact schema identity is not accepted by this build.
    Schema { format: String },
    /// A TOC section kind is outside the exact artifact vocabulary.
    UnknownSection { format: String, section: u16 },
    /// A TOC codec is outside the exact artifact vocabulary.
    UnknownCodec { format: String, codec: u8 },
    /// A TOC section kind occurs more than once.
    DuplicateSection { format: String, section: u16 },
    /// A TOC section violates its declared alignment.
    MisalignedSection { format: String, section: u16 },
    /// Two TOC section spans overlap.
    OverlappingSection { format: String, section: u16 },
    /// A header, payload, section, or manifest digest does not match its canonical bytes.
    HashMismatch { format: String, subject: String },
    /// A structural field violates the exact artifact contract.
    Format { format: String, field: String },
    /// A content-addressed artifact is absent from the project cache.
    NotFound { kind: String, content_hash: String },
    /// Two different byte streams resolved to the same content address.
    ContentAddressCollision { path: String },
    /// Publication was cancelled before the complete generation became visible.
    Cancelled,
    /// A superseded generation attempted to publish after a newer generation started.
    Superseded,
}

/// One authored plant mapping that cannot be preserved by reimport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ReimportConflictEntryDto {
    pub target: VegetationGuid,
    pub source: VegetationGuid,
    pub selector: PlantSourceSelectorDto,
    pub destination: PlantSemanticDestinationDto,
    pub reason: PlantReimportConflictReasonDto,
}

/// Complete visible conflict report for one rejected plant reimport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ReimportConflictDiagnosticDto {
    pub plant: crate::Uuid,
    pub conflicts: Vec<ReimportConflictEntryDto>,
}

/// Exact structured fields for every vegetation graph failure category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "category",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(
    export,
    tag = "category",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum VegetationGraphDiagnosticDto {
    /// A checked integer or fixed-point operation overflowed.
    NumericOverflow,
    /// A graph-owned collection could not reserve its checked capacity.
    MemoryReservation { resource: String, reason: String },
    /// An evaluator worker could not be created.
    WorkerSpawn { reason: String },
    /// An evaluator worker panicked before atomic publication.
    WorkerPanicked,
    /// A typed graph document was malformed.
    Document { path: String, reason: String },
    /// A dependency cycle closed through a stable node GUID.
    Cycle { node: VegetationGuid },
    /// An edge connected incompatible typed pin domains.
    TypeMismatch {
        from_node: VegetationGuid,
        from_pin: String,
        from_domain: String,
        to_node: VegetationGuid,
        to_pin: String,
        to_domain: String,
    },
    /// A cosmetic value reached authoritative state.
    Authority {
        node: VegetationGuid,
        reason: String,
    },
    /// A partition-local node had no finite influence bound.
    UnboundedInfluence { node: VegetationGuid },
    /// A graph estimate or evaluation exceeded an explicit cap.
    Limit {
        resource: String,
        requested: String,
        limit: String,
    },
    /// Evaluation was cancelled before atomic publication.
    Cancelled,
    /// Runtime-authoritative evaluation lacked a canonical input.
    AuthoritativeInput { node: VegetationGuid, input: String },
    /// An equivalent-GPU node lacked qualification for the device profile.
    GpuQualification {
        node: VegetationGuid,
        profile: String,
    },
    /// A qualified Slang dispatch failed before completing its batch.
    GpuExecution { profile: String, reason: String },
}
