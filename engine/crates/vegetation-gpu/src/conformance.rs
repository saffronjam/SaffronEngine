//! Combined spatial-numeric and vegetation graph-program conformance evidence.

use std::io::Write;
use std::sync::Arc;

use saffron_rendering::{
    Device, ShaderArtifactEvidence, SpatialNumericEvidence, ValidationEvidence,
    VulkanProfileEvidence, capture_spatial_numeric, shader_artifact_evidence,
    validation_issue_count, vulkan_profile_evidence,
};
use saffron_vegetation::{
    BIOME_NODE_VERSION, GRAPH_GPU_ABI_VERSION, GpuExecutionProfile, GpuShaderArtifactIdentity,
    GraphComputeExecutor, GraphOperator, graph_gpu_abi_hash, qualification_corpus,
    qualification_corpus_hash, qualification_reference_hash,
};
use serde::Serialize;

use crate::{Error, Result, VulkanGraphComputeExecutor};

const CONFORMANCE_SCHEMA_VERSION: u32 = 1;

/// Complete spatial-numeric and graph-program evidence from one physical Vulkan profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputeConformanceEvidence {
    /// Evidence document schema.
    pub schema_version: u32,
    /// Exact physical device and driver identity.
    pub profile: VulkanProfileEvidence,
    /// Shared RNG and fixed-numeric Rust/Slang golden evidence.
    pub spatial_numeric: SpatialNumericEvidence,
    /// Resident graph-program Rust/Slang qualification evidence.
    pub graph_program: GraphProgramEvidence,
    /// Validation-layer count bracketing both captures.
    pub validation: ValidationEvidence,
}

impl ComputeConformanceEvidence {
    /// Writes this evidence as one pretty-printed JSON value.
    pub fn write_json(&self, writer: impl Write) -> serde_json::Result<()> {
        serde_json::to_writer_pretty(writer, self)
    }
}

/// One graph operator/version qualified by the complete resident corpus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualifiedOperatorEvidence {
    /// Stable operator wire spelling.
    pub operator: String,
    /// Exact semantic version.
    pub semantic_version: u32,
}

/// Resident graph-program qualification evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphProgramEvidence {
    /// Resident graph ABI version.
    pub abi_version: u32,
    /// SHA-256 of the canonical ABI descriptor.
    pub abi_sha256: String,
    /// Number of distinct resident programs in the corpus.
    pub corpus_program_count: usize,
    /// Total ordered invocations across the corpus.
    pub corpus_invocation_count: usize,
    /// SHA-256 of every program and invocation in canonical order.
    pub corpus_sha256: String,
    /// SHA-256 of canonical Rust reference outputs.
    pub rust_reference_sha256: String,
    /// SHA-256 of canonical Slang outputs.
    pub slang_sha256: String,
    /// Exact shader artifact executed by qualification.
    pub shader_artifact: ShaderArtifactEvidence,
    /// Every operator admitted for this exact profile and artifact.
    pub qualified_operators: Vec<QualifiedOperatorEvidence>,
}

/// Executes both conformance phases and returns evidence bound to the exact physical profile.
pub fn capture_compute_conformance(device: Arc<Device>) -> Result<ComputeConformanceEvidence> {
    let before = validation_issue_count();
    let identity = device.device_identity();
    if !identity.is_physical_gpu() {
        return Err(Error::NonPhysicalDevice {
            device_type: identity.device_type_name().to_owned(),
        });
    }
    let profile = GpuExecutionProfile {
        name: identity.name.clone(),
        vendor_id: identity.vendor_id,
        device_id: identity.device_id,
        driver_version: identity.driver_version,
        api_version: identity.api_version,
        driver_id: identity.driver_id,
        device_uuid: identity.device_uuid,
        driver_uuid: identity.driver_uuid,
        molten_vk: identity.is_molten_vk(),
    };
    let profile_evidence = vulkan_profile_evidence(&identity);
    let spatial_numeric = capture_spatial_numeric(Arc::clone(&device))?;
    let graph_program = capture_graph_program(Arc::clone(&device), &profile)?;
    device.wait_idle()?;
    let after = validation_issue_count();
    let new_issues = after.saturating_sub(before);
    if new_issues != 0 {
        return Err(Error::ValidationIssues { count: new_issues });
    }
    Ok(ComputeConformanceEvidence {
        schema_version: CONFORMANCE_SCHEMA_VERSION,
        profile: profile_evidence,
        spatial_numeric,
        graph_program,
        validation: ValidationEvidence {
            before,
            after,
            new_issues,
        },
    })
}

fn capture_graph_program(
    device: Arc<Device>,
    expected_profile: &GpuExecutionProfile,
) -> Result<GraphProgramEvidence> {
    let executor = VulkanGraphComputeExecutor::new(device)?;
    if executor.profile() != expected_profile {
        return Err(Error::ProfileMismatch);
    }
    let identity = executor.shader_artifact_identity();
    let expected_artifact = GpuShaderArtifactIdentity {
        record_hash: identity.record_sha256().bytes(),
        compile_input_hash: identity.compile_input_sha256().bytes(),
        spirv_hash: identity.spirv_sha256().bytes(),
        compiler_identity_hash: identity.compiler_identity_sha256().bytes(),
    };
    let evidence = executor.qualifications().evidence();
    let first = evidence
        .first()
        .ok_or(Error::MissingQualificationEvidence)?;
    let hashes = first.result_hashes();
    let corpus_hash = qualification_corpus_hash();
    let reference_hash = qualification_reference_hash();
    let expected_operators = GraphOperator::ALL
        .iter()
        .copied()
        .filter(|operator| operator.has_slang_executor())
        .collect::<Vec<_>>();
    if first.artifact() != expected_artifact
        || hashes != (corpus_hash, reference_hash, reference_hash)
        || evidence.len() != expected_operators.len()
        || evidence.iter().any(|item| {
            item.profile() != expected_profile
                || item.artifact() != expected_artifact
                || item.result_hashes() != hashes
                || item.operator_version().1 != BIOME_NODE_VERSION
        })
        || evidence
            .iter()
            .zip(expected_operators)
            .any(|(item, expected)| item.operator_version().0 != expected)
    {
        return Err(Error::NoncanonicalQualificationEvidence);
    }
    let corpus = qualification_corpus();
    let corpus_invocation_count = corpus
        .iter()
        .map(|batch| batch.invocation_batch.invocation_count())
        .sum();
    Ok(GraphProgramEvidence {
        abi_version: GRAPH_GPU_ABI_VERSION,
        abi_sha256: hex_bytes(&graph_gpu_abi_hash()),
        corpus_program_count: corpus.len(),
        corpus_invocation_count,
        corpus_sha256: hex_bytes(&corpus_hash),
        rust_reference_sha256: hex_bytes(&reference_hash),
        slang_sha256: hex_bytes(&hashes.2),
        shader_artifact: shader_artifact_evidence(identity),
        qualified_operators: evidence
            .iter()
            .map(|item| {
                let (operator, semantic_version) = item.operator_version();
                QualifiedOperatorEvidence {
                    operator: operator.as_wire().to_owned(),
                    semantic_version,
                }
            })
            .collect(),
    })
}

fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing hexadecimal to String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_hashes_are_lowercase_hexadecimal() {
        assert_eq!(hex_bytes(&[0x00, 0x7f, 0xff]), "007fff");
    }
}
