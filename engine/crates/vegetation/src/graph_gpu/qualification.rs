//! The canonical qualification corpus and its pinned identities.

use crate::GraphCombineOperation;
use crate::hash::sha256;

use super::*;

/// One multi-instruction qualification batch.
#[derive(Debug, PartialEq, Eq)]
pub struct GraphGpuQualificationBatch {
    /// Resident program under test.
    pub program: GraphGpuProgram,
    /// Edge, normal, mask, and overflow invocations.
    pub invocation_batch: GraphGpuInvocationBatch,
}

/// Canonical multi-instruction corpus covering every resident opcode and failure semantics.
#[must_use]
pub fn qualification_corpus() -> Vec<GraphGpuQualificationBatch> {
    let mask = GraphGpuRegisterType::CandidateMask;
    let scalar = GraphGpuRegisterType::FixedScalar;
    let unit = GraphGpuRegisterType::Unit;
    let world_tick = GraphGpuRegisterType::WorldTick;

    let chain = GraphGpuProgram::new(
        vec![scalar; 8]
            .into_iter()
            .chain([unit; 3])
            .chain([mask])
            .collect(),
        vec![
            GraphGpuInstruction::Noise {
                destination: GraphGpuRegister(12),
                corners: std::array::from_fn(|index| GraphGpuRegister(index as u32)),
                blend: [
                    GraphGpuRegister(8),
                    GraphGpuRegister(9),
                    GraphGpuRegister(10),
                ],
                amplitude: 65_536,
            },
            GraphGpuInstruction::Curve {
                destination: GraphGpuRegister(13),
                input: GraphGpuRegister(12),
                points: vec![(0, -65_536), (32_768, 0), (u16::MAX, 65_536)],
            },
            GraphGpuInstruction::Remap {
                destination: GraphGpuRegister(14),
                input: GraphGpuRegister(13),
                input_min: -65_536,
                input_max: 65_536,
                output_min: 0,
                output_max: 65_535,
            },
            GraphGpuInstruction::Clamp {
                destination: GraphGpuRegister(15),
                input: GraphGpuRegister(14),
                minimum: 0,
                maximum: 65_535,
            },
            GraphGpuInstruction::FieldImportance {
                destination: GraphGpuRegister(16),
                candidates: GraphGpuRegister(11),
                weights: GraphGpuRegister(15),
                threshold: 32_768,
            },
        ],
        Some(GraphGpuRegister(15)),
        GraphGpuRegister(16),
    )
    .unwrap();
    let chain_inputs = [
        (
            [0, 8_192, 16_384, 24_576, 32_768, 40_960, 49_152, 65_536],
            [0, 0, 0],
            true,
        ),
        (
            [65_536, 49_152, 40_960, 32_768, 24_576, 16_384, 8_192, 0],
            [u16::MAX, 32_768, 1],
            true,
        ),
        ([0; 8], [32_768; 3], false),
    ];
    let mut chain_batch =
        GraphGpuInvocationBatch::with_capacity(&chain, chain_inputs.len()).unwrap();
    for (corners, blend, live) in chain_inputs {
        let inputs = corners
            .into_iter()
            .map(GraphGpuValue::FixedScalar)
            .chain(blend.into_iter().map(GraphGpuValue::Unit))
            .chain([GraphGpuValue::CandidateMask(live)]);
        chain_batch.push(inputs.map(Ok)).unwrap();
    }

    let branch = GraphGpuProgram::new(
        vec![scalar, scalar, mask],
        vec![
            GraphGpuInstruction::Clamp {
                destination: GraphGpuRegister(3),
                input: GraphGpuRegister(0),
                minimum: -131_072,
                maximum: 131_072,
            },
            GraphGpuInstruction::Remap {
                destination: GraphGpuRegister(4),
                input: GraphGpuRegister(1),
                input_min: -65_536,
                input_max: 65_536,
                output_min: -32_768,
                output_max: 32_768,
            },
            GraphGpuInstruction::Combine {
                destination: GraphGpuRegister(5),
                left: GraphGpuRegister(3),
                right: GraphGpuRegister(4),
                operation: GraphCombineOperation::Add,
            },
            GraphGpuInstruction::Curve {
                destination: GraphGpuRegister(6),
                input: GraphGpuRegister(5),
                points: vec![(0, -10), (u16::MAX, 70_000)],
            },
            GraphGpuInstruction::FieldImportance {
                destination: GraphGpuRegister(7),
                candidates: GraphGpuRegister(2),
                weights: GraphGpuRegister(6),
                threshold: 40_000,
            },
        ],
        Some(GraphGpuRegister(6)),
        GraphGpuRegister(7),
    )
    .unwrap();
    let branch_inputs = [
        (10_000, 20_000, true),
        (i32::MAX, 0, true),
        (0, -65_536, false),
    ];
    let mut branch_batch =
        GraphGpuInvocationBatch::with_capacity(&branch, branch_inputs.len()).unwrap();
    for (left, right, live) in branch_inputs {
        branch_batch
            .push(
                [
                    GraphGpuValue::FixedScalar(left),
                    GraphGpuValue::FixedScalar(right),
                    GraphGpuValue::CandidateMask(live),
                ]
                .map(Ok),
            )
            .unwrap();
    }

    let gradient = GraphGpuProgram::new(
        vec![
            world_tick, world_tick, world_tick, world_tick, world_tick, world_tick, mask,
        ],
        vec![GraphGpuInstruction::Gradient {
            destination: GraphGpuRegister(7),
            position: [
                GraphGpuRegister(0),
                GraphGpuRegister(1),
                GraphGpuRegister(2),
            ],
            exact_origin: [
                GraphGpuRegister(3),
                GraphGpuRegister(4),
                GraphGpuRegister(5),
            ],
            direction: [65_536, 32_768, -65_536],
            scale: 32_768,
            bias: 1_024,
        }],
        Some(GraphGpuRegister(7)),
        GraphGpuRegister(6),
    )
    .unwrap();
    let gradient_inputs = [
        ([10_000_i128, -20_000, 30_000], [0_i128; 3]),
        (
            [i128::MAX - 1_000, i128::MAX - 2_000, i128::MAX - 3_000],
            [i128::MAX - 1_100, i128::MAX - 2_100, i128::MAX - 3_100],
        ),
        (
            [i128::MIN + 1_000, i128::MIN + 2_000, i128::MIN + 3_000],
            [i128::MIN + 1_100, i128::MIN + 2_100, i128::MIN + 3_100],
        ),
        ([1_i128 << 80, -(1_i128 << 81), 0], [0_i128; 3]),
        ([i128::MAX, 1, 1], [0_i128; 3]),
    ];
    let mut gradient_batch =
        GraphGpuInvocationBatch::with_capacity(&gradient, gradient_inputs.len()).unwrap();
    for (position, origin) in gradient_inputs {
        gradient_batch
            .push(
                [
                    GraphGpuValue::WorldTick(position[0]),
                    GraphGpuValue::WorldTick(position[1]),
                    GraphGpuValue::WorldTick(position[2]),
                    GraphGpuValue::WorldTick(origin[0]),
                    GraphGpuValue::WorldTick(origin[1]),
                    GraphGpuValue::WorldTick(origin[2]),
                    GraphGpuValue::CandidateMask(true),
                ]
                .map(Ok),
            )
            .unwrap();
    }

    let multiply = GraphGpuProgram::new(
        vec![scalar, scalar, mask],
        vec![GraphGpuInstruction::Combine {
            destination: GraphGpuRegister(3),
            left: GraphGpuRegister(0),
            right: GraphGpuRegister(1),
            operation: GraphCombineOperation::Multiply,
        }],
        Some(GraphGpuRegister(3)),
        GraphGpuRegister(2),
    )
    .unwrap();
    let multiply_inputs = [(98_304, 43_691), (i32::MAX, i32::MAX)];
    let mut multiply_batch =
        GraphGpuInvocationBatch::with_capacity(&multiply, multiply_inputs.len()).unwrap();
    for (left, right) in multiply_inputs {
        multiply_batch
            .push(
                [
                    GraphGpuValue::FixedScalar(left),
                    GraphGpuValue::FixedScalar(right),
                    GraphGpuValue::CandidateMask(true),
                ]
                .map(Ok),
            )
            .unwrap();
    }

    vec![
        GraphGpuQualificationBatch {
            program: chain,
            invocation_batch: chain_batch,
        },
        GraphGpuQualificationBatch {
            program: branch,
            invocation_batch: branch_batch,
        },
        GraphGpuQualificationBatch {
            program: gradient,
            invocation_batch: gradient_batch,
        },
        GraphGpuQualificationBatch {
            program: multiply,
            invocation_batch: multiply_batch,
        },
    ]
}

/// Canonical ABI identity pinned by Rust, Slang, and rendering integration.
#[must_use]
pub fn graph_gpu_abi_hash() -> [u8; 32] {
    sha256(GRAPH_GPU_ABI_DESCRIPTOR)
}

/// Canonical corpus identity required by the qualification registry.
#[must_use]
pub fn qualification_corpus_hash() -> [u8; 32] {
    let mut bytes = b"saffron-anima/vegetation-graph-gpu-corpus/v2\0".to_vec();
    for batch in qualification_corpus() {
        append_program_words(&mut bytes, &batch.program);
        bytes.extend_from_slice(&(batch.invocation_batch.invocation_count() as u64).to_be_bytes());
        for invocation in batch.invocation_batch.invocations() {
            let word_count = invocation.len() * GRAPH_GPU_INVOCATION_WORDS;
            bytes.extend_from_slice(&(word_count as u64).to_be_bytes());
            for word in invocation.iter().flat_map(|value| value.words()) {
                bytes.extend_from_slice(&word.to_be_bytes());
            }
        }
    }
    sha256(&bytes)
}

fn append_program_words(bytes: &mut Vec<u8>, program: &GraphGpuProgram) {
    bytes.extend_from_slice(&(program.encoded_word_count() as u64).to_be_bytes());
    program.for_each_encoded_word(|word| {
        bytes.extend_from_slice(&word.to_be_bytes());
    });
}

/// Canonical Rust output bytes for the complete qualification corpus.
#[must_use]
pub fn qualification_reference_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    for batch in qualification_corpus() {
        for output in evaluate_gpu_program_reference(&batch.program, &batch.invocation_batch)
            .expect("canonical qualification batch matches its resident program")
        {
            for word in output.words() {
                bytes.extend_from_slice(&word.to_be_bytes());
            }
        }
    }
    bytes
}

/// Canonical reference output hash stored in matching per-profile evidence.
#[must_use]
pub fn qualification_reference_hash() -> [u8; 32] {
    sha256(&qualification_reference_bytes())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::program::GraphGpuOperator;
    use super::super::{
        GRAPH_GPU_INSTRUCTION_WORDS, GRAPH_GPU_OUTPUT_WORDS, GRAPH_GPU_PROGRAM_HEADER_WORDS,
    };
    use super::*;
    use crate::{
        Error, GpuExecutionProfile, GpuQualificationRegistry, GpuShaderArtifactIdentity,
        GraphOperator,
    };

    fn qualification_profile() -> GpuExecutionProfile {
        GpuExecutionProfile {
            name: "qualification-test".to_owned(),
            vendor_id: 1,
            device_id: 2,
            driver_version: 3,
            api_version: 4,
            driver_id: 5,
            device_uuid: [6; 16],
            driver_uuid: [7; 16],
            molten_vk: false,
        }
    }

    fn qualification_artifact() -> GpuShaderArtifactIdentity {
        GpuShaderArtifactIdentity {
            record_hash: [8; 32],
            compile_input_hash: [9; 32],
            spirv_hash: [10; 32],
            compiler_identity_hash: [11; 32],
        }
    }

    #[test]
    fn corpus_covers_every_declared_dual_domain_operator_with_resident_programs() {
        let corpus = qualification_corpus();
        assert!(
            corpus
                .iter()
                .any(|batch| batch.program.instructions.len() > 1)
        );
        let covered = corpus
            .iter()
            .flat_map(|batch| batch.program.instructions())
            .map(GraphGpuInstruction::operator)
            .collect::<BTreeSet<_>>();
        for operator in GraphOperator::ALL {
            if let Some(gpu) = GraphGpuOperator::from_graph(*operator) {
                assert!(
                    covered.contains(&gpu),
                    "{} lacks corpus coverage",
                    operator.as_wire()
                );
            }
        }
    }

    #[test]
    fn abi_and_corpus_hashes_are_pinned() {
        assert_eq!(GRAPH_GPU_PROGRAM_HEADER_WORDS, 8);
        assert_eq!(GRAPH_GPU_INSTRUCTION_WORDS, 16);
        assert_eq!(GRAPH_GPU_INVOCATION_WORDS, 4);
        assert_eq!(GRAPH_GPU_OUTPUT_WORDS, 4);
        assert_eq!(
            graph_gpu_abi_hash(),
            [
                0x83, 0x67, 0x71, 0x53, 0xa7, 0x7a, 0x50, 0xcf, 0x9e, 0xb3, 0x9c, 0xef, 0x25, 0xd3,
                0x11, 0xa8, 0xe9, 0xdf, 0x43, 0x2b, 0xeb, 0x1b, 0xdd, 0xe1, 0x23, 0x1e, 0xf8, 0x8e,
                0x32, 0x20, 0x71, 0xb7,
            ]
        );
        assert_eq!(
            qualification_corpus_hash(),
            [
                0x51, 0x07, 0xc1, 0xf5, 0x25, 0x69, 0xbe, 0xc6, 0x92, 0x2e, 0xc0, 0x9e, 0xdd, 0xe5,
                0xfb, 0x03, 0x82, 0xc4, 0xcc, 0xaf, 0x15, 0x6f, 0xda, 0x13, 0x48, 0x60, 0xda, 0x2a,
                0x86, 0xed, 0x61, 0x5c,
            ]
        );
        assert_eq!(
            qualification_reference_hash(),
            [
                0xfb, 0x44, 0xdc, 0xa4, 0x1e, 0xe5, 0x9d, 0x11, 0xc2, 0xfe, 0x72, 0x54, 0x92, 0x25,
                0x27, 0x0a, 0x02, 0x4f, 0xba, 0x00, 0xcd, 0x86, 0xed, 0xe1, 0x01, 0xdd, 0x19, 0xbd,
                0x75, 0x86, 0xc1, 0x9e,
            ]
        );
    }

    #[test]
    fn only_complete_program_profile_and_artifact_evidence_is_admitted() {
        let profile = qualification_profile();
        let artifact = qualification_artifact();
        let registry =
            GpuQualificationRegistry::qualify(profile.clone(), artifact, |program, invocations| {
                evaluate_gpu_program_reference(program, invocations)
            })
            .unwrap();
        assert_eq!(registry.evidence().len(), 7);
        assert!(
            registry
                .evidence()
                .iter()
                .all(|evidence| evidence.profile() == &profile && evidence.artifact() == artifact)
        );

        let mismatch =
            GpuQualificationRegistry::qualify(profile.clone(), artifact, |program, invocations| {
                let mut outputs = evaluate_gpu_program_reference(program, invocations)?;
                outputs[0].value ^= 1;
                Ok(outputs)
            })
            .unwrap_err();
        assert!(matches!(mismatch, Error::GraphDocument { .. }));
    }
}
