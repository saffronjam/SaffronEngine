//! Typed, machine-readable capture of physical-GPU numeric conformance evidence.

use std::io::Write;
use std::mem::size_of;
use std::sync::Arc;

use saffron_spatial::{
    DecisionCurve, DecisionScalar, PHILOX4X32_ZERO_VECTOR, RandomDomain, RandomStream,
    UnitInterval, WorldCellKey, div_round_ties_even,
};
use saffron_vegetation::{
    BIOME_NODE_VERSION, GRAPH_GPU_ABI_VERSION, GpuExecutionProfile, GpuShaderArtifactIdentity,
    GraphComputeExecutor, GraphOperator, graph_gpu_abi_hash, qualification_corpus,
    qualification_corpus_hash, qualification_reference_hash,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::compute_dispatch::{ComputeBuffer, ComputeDispatch, ComputeDispatchOutcome};
use crate::{
    Device, Error, Result, ShaderArtifactIdentity, VulkanGraphComputeExecutor,
    validation_issue_count,
};

const CONFORMANCE_SCHEMA_VERSION: u32 = 1;
const SPATIAL_GOLDEN_WORDS: usize = 32;

/// Complete Phase-1 and Phase-3 conformance evidence from one physical Vulkan profile.
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

/// Physical Vulkan identity recorded with semantic evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VulkanProfileEvidence {
    /// Physical-device name.
    pub name: String,
    /// Vulkan physical-device class.
    pub device_type: String,
    /// Vulkan vendor ID.
    pub vendor_id: u32,
    /// Vulkan device ID.
    pub device_id: u32,
    /// Vulkan driver version.
    pub driver_version: u32,
    /// Vulkan API version.
    pub api_version: u32,
    /// Vulkan driver implementation ID.
    pub driver_id: u32,
    /// Lowercase hexadecimal physical-device UUID.
    pub device_uuid: String,
    /// Lowercase hexadecimal driver UUID.
    pub driver_uuid: String,
    /// Whether the profile is running through MoltenVK.
    pub molten_vk: bool,
}

/// Exact generated shader identity included in conformance evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShaderArtifactEvidence {
    /// Logical shader variant.
    pub shader: String,
    /// Entry-point source.
    pub source: String,
    /// SPIR-V artifact filename.
    pub artifact: String,
    /// Compiler-resolved transitive source closure.
    pub source_files: Vec<String>,
    /// Exact compiler identity reported by Slang.
    pub compiler_identity: String,
    /// SHA-256 of the exact compiler identity.
    pub compiler_identity_sha256: String,
    /// Ordered SPIR-V compilation flags.
    pub spirv_flags: Vec<String>,
    /// Ordered preprocessor definitions.
    pub defines: Vec<String>,
    /// SHA-256 of flags, defines, source names, and source bytes.
    pub compile_input_sha256: String,
    /// SHA-256 of the exact loaded SPIR-V bytes.
    pub spirv_sha256: String,
    /// SHA-256 identity of the complete canonical manifest record.
    pub record_sha256: String,
}

/// Phase-1 shared RNG and fixed-numeric golden evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpatialNumericEvidence {
    /// Exact shader artifact executed by the capture.
    pub shader_artifact: ShaderArtifactEvidence,
    /// Number of canonical 32-bit output words.
    pub golden_word_count: usize,
    /// SHA-256 of canonical big-endian Rust reference words.
    pub rust_reference_sha256: String,
    /// SHA-256 of canonical big-endian Slang result words.
    pub slang_sha256: String,
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

/// Phase-3 resident graph-program qualification evidence.
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

/// Validation issue counts around conformance execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationEvidence {
    /// Process-global warning/error count before capture.
    pub before: u64,
    /// Process-global warning/error count after capture.
    pub after: u64,
    /// New validation warnings/errors raised by this capture.
    pub new_issues: u64,
}

/// Executes both conformance phases and returns evidence bound to the exact physical profile.
pub fn capture_compute_conformance(device: Arc<Device>) -> Result<ComputeConformanceEvidence> {
    let before = validation_issue_count();
    let physical_identity = device.device_identity();
    if !physical_identity.is_physical_gpu() {
        return Err(Error::ShaderLoad(format!(
            "platform conformance evidence requires a physical integrated or discrete GPU, found {}",
            physical_identity.device_type_name()
        )));
    }
    let device_type = physical_identity.device_type_name();
    let molten_vk = physical_identity.is_molten_vk();
    let profile = GpuExecutionProfile {
        name: physical_identity.name,
        vendor_id: physical_identity.vendor_id,
        device_id: physical_identity.device_id,
        driver_version: physical_identity.driver_version,
        api_version: physical_identity.api_version,
        driver_id: physical_identity.driver_id,
        device_uuid: physical_identity.device_uuid,
        driver_uuid: physical_identity.driver_uuid,
        molten_vk,
    };
    let spatial_numeric = capture_spatial_numeric(Arc::clone(&device))?;
    let graph_program = capture_graph_program(Arc::clone(&device), &profile)?;
    device.wait_idle()?;
    let after = validation_issue_count();
    if after != before {
        return Err(Error::ShaderLoad(format!(
            "compute conformance raised {} Vulkan validation issues",
            after.saturating_sub(before)
        )));
    }
    Ok(ComputeConformanceEvidence {
        schema_version: CONFORMANCE_SCHEMA_VERSION,
        profile: profile_evidence(&profile, device_type),
        spatial_numeric,
        graph_program,
        validation: ValidationEvidence {
            before,
            after,
            new_issues: after.saturating_sub(before),
        },
    })
}

fn capture_spatial_numeric(device: Arc<Device>) -> Result<SpatialNumericEvidence> {
    let mut dispatcher = ComputeDispatch::new(device, "spatial_numeric_test", 1)?;
    let artifact = artifact_evidence(dispatcher.shader_artifact_identity());
    let outcome = dispatcher.run_interruptible(
        vec![ComputeBuffer::zeroed(
            SPATIAL_GOLDEN_WORDS * size_of::<u32>(),
        )],
        [1, 1, 1],
        || None,
    )?;
    let buffers = match outcome {
        ComputeDispatchOutcome::Complete(buffers) => buffers,
        ComputeDispatchOutcome::Aborted(_) => {
            return Err(Error::ShaderLoad(
                "spatial numeric conformance aborted without a cancellation source".to_owned(),
            ));
        }
    };
    if buffers.len() != 1 || buffers[0].len() != SPATIAL_GOLDEN_WORDS * size_of::<u32>() {
        return Err(Error::ShaderLoad(
            "spatial numeric conformance returned the wrong buffer shape".to_owned(),
        ));
    }
    let actual = buffers[0]
        .chunks_exact(size_of::<u32>())
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
        .collect::<Vec<_>>();
    let expected = spatial_numeric_reference_words();
    if actual != expected {
        let mismatch = actual
            .iter()
            .zip(expected)
            .position(|(actual, expected)| *actual != expected)
            .unwrap_or(0);
        return Err(Error::ShaderLoad(format!(
            "spatial numeric Rust/Slang mismatch at golden word {mismatch}"
        )));
    }
    Ok(SpatialNumericEvidence {
        shader_artifact: artifact,
        golden_word_count: SPATIAL_GOLDEN_WORDS,
        rust_reference_sha256: hash_words(&expected),
        slang_sha256: hash_words(&actual),
    })
}

fn capture_graph_program(
    device: Arc<Device>,
    expected_profile: &GpuExecutionProfile,
) -> Result<GraphProgramEvidence> {
    let executor = VulkanGraphComputeExecutor::new(device)?;
    if executor.profile() != expected_profile {
        return Err(Error::ShaderLoad(
            "graph qualification profile differs from the captured Vulkan profile".to_owned(),
        ));
    }
    let identity = executor.shader_artifact_identity();
    let expected_artifact = GpuShaderArtifactIdentity {
        record_hash: identity.record_sha256().bytes(),
        compile_input_hash: identity.compile_input_sha256().bytes(),
        spirv_hash: identity.spirv_sha256().bytes(),
        compiler_identity_hash: identity.compiler_identity_sha256().bytes(),
    };
    let evidence = executor.qualifications().evidence();
    let first = evidence.first().ok_or_else(|| {
        Error::ShaderLoad("graph qualification produced no operator evidence".to_owned())
    })?;
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
        return Err(Error::ShaderLoad(
            "graph qualification evidence is not canonical for one profile and artifact".to_owned(),
        ));
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
        shader_artifact: artifact_evidence(identity),
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

fn profile_evidence(profile: &GpuExecutionProfile, device_type: &str) -> VulkanProfileEvidence {
    VulkanProfileEvidence {
        name: profile.name.clone(),
        device_type: device_type.to_owned(),
        vendor_id: profile.vendor_id,
        device_id: profile.device_id,
        driver_version: profile.driver_version,
        api_version: profile.api_version,
        driver_id: profile.driver_id,
        device_uuid: hex_bytes(&profile.device_uuid),
        driver_uuid: hex_bytes(&profile.driver_uuid),
        molten_vk: profile.molten_vk,
    }
}

fn artifact_evidence(identity: &ShaderArtifactIdentity) -> ShaderArtifactEvidence {
    ShaderArtifactEvidence {
        shader: identity.shader().to_owned(),
        source: identity.source().to_owned(),
        artifact: identity.artifact().to_owned(),
        source_files: identity.source_files().to_vec(),
        compiler_identity: identity.compiler_identity().to_owned(),
        compiler_identity_sha256: identity.compiler_identity_sha256().to_string(),
        spirv_flags: identity.spirv_flags().to_vec(),
        defines: identity.defines().to_vec(),
        compile_input_sha256: identity.compile_input_sha256().to_string(),
        spirv_sha256: identity.spirv_sha256().to_string(),
        record_sha256: identity.record_sha256().to_string(),
    }
}

pub(crate) fn spatial_numeric_reference_words() -> [u32; SPATIAL_GOLDEN_WORDS] {
    let mut expected = [0_u32; SPATIAL_GOLDEN_WORDS];
    expected[0..4].copy_from_slice(&PHILOX4X32_ZERO_VECTOR);
    let stream = RandomStream::new(conformance_domain());
    expected[4..8].copy_from_slice(&stream.sample(0));
    expected[8..12].copy_from_slice(&stream.sample(u64::MAX));
    let a = DecisionScalar::from_bits(98_304);
    let b = DecisionScalar::from_bits(43_691);
    expected[12] = a
        .checked_add(DecisionScalar::from_bits(-32_768))
        .unwrap()
        .bits() as u32;
    expected[13] = a
        .checked_sub(DecisionScalar::from_bits(-32_768))
        .unwrap()
        .bits() as u32;
    expected[14] = a.checked_mul(b).unwrap().bits() as u32;
    expected[15] = a.checked_div(b).unwrap().bits() as u32;
    expected[16] = DecisionScalar::from_bits(-131_072)
        .lerp(
            DecisionScalar::from_bits(131_072),
            UnitInterval::from_bits(32_768),
        )
        .unwrap()
        .bits() as u32;
    expected[17] = DecisionCurve::new(vec![
        (UnitInterval::ZERO, DecisionScalar::from_bits(-131_072)),
        (UnitInterval::ONE, DecisionScalar::from_bits(131_072)),
    ])
    .unwrap()
    .sample(UnitInterval::from_bits(32_768))
    .unwrap()
    .bits() as u32;
    for (output, (numerator, denominator)) in
        expected[18..22]
            .iter_mut()
            .zip([(5, 2), (-5, 2), (7, 2), (-7, 2)])
    {
        *output = div_round_ties_even(numerator, denominator).unwrap() as i32 as u32;
    }
    expected[24] = chance_word(0, UnitInterval::ZERO);
    expected[25] = chance_word(u32::MAX, UnitInterval::ZERO);
    expected[26] = chance_word(0, UnitInterval::ONE);
    expected[27] = chance_word(u32::MAX, UnitInterval::ONE);
    expected[28] = chance_word(0x8000_0000, UnitInterval::from_bits(32_768));
    expected[29] = chance_word(0x8001_0002, UnitInterval::from_bits(32_768));
    expected[30] = u32::from(stream.chance(0, 0, UnitInterval::from_bits(46_076)));
    expected[31] = u32::from(stream.chance(0, 0, UnitInterval::from_bits(46_077)));
    expected
}

fn chance_word(draw: u32, probability: UnitInterval) -> u32 {
    u32::from(
        u64::from(draw) * u64::from(u16::MAX)
            < u64::from(probability.bits()) * (u64::from(u32::MAX) + 1),
    )
}

fn conformance_domain() -> RandomDomain {
    RandomDomain {
        map: 0x0123_4567_89AB_CDEF_0011_2233_4455_6677,
        node_guid: 0x8877_6655_4433_2211_FEDC_BA98_7654_3210,
        node_semantic_revision: 9,
        seed_namespace: 0xCAFE_BABE_1020_3040_5060_7080_90A0_B0C0,
        cell: WorldCellKey::new(-5, 7, -11, 3).unwrap(),
        candidate: 123_456,
        ancestor: 789,
        species: 0xDEAD_BEEF_CAFE_BABE_1122_3344_5566_7788,
        channel: 4,
    }
}

fn hash_words(words: &[u32]) -> String {
    let mut hasher = Sha256::new();
    for word in words {
        hasher.update(word.to_be_bytes());
    }
    hex_bytes(&hasher.finalize())
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
    fn evidence_json_uses_hexadecimal_hashes_and_uuid_strings() {
        assert_eq!(hex_bytes(&[0x00, 0x7f, 0xff]), "007fff");
        assert_eq!(hash_words(&[0x0102_0304]).len(), 64);
    }
}
