//! The control-plane error type, structured wire conversion, and `Result` alias.

use saffron_protocol::{
    ControlDiagnosticDto, ControlFailureDto, VegetationGraphDiagnosticDto, VegetationGuid,
};

/// Failures raised while standing up the socket server, framing requests, or
/// running a command handler.
///
/// The variants are typed so dispatch converts every failure through one shared wire DTO.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A socket syscall (socket/bind/listen/accept) failed while standing up the
    /// server. The payload names the syscall and the OS error.
    #[error("{0}")]
    Socket(String),
    /// The resolved socket path does not fit in `sockaddr_un.sun_path`.
    #[error("socket path too long: {0}")]
    PathTooLong(String),
    /// A command handler reported a business failure.
    #[error("{0}")]
    Command(String),
    /// A request param failed to deserialize into the handler's typed DTO.
    #[error("{0}")]
    Params(String),
    /// The request envelope was not valid JSON.
    #[error("{0}")]
    InvalidRequest(String),
    /// A command was rejected because a project load is in flight; the editor drops and retries.
    #[error("engine busy loading project")]
    Busy,
    /// A domain operation returned a structured diagnostic.
    #[error("{message}")]
    Diagnostic {
        /// Human-readable summary for logs and text clients.
        message: String,
        /// Machine-readable category and exact source fields.
        diagnostic: Box<ControlDiagnosticDto>,
    },
    /// A shared failure retained by an asynchronous control operation.
    #[error("{}", .0.message())]
    Failure(Box<ControlFailureDto>),
}

impl Error {
    /// Builds a [`Error::Command`] from anything that renders as a string.
    pub fn command(message: impl Into<String>) -> Self {
        Self::Command(message.into())
    }

    /// Converts this internal failure into the one shared wire representation.
    #[must_use]
    pub fn into_failure(self) -> ControlFailureDto {
        match self {
            Error::Socket(message) => ControlFailureDto::Transport { message },
            Error::PathTooLong(path) => ControlFailureDto::Transport {
                message: format!("socket path too long: {path}"),
            },
            Error::Command(message) => ControlFailureDto::Command { message },
            Error::Params(message) => ControlFailureDto::Params { message },
            Error::InvalidRequest(message) => ControlFailureDto::InvalidRequest { message },
            Error::Busy => ControlFailureDto::BusyLoading {
                message: "engine busy loading project".to_owned(),
            },
            Error::Diagnostic {
                message,
                diagnostic,
            } => ControlFailureDto::Diagnostic {
                message,
                diagnostic: *diagnostic,
            },
            Error::Failure(failure) => *failure,
        }
    }
}

impl From<saffron_assets::Error> for Error {
    fn from(error: saffron_assets::Error) -> Self {
        match error {
            saffron_assets::Error::Vegetation(source) => source.into(),
            other => Self::Command(other.to_string()),
        }
    }
}

impl From<saffron_vegetation::Error> for Error {
    fn from(error: saffron_vegetation::Error) -> Self {
        use saffron_vegetation::Error as VegetationError;

        let detail = match &error {
            VegetationError::NumericOverflow => VegetationGraphDiagnosticDto::NumericOverflow,
            VegetationError::MemoryReservation { resource, source } => {
                VegetationGraphDiagnosticDto::MemoryReservation {
                    resource: (*resource).to_owned(),
                    reason: source.to_string(),
                }
            }
            VegetationError::GraphWorkerSpawn { source } => {
                VegetationGraphDiagnosticDto::WorkerSpawn {
                    reason: source.to_string(),
                }
            }
            VegetationError::GraphWorkerPanicked => VegetationGraphDiagnosticDto::WorkerPanicked,
            VegetationError::GraphDocument { path, reason } => {
                VegetationGraphDiagnosticDto::Document {
                    path: path.clone(),
                    reason: reason.clone(),
                }
            }
            VegetationError::GraphCycle { node } => VegetationGraphDiagnosticDto::Cycle {
                node: vegetation_guid(*node),
            },
            VegetationError::GraphTypeMismatch {
                from_node,
                from_pin,
                from_domain,
                to_node,
                to_pin,
                to_domain,
            } => VegetationGraphDiagnosticDto::TypeMismatch {
                from_node: vegetation_guid(*from_node),
                from_pin: from_pin.clone(),
                from_domain: (*from_domain).to_owned(),
                to_node: vegetation_guid(*to_node),
                to_pin: to_pin.clone(),
                to_domain: (*to_domain).to_owned(),
            },
            VegetationError::GraphAuthority { node, reason } => {
                VegetationGraphDiagnosticDto::Authority {
                    node: vegetation_guid(*node),
                    reason: reason.clone(),
                }
            }
            VegetationError::GraphUnboundedInfluence { node } => {
                VegetationGraphDiagnosticDto::UnboundedInfluence {
                    node: vegetation_guid(*node),
                }
            }
            VegetationError::GraphLimit {
                resource,
                requested,
                limit,
            } => VegetationGraphDiagnosticDto::Limit {
                resource: (*resource).to_owned(),
                requested: requested.to_string(),
                limit: limit.to_string(),
            },
            VegetationError::GraphCancelled => VegetationGraphDiagnosticDto::Cancelled,
            VegetationError::GraphAuthoritativeInput { node, input } => {
                VegetationGraphDiagnosticDto::AuthoritativeInput {
                    node: vegetation_guid(*node),
                    input: input.clone(),
                }
            }
            VegetationError::GraphGpuQualification { node, profile } => {
                VegetationGraphDiagnosticDto::GpuQualification {
                    node: vegetation_guid(*node),
                    profile: profile.clone(),
                }
            }
            VegetationError::GraphGpuExecution { profile, reason } => {
                VegetationGraphDiagnosticDto::GpuExecution {
                    profile: profile.clone(),
                    reason: reason.clone(),
                }
            }
            VegetationError::FormatVersion { .. }
            | VegetationError::InvalidFormat { .. }
            | VegetationError::InvalidPlantId
            | VegetationError::InvalidPlantTagId
            | VegetationError::DuplicatePlantId(_)
            | VegetationError::PointSchema(_)
            | VegetationError::Mutation(_)
            | VegetationError::ManifestMismatch
            | VegetationError::UnknownRuntimeCell { .. }
            | VegetationError::PlantNotResident { .. }
            | VegetationError::StaleGeneration { .. }
            | VegetationError::FacetNotResident { .. }
            | VegetationError::ArtifactIo { .. }
            | VegetationError::ArtifactTruncated { .. }
            | VegetationError::ArtifactSchema { .. }
            | VegetationError::ArtifactUnknownSection { .. }
            | VegetationError::ArtifactUnknownCodec { .. }
            | VegetationError::ArtifactDuplicateSection { .. }
            | VegetationError::ArtifactMisalignedSection { .. }
            | VegetationError::ArtifactOverlappingSection { .. }
            | VegetationError::ArtifactHashMismatch { .. }
            | VegetationError::ArtifactFormat { .. }
            | VegetationError::Spatial(_)
            | VegetationError::Json(_) => return Self::Command(error.to_string()),
        };
        Self::Diagnostic {
            message: error.to_string(),
            diagnostic: Box::new(ControlDiagnosticDto::VegetationGraph(detail)),
        }
    }
}

fn vegetation_guid(value: u128) -> VegetationGuid {
    VegetationGuid(format!("{value:032x}"))
}

/// The crate `Result` alias bound to the typed [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn detail(error: saffron_vegetation::Error) -> Value {
        let ControlFailureDto::Diagnostic { diagnostic, .. } = Error::from(error).into_failure()
        else {
            panic!("expected a structured diagnostic")
        };
        let ControlDiagnosticDto::VegetationGraph(detail) = diagnostic else {
            panic!("expected a vegetation graph diagnostic")
        };
        serde_json::to_value(detail).unwrap()
    }

    #[test]
    fn every_vegetation_graph_error_preserves_its_exact_fields() {
        let mut allocation = Vec::<u8>::new();
        let reservation = allocation.try_reserve(usize::MAX).unwrap_err();
        let reservation_reason = reservation.to_string();
        let cases = [
            (
                saffron_vegetation::Error::NumericOverflow,
                json!({ "category": "numeric-overflow" }),
            ),
            (
                saffron_vegetation::Error::MemoryReservation {
                    resource: "candidate stream",
                    source: reservation,
                },
                json!({
                    "category": "memory-reservation",
                    "resource": "candidate stream",
                    "reason": reservation_reason,
                }),
            ),
            (
                saffron_vegetation::Error::GraphWorkerSpawn {
                    source: std::io::Error::other("thread quota"),
                },
                json!({ "category": "worker-spawn", "reason": "thread quota" }),
            ),
            (
                saffron_vegetation::Error::GraphWorkerPanicked,
                json!({ "category": "worker-panicked" }),
            ),
            (
                saffron_vegetation::Error::GraphDocument {
                    path: "graph.nodes.0".to_owned(),
                    reason: "missing pin".to_owned(),
                },
                json!({
                    "category": "document",
                    "path": "graph.nodes.0",
                    "reason": "missing pin",
                }),
            ),
            (
                saffron_vegetation::Error::GraphCycle { node: 1 },
                json!({
                    "category": "cycle",
                    "node": "00000000000000000000000000000001",
                }),
            ),
            (
                saffron_vegetation::Error::GraphTypeMismatch {
                    from_node: 1,
                    from_pin: "points".to_owned(),
                    from_domain: "candidate-stream",
                    to_node: 2,
                    to_pin: "density".to_owned(),
                    to_domain: "scalar-field",
                },
                json!({
                    "category": "type-mismatch",
                    "fromNode": "00000000000000000000000000000001",
                    "fromPin": "points",
                    "fromDomain": "candidate-stream",
                    "toNode": "00000000000000000000000000000002",
                    "toPin": "density",
                    "toDomain": "scalar-field",
                }),
            ),
            (
                saffron_vegetation::Error::GraphAuthority {
                    node: 3,
                    reason: "cosmetic source".to_owned(),
                },
                json!({
                    "category": "authority",
                    "node": "00000000000000000000000000000003",
                    "reason": "cosmetic source",
                }),
            ),
            (
                saffron_vegetation::Error::GraphUnboundedInfluence { node: 4 },
                json!({
                    "category": "unbounded-influence",
                    "node": "00000000000000000000000000000004",
                }),
            ),
            (
                saffron_vegetation::Error::GraphLimit {
                    resource: "candidates",
                    requested: 16,
                    limit: 4,
                },
                json!({
                    "category": "limit",
                    "resource": "candidates",
                    "requested": "16",
                    "limit": "4",
                }),
            ),
            (
                saffron_vegetation::Error::GraphCancelled,
                json!({ "category": "cancelled" }),
            ),
            (
                saffron_vegetation::Error::GraphAuthoritativeInput {
                    node: 5,
                    input: "surface-height".to_owned(),
                },
                json!({
                    "category": "authoritative-input",
                    "node": "00000000000000000000000000000005",
                    "input": "surface-height",
                }),
            ),
            (
                saffron_vegetation::Error::GraphGpuQualification {
                    node: 6,
                    profile: "apple-m3".to_owned(),
                },
                json!({
                    "category": "gpu-qualification",
                    "node": "00000000000000000000000000000006",
                    "profile": "apple-m3",
                }),
            ),
            (
                saffron_vegetation::Error::GraphGpuExecution {
                    profile: "apple-m3".to_owned(),
                    reason: "dispatch failed".to_owned(),
                },
                json!({
                    "category": "gpu-execution",
                    "profile": "apple-m3",
                    "reason": "dispatch failed",
                }),
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(detail(error), expected);
        }
    }

    #[test]
    fn non_graph_vegetation_error_is_a_command_failure() {
        let failure = Error::from(saffron_vegetation::Error::InvalidPlantId).into_failure();
        assert!(matches!(failure, ControlFailureDto::Command { .. }));
    }
}
