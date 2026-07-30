//! Device-profile qualification evidence admitted by execution-plan selection.

use crate::Result;
use crate::hash::sha256;

use super::*;

/// Device/profile identity used for `EquivalentGpu` qualification.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GpuExecutionProfile {
    /// Stable profile name used in diagnostics and persisted evidence.
    pub name: String,
    /// Vulkan vendor ID.
    pub vendor_id: u32,
    /// Vulkan device ID.
    pub device_id: u32,
    pub driver_version: u32,
    /// Vulkan API version exposed by the physical device.
    pub api_version: u32,
    /// Vulkan driver implementation identity.
    pub driver_id: u32,
    /// Stable Vulkan physical-device UUID.
    pub device_uuid: [u8; 16],
    /// Stable Vulkan driver UUID.
    pub driver_uuid: [u8; 16],
    /// Whether the profile runs through MoltenVK.
    pub molten_vk: bool,
}

/// Exact verified shader artifact bound into GPU qualification evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GpuShaderArtifactIdentity {
    /// Canonical manifest-record identity, including flags, defines, and source closure.
    pub record_hash: [u8; 32],
    /// Exact compile-input identity.
    pub compile_input_hash: [u8; 32],
    /// Exact loaded SPIR-V identity.
    pub spirv_hash: [u8; 32],
    pub compiler_identity_hash: [u8; 32],
}

/// Matching Rust/Slang bytes for one operator/profile/corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuEquivalenceEvidence {
    profile: GpuExecutionProfile,
    /// Exact shader artifact executed by the qualification corpus.
    artifact: GpuShaderArtifactIdentity,
    operator: GraphOperator,
    version: u32,
    /// Canonical qualification corpus identity.
    corpus_hash: [u8; 32],
    /// Reference output identity.
    rust_hash: [u8; 32],
    /// Slang output identity.
    slang_hash: [u8; 32],
}

impl GpuEquivalenceEvidence {
    /// Exact device/driver profile that executed this evidence.
    #[must_use]
    pub fn profile(&self) -> &GpuExecutionProfile {
        &self.profile
    }

    /// Exact verified shader artifact that executed this evidence.
    #[must_use]
    pub const fn artifact(&self) -> GpuShaderArtifactIdentity {
        self.artifact
    }

    /// Qualified operator and semantic version.
    #[must_use]
    pub const fn operator_version(&self) -> (GraphOperator, u32) {
        (self.operator, self.version)
    }

    /// Corpus, Rust reference, and Slang output identities.
    #[must_use]
    pub const fn result_hashes(&self) -> ([u8; 32], [u8; 32], [u8; 32]) {
        (self.corpus_hash, self.rust_hash, self.slang_hash)
    }
}

/// Qualification evidence admitted by execution-plan selection.
#[derive(Clone, Debug, Default)]
pub struct GpuQualificationRegistry {
    evidence: Vec<GpuEquivalenceEvidence>,
}

impl GpuQualificationRegistry {
    /// Executes and verifies the canonical corpus before minting qualification evidence.
    pub fn qualify(
        profile: GpuExecutionProfile,
        artifact: GpuShaderArtifactIdentity,
        mut execute: impl FnMut(
            &crate::GraphGpuProgram,
            &crate::GraphGpuInvocationBatch,
        ) -> Result<Vec<crate::GraphGpuOutput>>,
    ) -> Result<Self> {
        if profile.name.is_empty()
            || profile.device_uuid == [0; 16]
            || profile.driver_uuid == [0; 16]
            || artifact.record_hash == [0; 32]
            || artifact.compile_input_hash == [0; 32]
            || artifact.spirv_hash == [0; 32]
            || artifact.compiler_identity_hash == [0; 32]
        {
            return Err(graph_document(
                "gpuQualification.identity",
                "device, driver, compiler, and shader artifact identities must be complete",
            ));
        }
        let corpus = crate::graph_gpu::qualification_corpus();
        let mut actual_bytes = Vec::new();
        for (batch_index, batch) in corpus.iter().enumerate() {
            let actual = execute(&batch.program, &batch.invocation_batch)?;
            if actual.len() != batch.invocation_batch.invocation_count() {
                return Err(graph_document(
                    "gpuQualification.outputs",
                    "qualification executor returned the wrong result count",
                ));
            }
            let expected = crate::graph_gpu::evaluate_gpu_program_reference(
                &batch.program,
                &batch.invocation_batch,
            )?;
            if let Some(invocation_index) = actual
                .iter()
                .zip(&expected)
                .position(|(actual, expected)| actual != expected)
            {
                let reason = format!(
                    "qualification mismatch at batch {batch_index}, invocation {invocation_index}"
                );
                return Err(graph_document("gpuQualification.outputs", &reason));
            }
            for output in actual {
                for word in output.words() {
                    actual_bytes.extend_from_slice(&word.to_be_bytes());
                }
            }
        }
        let corpus_hash = crate::graph_gpu::qualification_corpus_hash();
        let reference_hash = crate::graph_gpu::qualification_reference_hash();
        let slang_hash = sha256(&actual_bytes);
        if slang_hash != reference_hash {
            return Err(graph_document(
                "gpuQualification",
                "Rust and Slang result hashes must match the canonical reference",
            ));
        }
        let evidence = GraphOperator::ALL
            .iter()
            .copied()
            .filter(|operator| operator.has_slang_executor())
            .map(|operator| GpuEquivalenceEvidence {
                profile: profile.clone(),
                artifact,
                operator,
                version: BIOME_NODE_VERSION,
                corpus_hash,
                rust_hash: reference_hash,
                slang_hash,
            })
            .collect();
        Ok(Self { evidence })
    }

    /// Complete immutable evidence set in canonical operator order.
    #[must_use]
    pub fn evidence(&self) -> &[GpuEquivalenceEvidence] {
        &self.evidence
    }

    /// Whether one operator/version is qualified for this exact profile.
    #[must_use]
    pub fn contains(
        &self,
        operator: GraphOperator,
        version: u32,
        profile: &GpuExecutionProfile,
    ) -> bool {
        self.evidence.iter().any(|evidence| {
            evidence.operator == operator
                && evidence.version == version
                && &evidence.profile == profile
                && evidence.rust_hash == evidence.slang_hash
        })
    }
}
