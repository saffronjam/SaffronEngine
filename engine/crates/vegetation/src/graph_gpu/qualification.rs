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

    let extrema = GraphGpuProgram::new(
        vec![scalar, scalar, mask],
        vec![
            GraphGpuInstruction::Combine {
                destination: GraphGpuRegister(3),
                left: GraphGpuRegister(0),
                right: GraphGpuRegister(1),
                operation: GraphCombineOperation::Minimum,
            },
            GraphGpuInstruction::Combine {
                destination: GraphGpuRegister(4),
                left: GraphGpuRegister(0),
                right: GraphGpuRegister(1),
                operation: GraphCombineOperation::Maximum,
            },
            GraphGpuInstruction::FieldImportance {
                destination: GraphGpuRegister(5),
                candidates: GraphGpuRegister(2),
                weights: GraphGpuRegister(3),
                threshold: 32_768,
            },
        ],
        Some(GraphGpuRegister(4)),
        GraphGpuRegister(5),
    )
    .unwrap();
    let extrema_inputs = [
        (10_000, 20_000, true),
        (65_536, -65_536, true),
        (i32::MIN, i32::MAX, true),
        (40_000, 40_000, true),
        (0, 0, false),
    ];
    let mut extrema_batch =
        GraphGpuInvocationBatch::with_capacity(&extrema, extrema_inputs.len()).unwrap();
    for (left, right, live) in extrema_inputs {
        extrema_batch
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

    // A zero-scale ramp answers its bias without touching position, so the extreme tick pair that
    // would overflow a scaled ramp must stay valid. The program publishes no value register, which
    // is the other terminal form the ABI allows.
    let constant = GraphGpuProgram::new(
        vec![
            world_tick, world_tick, world_tick, world_tick, world_tick, world_tick, mask,
        ],
        vec![
            GraphGpuInstruction::Gradient {
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
                direction: [65_536, 0, 0],
                scale: 0,
                bias: 4_096,
            },
            GraphGpuInstruction::FieldImportance {
                destination: GraphGpuRegister(8),
                candidates: GraphGpuRegister(6),
                weights: GraphGpuRegister(7),
                threshold: 4_096,
            },
        ],
        None,
        GraphGpuRegister(8),
    )
    .unwrap();
    let constant_inputs = [
        ([0_i128; 3], [0_i128; 3], true),
        ([i128::MAX, i128::MIN, 0], [i128::MIN, i128::MAX, 0], true),
        ([1_i128, 2, 3], [4_i128, 5, 6], false),
    ];
    let mut constant_batch =
        GraphGpuInvocationBatch::with_capacity(&constant, constant_inputs.len()).unwrap();
    for (position, origin, live) in constant_inputs {
        constant_batch
            .push(
                [
                    GraphGpuValue::WorldTick(position[0]),
                    GraphGpuValue::WorldTick(position[1]),
                    GraphGpuValue::WorldTick(position[2]),
                    GraphGpuValue::WorldTick(origin[0]),
                    GraphGpuValue::WorldTick(origin[1]),
                    GraphGpuValue::WorldTick(origin[2]),
                    GraphGpuValue::CandidateMask(live),
                ]
                .map(Ok),
            )
            .unwrap();
    }

    let curve_edges = GraphGpuProgram::new(
        vec![scalar, mask],
        vec![
            GraphGpuInstruction::Curve {
                destination: GraphGpuRegister(2),
                input: GraphGpuRegister(0),
                points: vec![(16_384, 12_345)],
            },
            GraphGpuInstruction::Curve {
                destination: GraphGpuRegister(3),
                input: GraphGpuRegister(0),
                points: vec![(16_384, -65_536), (32_768, 0), (49_152, 65_536)],
            },
            GraphGpuInstruction::Combine {
                destination: GraphGpuRegister(4),
                left: GraphGpuRegister(2),
                right: GraphGpuRegister(3),
                operation: GraphCombineOperation::Add,
            },
            GraphGpuInstruction::FieldImportance {
                destination: GraphGpuRegister(5),
                candidates: GraphGpuRegister(1),
                weights: GraphGpuRegister(4),
                threshold: 12_345,
            },
        ],
        Some(GraphGpuRegister(4)),
        GraphGpuRegister(5),
    )
    .unwrap();
    let curve_edge_inputs = [
        -1_000, 0, 16_384, 20_000, 32_768, 40_000, 49_152, 65_535, 200_000,
    ];
    let mut curve_edge_batch =
        GraphGpuInvocationBatch::with_capacity(&curve_edges, curve_edge_inputs.len()).unwrap();
    for input in curve_edge_inputs {
        curve_edge_batch
            .push(
                [
                    GraphGpuValue::FixedScalar(input),
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
        GraphGpuQualificationBatch {
            program: extrema,
            invocation_batch: extrema_batch,
        },
        GraphGpuQualificationBatch {
            program: constant,
            invocation_batch: constant_batch,
        },
        GraphGpuQualificationBatch {
            program: curve_edges,
            invocation_batch: curve_edge_batch,
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

    /// Qualification mints evidence per operator, so an operator branch the corpus never drove
    /// would ship licensed by a run that skipped it. Coverage is asserted over each operator's
    /// behaviour, not just its opcode.
    #[test]
    fn corpus_drives_every_dual_domain_operator_branch() {
        let corpus = qualification_corpus();
        assert!(
            corpus
                .iter()
                .any(|batch| batch.program.instructions.len() > 1)
        );
        let instructions = || corpus.iter().flat_map(|batch| batch.program.instructions());
        let covered = instructions()
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

        let combines = instructions()
            .filter_map(|instruction| match instruction {
                GraphGpuInstruction::Combine { operation, .. } => Some(*operation),
                _ => None,
            })
            .collect::<Vec<_>>();
        for operation in GraphCombineOperation::ALL {
            assert!(
                combines.contains(operation),
                "combine {operation:?} lacks corpus coverage"
            );
        }

        let gradient_scales = instructions()
            .filter_map(|instruction| match instruction {
                GraphGpuInstruction::Gradient { scale, .. } => Some(*scale),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            gradient_scales.contains(&0),
            "the constant ramp is undriven"
        );
        assert!(gradient_scales.iter().any(|scale| *scale != 0));

        let curve_lengths = instructions()
            .filter_map(|instruction| match instruction {
                GraphGpuInstruction::Curve { points, .. } => Some(points.len()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            curve_lengths.contains(&1),
            "the single-point curve is undriven"
        );
        assert!(curve_lengths.iter().any(|length| *length > 2));

        assert!(
            corpus.iter().any(|batch| batch.program.output().is_none()),
            "the value-free terminal form is undriven"
        );
        assert!(corpus.iter().any(|batch| batch.program.output().is_some()));
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
                0xe7, 0x23, 0x93, 0xbf, 0x77, 0x25, 0xa2, 0x65, 0xa5, 0xa2, 0xcc, 0x37, 0x7b, 0x04,
                0xc9, 0xc0, 0xe0, 0xca, 0x2e, 0x36, 0x8b, 0xf1, 0xe4, 0xb0, 0x09, 0x2c, 0xfc, 0x5f,
                0x1e, 0x9a, 0x5b, 0x30,
            ]
        );
        assert_eq!(
            qualification_reference_hash(),
            [
                0x89, 0xae, 0x9f, 0x1a, 0xf6, 0x8c, 0xd8, 0x04, 0x08, 0x23, 0xf2, 0xa5, 0xcd, 0xdb,
                0x79, 0x88, 0x7e, 0x71, 0x76, 0x43, 0xc6, 0x84, 0x23, 0xfb, 0x1e, 0xbd, 0xdc, 0x14,
                0xbd, 0x78, 0x43, 0xfc,
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
