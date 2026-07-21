//! Resident graph-program ABI, Rust reference execution, qualification, and scheduling.

use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::time::Instant;

use saffron_spatial::{
    DecisionCurve, DecisionScalar, LOCAL_TICKS_PER_METER, UnitInterval, div_round_ties_even,
};

use crate::hash::sha256;
use crate::memory::{requested_vec_bytes, reserve_exact};
use crate::{
    CompiledBiomeGraph, CompiledGraphNode, CompiledGraphUnit, Error, GpuExecutionProfile,
    GpuQualificationRegistry, GraphAuthority, GraphCancellationToken, GraphCombineOperation,
    GraphDomain, GraphNodeAddress, GraphOperator, QualifiedGraphPin, Result,
};

/// Canonical resident-program ABI schema.
pub const GRAPH_GPU_ABI_VERSION: u32 = 1;
/// Fixed program header words.
pub const GRAPH_GPU_PROGRAM_HEADER_WORDS: usize = 8;
/// Fixed words per SSA instruction.
pub const GRAPH_GPU_INSTRUCTION_WORDS: usize = 16;
/// Fixed words per typed invocation input register.
pub const GRAPH_GPU_INVOCATION_WORDS: usize = 4;
/// Fixed output words per candidate: valid, mask, type, and scalar value.
pub const GRAPH_GPU_OUTPUT_WORDS: usize = 4;
/// Maximum external typed registers loaded per candidate.
pub const GRAPH_GPU_MAX_INPUTS: usize = 32;
/// Maximum instructions executed by one resident program.
pub const GRAPH_GPU_MAX_INSTRUCTIONS: usize = 32;
/// Maximum input plus SSA result registers.
pub const GRAPH_GPU_MAX_REGISTERS: usize = 64;
/// Maximum points embedded in one bounded curve instruction.
pub const GRAPH_GPU_MAX_CURVE_POINTS: usize = 6;

const GRAPH_GPU_MAGIC: u32 = 0x5341_4750;
const GRAPH_GPU_NO_REGISTER: u32 = u32::MAX;
const GRAPH_GPU_ABI_DESCRIPTOR: &[u8] = b"saffron-anima/graph-gpu-program/v1\0header=magic,version,inputCount,instructionCount,registerCount,outputRegister,outputType,maskRegister;inputTypes=inputCount*u32;instruction=opcode,destination,14*operand;input=invocation*inputCount*uint4-little-limb;output=valid,mask,type,value;maxInputs=32;maxInstructions=32;maxRegisters=64;maxCurvePoints=6";

/// Stable instruction opcode shared with `vegetation_graph.slang`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum GraphGpuOperator {
    /// Coherent eight-corner value-noise interpolation followed by amplitude.
    Noise = 1,
    /// Three fixed coordinates multiplied by a fixed axis.
    Gradient = 2,
    /// Bounded exact piecewise-linear curve.
    Curve = 3,
    /// Fixed range remapping with clamped input.
    Remap = 4,
    /// Fixed scalar add/multiply/minimum/maximum.
    Combine = 5,
    /// Fixed scalar clamp.
    Clamp = 6,
    /// Candidate-mask threshold intersection.
    FieldImportance = 7,
}

impl GraphGpuOperator {
    /// Maps a graph operator only when resident-program semantics are complete.
    #[must_use]
    pub const fn from_graph(operator: GraphOperator) -> Option<Self> {
        Some(match operator {
            GraphOperator::Noise => Self::Noise,
            GraphOperator::Gradient => Self::Gradient,
            GraphOperator::Curve => Self::Curve,
            GraphOperator::Remap => Self::Remap,
            GraphOperator::Combine => Self::Combine,
            GraphOperator::Clamp => Self::Clamp,
            GraphOperator::FieldImportance => Self::FieldImportance,
            _ => return None,
        })
    }
}

/// Typed register kind shared by Rust validation and the Slang interpreter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum GraphGpuRegisterType {
    /// Signed Q15.16 scalar.
    FixedScalar = 1,
    /// Closed normalized `u16` value.
    Unit = 2,
    /// Candidate-liveness mask.
    CandidateMask = 3,
    /// Signed world-position tick encoded as four little-endian two's-complement limbs.
    WorldTick = 4,
}

/// One bounded SSA register index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphGpuRegister(pub u32);

/// One typed per-candidate program input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphGpuValue {
    /// Signed Q15.16 scalar.
    FixedScalar(i32),
    /// Closed normalized value.
    Unit(u16),
    /// Candidate-liveness mask.
    CandidateMask(bool),
    /// One exact signed world-position tick.
    WorldTick(i128),
}

impl GraphGpuValue {
    /// Exact register kind.
    #[must_use]
    pub const fn register_type(self) -> GraphGpuRegisterType {
        match self {
            Self::FixedScalar(_) => GraphGpuRegisterType::FixedScalar,
            Self::Unit(_) => GraphGpuRegisterType::Unit,
            Self::CandidateMask(_) => GraphGpuRegisterType::CandidateMask,
            Self::WorldTick(_) => GraphGpuRegisterType::WorldTick,
        }
    }

    /// Canonical three-lane native representation.
    #[must_use]
    pub const fn words(self) -> [u32; GRAPH_GPU_INVOCATION_WORDS] {
        match self {
            Self::FixedScalar(value) => [value as u32, 0, 0, 0],
            Self::Unit(value) => [value as u32, 0, 0, 0],
            Self::CandidateMask(value) => [value as u32, 0, 0, 0],
            Self::WorldTick(value) => [
                value as u32,
                (value >> 32) as u32,
                (value >> 64) as u32,
                (value >> 96) as u32,
            ],
        }
    }
}

/// One typed SSA instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphGpuInstruction {
    /// Coherent noise from eight corners and three interpolation weights.
    Noise {
        /// New scalar register.
        destination: GraphGpuRegister,
        /// Eight scalar corner registers.
        corners: [GraphGpuRegister; 8],
        /// Three unit interpolation registers.
        blend: [GraphGpuRegister; 3],
        /// Q15.16 amplitude.
        amplitude: i32,
    },
    /// Position multiplied component-wise by a constant axis.
    Gradient {
        /// New scalar register.
        destination: GraphGpuRegister,
        /// Exact world-position tick registers.
        position: [GraphGpuRegister; 3],
        /// Exact origin tick registers.
        exact_origin: [GraphGpuRegister; 3],
        /// Non-zero Q15.16 direction.
        direction: [i32; 3],
        /// Q15.16 ramp scale.
        scale: i32,
        /// Q15.16 ramp bias.
        bias: i32,
    },
    /// Exact bounded piecewise-linear curve with endpoint clamping.
    Curve {
        /// New scalar register.
        destination: GraphGpuRegister,
        /// Scalar input, clamped to the unit domain.
        input: GraphGpuRegister,
        /// Ascending unit/fixed curve points.
        points: Vec<(u16, i32)>,
    },
    /// Exact fixed remapping.
    Remap {
        /// New scalar register.
        destination: GraphGpuRegister,
        /// Scalar input.
        input: GraphGpuRegister,
        /// Input minimum.
        input_min: i32,
        /// Input maximum.
        input_max: i32,
        /// Output minimum.
        output_min: i32,
        /// Output maximum.
        output_max: i32,
    },
    /// Binary scalar combination, including branch convergence.
    Combine {
        /// New scalar register.
        destination: GraphGpuRegister,
        /// Left scalar branch.
        left: GraphGpuRegister,
        /// Right scalar branch.
        right: GraphGpuRegister,
        /// Exact combination operation.
        operation: GraphCombineOperation,
    },
    /// Exact scalar clamp.
    Clamp {
        /// New scalar register.
        destination: GraphGpuRegister,
        /// Scalar input.
        input: GraphGpuRegister,
        /// Inclusive minimum.
        minimum: i32,
        /// Inclusive maximum.
        maximum: i32,
    },
    /// Intersects an existing candidate mask with a scalar threshold.
    FieldImportance {
        /// New mask register.
        destination: GraphGpuRegister,
        /// Prior candidate mask.
        candidates: GraphGpuRegister,
        /// Scalar weight.
        weights: GraphGpuRegister,
        /// Inclusive unit threshold.
        threshold: u16,
    },
}

impl GraphGpuInstruction {
    /// Instruction opcode.
    #[must_use]
    pub const fn operator(&self) -> GraphGpuOperator {
        match self {
            Self::Noise { .. } => GraphGpuOperator::Noise,
            Self::Gradient { .. } => GraphGpuOperator::Gradient,
            Self::Curve { .. } => GraphGpuOperator::Curve,
            Self::Remap { .. } => GraphGpuOperator::Remap,
            Self::Combine { .. } => GraphGpuOperator::Combine,
            Self::Clamp { .. } => GraphGpuOperator::Clamp,
            Self::FieldImportance { .. } => GraphGpuOperator::FieldImportance,
        }
    }

    /// New SSA register.
    #[must_use]
    pub const fn destination(&self) -> GraphGpuRegister {
        match self {
            Self::Noise { destination, .. }
            | Self::Gradient { destination, .. }
            | Self::Curve { destination, .. }
            | Self::Remap { destination, .. }
            | Self::Combine { destination, .. }
            | Self::Clamp { destination, .. }
            | Self::FieldImportance { destination, .. } => *destination,
        }
    }

    const fn result_type(&self) -> GraphGpuRegisterType {
        match self {
            Self::FieldImportance { .. } => GraphGpuRegisterType::CandidateMask,
            _ => GraphGpuRegisterType::FixedScalar,
        }
    }

    fn words(&self) -> [u32; GRAPH_GPU_INSTRUCTION_WORDS] {
        let mut words = [0_u32; GRAPH_GPU_INSTRUCTION_WORDS];
        words[0] = self.operator() as u32;
        words[1] = self.destination().0;
        match self {
            Self::Noise {
                corners,
                blend,
                amplitude,
                ..
            } => {
                for (word, register) in words[2..10].iter_mut().zip(corners) {
                    *word = register.0;
                }
                for (word, register) in words[10..13].iter_mut().zip(blend) {
                    *word = register.0;
                }
                words[13] = *amplitude as u32;
            }
            Self::Gradient {
                position,
                exact_origin,
                direction,
                scale,
                bias,
                ..
            } => {
                words[2] = position[0].0;
                words[3] = position[1].0;
                words[4] = position[2].0;
                words[5] = exact_origin[0].0;
                words[6] = exact_origin[1].0;
                words[7] = exact_origin[2].0;
                words[8] = direction[0] as u32;
                words[9] = direction[1] as u32;
                words[10] = direction[2] as u32;
                words[11] = *scale as u32;
                words[12] = *bias as u32;
            }
            Self::Curve { input, points, .. } => {
                words[2] = input.0;
                words[3] = points.len() as u32;
                for (index, (x, y)) in points.iter().enumerate() {
                    words[4 + index * 2] = u32::from(*x);
                    words[5 + index * 2] = *y as u32;
                }
            }
            Self::Remap {
                input,
                input_min,
                input_max,
                output_min,
                output_max,
                ..
            } => {
                words[2] = input.0;
                words[3] = *input_min as u32;
                words[4] = *input_max as u32;
                words[5] = *output_min as u32;
                words[6] = *output_max as u32;
            }
            Self::Combine {
                left,
                right,
                operation,
                ..
            } => {
                words[2] = left.0;
                words[3] = right.0;
                words[4] = combine_tag(*operation);
            }
            Self::Clamp {
                input,
                minimum,
                maximum,
                ..
            } => {
                words[2] = input.0;
                words[3] = *minimum as u32;
                words[4] = *maximum as u32;
            }
            Self::FieldImportance {
                candidates,
                weights,
                threshold,
                ..
            } => {
                words[2] = candidates.0;
                words[3] = weights.0;
                words[4] = u32::from(*threshold);
            }
        }
        words
    }
}

/// One validated bounded program shared by every candidate in a dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphGpuProgram {
    input_types: Vec<GraphGpuRegisterType>,
    instructions: Vec<GraphGpuInstruction>,
    output: Option<GraphGpuRegister>,
    candidate_mask: GraphGpuRegister,
    register_types: Vec<GraphGpuRegisterType>,
}

impl GraphGpuProgram {
    /// Validates SSA ordering, register types, bounds, immediates, and terminal outputs.
    pub fn new(
        input_types: Vec<GraphGpuRegisterType>,
        instructions: Vec<GraphGpuInstruction>,
        output: Option<GraphGpuRegister>,
        candidate_mask: GraphGpuRegister,
    ) -> Result<Self> {
        if input_types.is_empty() || input_types.len() > GRAPH_GPU_MAX_INPUTS {
            return Err(program_error(
                "inputs",
                "input register count is outside the ABI bounds",
            ));
        }
        if instructions.is_empty() || instructions.len() > GRAPH_GPU_MAX_INSTRUCTIONS {
            return Err(program_error(
                "instructions",
                "instruction count is outside the ABI bounds",
            ));
        }
        let register_count = input_types
            .len()
            .checked_add(instructions.len())
            .ok_or(Error::NumericOverflow)?;
        if register_count > GRAPH_GPU_MAX_REGISTERS {
            return Err(program_error(
                "registers",
                "register count exceeds the ABI bound",
            ));
        }
        let mut register_types = Vec::new();
        reserve_exact(
            &mut register_types,
            register_count,
            "resident graph register types",
        )?;
        register_types.extend_from_slice(&input_types);
        for (index, instruction) in instructions.iter().enumerate() {
            let expected = input_types.len() + index;
            if usize::try_from(instruction.destination().0).ok() != Some(expected) {
                return Err(program_error(
                    "instructions.destination",
                    "SSA destinations must be contiguous and assigned exactly once",
                ));
            }
            validate_instruction(instruction, &register_types)?;
            register_types.push(instruction.result_type());
        }
        let output_type = output
            .map(|register| register_type(&register_types, register, "output"))
            .transpose()?;
        if output_type.is_some_and(|kind| kind == GraphGpuRegisterType::CandidateMask) {
            return Err(program_error(
                "output",
                "candidate masks use the dedicated terminal mask register",
            ));
        }
        if register_type(&register_types, candidate_mask, "candidateMask")?
            != GraphGpuRegisterType::CandidateMask
        {
            return Err(program_error(
                "candidateMask",
                "terminal candidate mask register has the wrong type",
            ));
        }
        Ok(Self {
            input_types,
            instructions,
            output,
            candidate_mask,
            register_types,
        })
    }

    /// External input register kinds in canonical register order.
    #[must_use]
    pub fn input_types(&self) -> &[GraphGpuRegisterType] {
        &self.input_types
    }

    /// Canonical SSA instruction order.
    #[must_use]
    pub fn instructions(&self) -> &[GraphGpuInstruction] {
        &self.instructions
    }

    /// Optional terminal value register.
    #[must_use]
    pub const fn output(&self) -> Option<GraphGpuRegister> {
        self.output
    }

    /// Optional terminal value kind.
    #[must_use]
    pub fn output_type(&self) -> Option<GraphGpuRegisterType> {
        self.output
            .map(|register| self.register_types[register.0 as usize])
    }

    /// Terminal candidate-mask register.
    #[must_use]
    pub const fn candidate_mask(&self) -> GraphGpuRegister {
        self.candidate_mask
    }

    /// Exact encoded word count without materializing the program buffer.
    #[must_use]
    pub fn encoded_word_count(&self) -> usize {
        GRAPH_GPU_PROGRAM_HEADER_WORDS
            + self.input_types.len()
            + self.instructions.len() * GRAPH_GPU_INSTRUCTION_WORDS
    }

    /// Exact encoded byte count without materializing the program buffer.
    pub fn encoded_byte_count(&self) -> Result<usize> {
        self.encoded_word_count()
            .checked_mul(size_of::<u32>())
            .ok_or(Error::NumericOverflow)
    }

    /// Retained heap bytes requested by the program's actual vector capacities.
    pub fn requested_memory_bytes(&self) -> Result<u64> {
        let mut bytes = requested_vec_bytes::<GraphGpuRegisterType>(self.input_types.capacity())?;
        bytes = bytes
            .checked_add(requested_vec_bytes::<GraphGpuInstruction>(
                self.instructions.capacity(),
            )?)
            .ok_or(Error::NumericOverflow)?;
        bytes = bytes
            .checked_add(requested_vec_bytes::<GraphGpuRegisterType>(
                self.register_types.capacity(),
            )?)
            .ok_or(Error::NumericOverflow)?;
        for instruction in &self.instructions {
            if let GraphGpuInstruction::Curve { points, .. } = instruction {
                bytes = bytes
                    .checked_add(requested_vec_bytes::<(u16, i32)>(points.capacity())?)
                    .ok_or(Error::NumericOverflow)?;
            }
        }
        Ok(bytes)
    }

    /// Exact tightly packed program-buffer words consumed by Slang.
    pub fn encoded_words(&self) -> Result<Vec<u32>> {
        let mut words = Vec::new();
        reserve_exact(
            &mut words,
            self.encoded_word_count(),
            "resident graph program words",
        )?;
        self.for_each_encoded_word(|word| words.push(word));
        Ok(words)
    }

    fn for_each_encoded_word(&self, mut append: impl FnMut(u32)) {
        let output_type = self.output_type().map_or(0, |kind| kind as u32);
        for word in [
            GRAPH_GPU_MAGIC,
            GRAPH_GPU_ABI_VERSION,
            self.input_types.len() as u32,
            self.instructions.len() as u32,
            self.register_types.len() as u32,
            self.output
                .map_or(GRAPH_GPU_NO_REGISTER, |register| register.0),
            output_type,
            self.candidate_mask.0,
        ] {
            append(word);
        }
        for kind in &self.input_types {
            append(*kind as u32);
        }
        for instruction in &self.instructions {
            for word in instruction.words() {
                append(word);
            }
        }
    }
}

/// One validated flat candidate batch for a single resident-program input schema.
#[derive(Debug, PartialEq, Eq)]
pub struct GraphGpuInvocationBatch {
    input_types: [GraphGpuRegisterType; GRAPH_GPU_MAX_INPUTS],
    input_stride: usize,
    invocation_count: usize,
    invocation_capacity: usize,
    inputs: Vec<GraphGpuValue>,
}

impl GraphGpuInvocationBatch {
    /// Requested flat-buffer bytes for a batch capacity before allocation.
    pub fn requested_memory_bytes_for_capacity(
        program: &GraphGpuProgram,
        invocation_capacity: usize,
    ) -> Result<u64> {
        let value_capacity = invocation_capacity
            .checked_mul(program.input_types.len())
            .ok_or(Error::NumericOverflow)?;
        value_capacity
            .checked_mul(GRAPH_GPU_INVOCATION_WORDS)
            .and_then(|words| words.checked_mul(size_of::<u32>()))
            .ok_or(Error::NumericOverflow)?;
        requested_vec_bytes::<GraphGpuValue>(value_capacity)
    }

    /// Reserves one flat buffer for an exact maximum number of invocations.
    pub fn with_capacity(program: &GraphGpuProgram, invocation_capacity: usize) -> Result<Self> {
        let input_stride = program.input_types.len();
        let value_capacity = invocation_capacity
            .checked_mul(input_stride)
            .ok_or(Error::NumericOverflow)?;
        Self::requested_memory_bytes_for_capacity(program, invocation_capacity)?;
        let mut input_types = [GraphGpuRegisterType::FixedScalar; GRAPH_GPU_MAX_INPUTS];
        input_types[..input_stride].copy_from_slice(&program.input_types);
        let mut inputs = Vec::new();
        reserve_exact(
            &mut inputs,
            value_capacity,
            "resident graph invocation batch",
        )?;
        Ok(Self {
            input_types,
            input_stride,
            invocation_count: 0,
            invocation_capacity,
            inputs,
        })
    }

    /// Appends one fallibly produced invocation after validating its exact register signature.
    pub fn push(&mut self, inputs: impl IntoIterator<Item = Result<GraphGpuValue>>) -> Result<()> {
        if self.invocation_count == self.invocation_capacity {
            return Err(program_error(
                "invocationBatch.capacity",
                "invocation count exceeds the reserved batch capacity",
            ));
        }
        let mut validated = [GraphGpuValue::FixedScalar(0); GRAPH_GPU_MAX_INPUTS];
        let mut input_count = 0;
        for value in inputs {
            let value = value?;
            if input_count == self.input_stride
                || value.register_type() != self.input_types[input_count]
            {
                return Err(program_error(
                    "invocationBatch.inputs",
                    "invocation inputs do not match the batch register signature",
                ));
            }
            validated[input_count] = value;
            input_count += 1;
        }
        if input_count != self.input_stride {
            return Err(program_error(
                "invocationBatch.inputs",
                "invocation inputs do not match the batch register signature",
            ));
        }
        self.inputs
            .extend_from_slice(&validated[..self.input_stride]);
        self.invocation_count += 1;
        Ok(())
    }

    /// Program input signature carried by every invocation.
    #[must_use]
    pub fn input_types(&self) -> &[GraphGpuRegisterType] {
        &self.input_types[..self.input_stride]
    }

    /// Typed value count in each invocation.
    #[must_use]
    pub const fn input_stride(&self) -> usize {
        self.input_stride
    }

    /// Number of validated invocations currently stored.
    #[must_use]
    pub const fn invocation_count(&self) -> usize {
        self.invocation_count
    }

    /// Maximum number of invocations admitted by this allocation.
    #[must_use]
    pub const fn invocation_capacity(&self) -> usize {
        self.invocation_capacity
    }

    /// Actual flat value capacity retained by the backing vector.
    #[must_use]
    pub fn value_capacity(&self) -> usize {
        self.inputs.capacity()
    }

    /// All typed invocation inputs in invocation-major register order.
    #[must_use]
    pub fn flat_inputs(&self) -> &[GraphGpuValue] {
        &self.inputs
    }

    /// Ordered fixed-stride invocation views.
    pub fn invocations(&self) -> impl ExactSizeIterator<Item = &[GraphGpuValue]> + Clone {
        self.inputs.chunks_exact(self.input_stride)
    }

    /// Exact encoded word count without materializing the invocation buffer.
    #[must_use]
    pub fn encoded_word_count(&self) -> usize {
        self.inputs.len() * GRAPH_GPU_INVOCATION_WORDS
    }

    /// Exact encoded byte count without materializing the invocation buffer.
    pub fn encoded_byte_count(&self) -> Result<usize> {
        self.encoded_word_count()
            .checked_mul(size_of::<u32>())
            .ok_or(Error::NumericOverflow)
    }

    /// Retained heap bytes requested by the flat input vector's actual capacity.
    pub fn requested_memory_bytes(&self) -> Result<u64> {
        requested_vec_bytes::<GraphGpuValue>(self.inputs.capacity())
    }

    /// Validates that the batch carries the resident program's exact input schema.
    pub fn validate_program(&self, program: &GraphGpuProgram) -> Result<()> {
        if self.input_types() != program.input_types() {
            return Err(program_error(
                "invocationBatch.schema",
                "invocation batch does not match the program register signature",
            ));
        }
        Ok(())
    }
}

/// One fixed-width resident-program result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphGpuOutput {
    /// Whether every instruction completed without invalid input or overflow.
    pub valid: bool,
    /// Terminal candidate liveness after every mask instruction.
    pub candidate_mask: bool,
    /// Optional terminal value kind.
    pub value_type: Option<GraphGpuRegisterType>,
    /// Terminal scalar value.
    pub value: u32,
}

impl GraphGpuOutput {
    /// Stable words read from the Slang executor.
    #[must_use]
    pub const fn words(self) -> [u32; GRAPH_GPU_OUTPUT_WORDS] {
        [
            self.valid as u32,
            self.candidate_mask as u32,
            match self.value_type {
                Some(kind) => kind as u32,
                None => 0,
            },
            self.value,
        ]
    }
}

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

/// Executes one resident program through canonical Rust semantics.
pub fn evaluate_gpu_program_reference(
    program: &GraphGpuProgram,
    invocation_batch: &GraphGpuInvocationBatch,
) -> Result<Vec<GraphGpuOutput>> {
    invocation_batch.validate_program(program)?;
    Ok(invocation_batch
        .invocations()
        .map(|invocation| evaluate_gpu_invocation_reference(program, invocation))
        .collect())
}

fn evaluate_gpu_invocation_reference(
    program: &GraphGpuProgram,
    invocation: &[GraphGpuValue],
) -> GraphGpuOutput {
    let value_type = program
        .output
        .map(|register| program.register_types[register.0 as usize]);
    let result = evaluate_gpu_invocation_reference_inner(program, invocation);
    match result {
        Ok((candidate_mask, value)) => GraphGpuOutput {
            valid: true,
            candidate_mask,
            value_type,
            value,
        },
        Err(_) => GraphGpuOutput {
            valid: false,
            candidate_mask: false,
            value_type,
            value: 0,
        },
    }
}

fn evaluate_gpu_invocation_reference_inner(
    program: &GraphGpuProgram,
    invocation: &[GraphGpuValue],
) -> Result<(bool, u32)> {
    let mut registers = [GraphGpuValue::FixedScalar(0); GRAPH_GPU_MAX_REGISTERS];
    registers[..invocation.len()].copy_from_slice(invocation);
    for instruction in &program.instructions {
        let value = match instruction {
            GraphGpuInstruction::Noise {
                corners,
                blend,
                amplitude,
                ..
            } => {
                let corners: [DecisionScalar; 8] =
                    std::array::from_fn(|index| fixed(registers[corners[index].0 as usize]));
                let blend: [UnitInterval; 3] =
                    std::array::from_fn(|index| unit(registers[blend[index].0 as usize]));
                let x00 = corners[0].lerp(corners[1], blend[0])?;
                let x10 = corners[2].lerp(corners[3], blend[0])?;
                let x01 = corners[4].lerp(corners[5], blend[0])?;
                let x11 = corners[6].lerp(corners[7], blend[0])?;
                let y0 = x00.lerp(x10, blend[1])?;
                let y1 = x01.lerp(x11, blend[1])?;
                GraphGpuValue::FixedScalar(
                    y0.lerp(y1, blend[2])?
                        .checked_mul(DecisionScalar::from_bits(*amplitude))?
                        .bits(),
                )
            }
            GraphGpuInstruction::Gradient {
                position,
                exact_origin,
                direction,
                scale,
                bias,
                ..
            } => GraphGpuValue::FixedScalar(evaluate_gradient_ramp(
                position.map(|register| world_tick(registers[register.0 as usize])),
                exact_origin.map(|register| world_tick(registers[register.0 as usize])),
                *direction,
                *scale,
                *bias,
            )?),
            GraphGpuInstruction::Curve { input, points, .. } => {
                let value = fixed(registers[input.0 as usize])
                    .bits()
                    .clamp(0, i32::from(u16::MAX));
                let curve = DecisionCurve::new(
                    points
                        .iter()
                        .map(|(x, y)| (UnitInterval::from_bits(*x), DecisionScalar::from_bits(*y)))
                        .collect(),
                )?;
                GraphGpuValue::FixedScalar(
                    curve.sample(UnitInterval::from_bits(value as u16))?.bits(),
                )
            }
            GraphGpuInstruction::Remap {
                input,
                input_min,
                input_max,
                output_min,
                output_max,
                ..
            } => {
                let input = fixed(registers[input.0 as usize]);
                let input_min = DecisionScalar::from_bits(*input_min);
                let input_max = DecisionScalar::from_bits(*input_max);
                let output_min = DecisionScalar::from_bits(*output_min);
                let output_max = DecisionScalar::from_bits(*output_max);
                let ratio = input
                    .clamp(input_min, input_max)
                    .checked_sub(input_min)?
                    .checked_div(input_max.checked_sub(input_min)?)?;
                GraphGpuValue::FixedScalar(
                    output_min
                        .checked_add(output_max.checked_sub(output_min)?.checked_mul(ratio)?)?
                        .bits(),
                )
            }
            GraphGpuInstruction::Combine {
                left,
                right,
                operation,
                ..
            } => {
                let left = fixed(registers[left.0 as usize]);
                let right = fixed(registers[right.0 as usize]);
                let value = match operation {
                    GraphCombineOperation::Add => left.checked_add(right)?,
                    GraphCombineOperation::Multiply => left.checked_mul(right)?,
                    GraphCombineOperation::Minimum => left.min(right),
                    GraphCombineOperation::Maximum => left.max(right),
                };
                GraphGpuValue::FixedScalar(value.bits())
            }
            GraphGpuInstruction::Clamp {
                input,
                minimum,
                maximum,
                ..
            } => GraphGpuValue::FixedScalar(
                fixed(registers[input.0 as usize])
                    .clamp(
                        DecisionScalar::from_bits(*minimum),
                        DecisionScalar::from_bits(*maximum),
                    )
                    .bits(),
            ),
            GraphGpuInstruction::FieldImportance {
                candidates,
                weights,
                threshold,
                ..
            } => GraphGpuValue::CandidateMask(
                candidate_mask(registers[candidates.0 as usize])
                    && fixed(registers[weights.0 as usize]).bits() >= i32::from(*threshold),
            ),
        };
        registers[instruction.destination().0 as usize] = value;
    }
    let candidate_mask = candidate_mask(registers[program.candidate_mask.0 as usize]);
    let value = program
        .output
        .map(|register| registers[register.0 as usize].words()[0])
        .unwrap_or(0);
    Ok((candidate_mask, value))
}

fn fixed(value: GraphGpuValue) -> DecisionScalar {
    match value {
        GraphGpuValue::FixedScalar(value) => DecisionScalar::from_bits(value),
        _ => unreachable!("validated program register type"),
    }
}

fn unit(value: GraphGpuValue) -> UnitInterval {
    match value {
        GraphGpuValue::Unit(value) => UnitInterval::from_bits(value),
        _ => unreachable!("validated program register type"),
    }
}

fn world_tick(value: GraphGpuValue) -> i128 {
    match value {
        GraphGpuValue::WorldTick(value) => value,
        _ => unreachable!("validated program register type"),
    }
}

fn candidate_mask(value: GraphGpuValue) -> bool {
    match value {
        GraphGpuValue::CandidateMask(value) => value,
        _ => unreachable!("validated program register type"),
    }
}

/// Executes the canonical checked scalar directional-ramp numeric contract.
pub fn evaluate_gradient_ramp(
    position: [i128; 3],
    exact_origin: [i128; 3],
    direction: [i32; 3],
    scale: i32,
    bias: i32,
) -> Result<i32> {
    if scale == 0 {
        return Ok(bias);
    }
    let mut dot_numerator = 0_i128;
    for lane in 0..3 {
        let delta = position[lane]
            .checked_sub(exact_origin[lane])
            .ok_or(Error::NumericOverflow)?;
        let term = delta
            .checked_mul(i128::from(direction[lane]))
            .ok_or(Error::NumericOverflow)?;
        dot_numerator = dot_numerator
            .checked_add(term)
            .ok_or(Error::NumericOverflow)?;
    }
    let denominator = i128::from(LOCAL_TICKS_PER_METER) * 65_536;
    let numerator = dot_numerator
        .checked_mul(i128::from(scale))
        .and_then(|value| value.checked_add(i128::from(bias).checked_mul(denominator)?))
        .ok_or(Error::NumericOverflow)?;
    i32::try_from(div_round_ties_even(numerator, denominator)?).map_err(|_| Error::NumericOverflow)
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

/// Runtime executor for one resident program over an ordered candidate batch.
pub trait GraphComputeExecutor: Send + Sync {
    /// Exact Vulkan profile against which qualification evidence was captured.
    fn profile(&self) -> &GpuExecutionProfile;
    /// Complete-program evidence admitted for this profile.
    fn qualifications(&self) -> &GpuQualificationRegistry;
    /// Executes one program without CPU round-trips between its instructions.
    fn execute_program(
        &self,
        program: &GraphGpuProgram,
        invocation_batch: &GraphGpuInvocationBatch,
        cancellation: &GraphCancellationToken,
        deadline: Instant,
    ) -> Result<Vec<GraphGpuOutput>>;
}

/// Execution domain chosen for one group without changing graph semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GraphExecutionDomain {
    /// Single-thread canonical Rust interpreter.
    ReferenceCpu,
    /// Cell-parallel canonical Rust interpreter.
    ParallelCpu,
    /// Resident Slang program on a qualified path.
    SlangCompute,
}

/// Stable address of one compiled node, including nested module calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphExecutionNode {
    /// Fully qualified node address shared with spatial planning and provenance.
    pub address: GraphNodeAddress,
    /// Stable operation.
    pub operator: GraphOperator,
    /// Node authority.
    pub authority: GraphAuthority,
    /// Hash of the complete migrated node document.
    pub node_hash: [u8; 32],
}

/// One exact pin crossing an execution-domain boundary.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphExecutionBoundary {
    /// Fully qualified source or destination pin.
    pub pin: QualifiedGraphPin,
    /// Typed value domain crossing the boundary.
    pub domain: GraphDomain,
}

/// One connected execution group and its explicit transfer boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphExecutionGroup {
    /// Actual execution domain.
    pub domain: GraphExecutionDomain,
    /// Canonical nodes in this connected group.
    pub nodes: Vec<GraphExecutionNode>,
    /// External pins read by this group.
    pub inputs: Vec<GraphExecutionBoundary>,
    /// Pins retained for consumers outside this group.
    pub outputs: Vec<GraphExecutionBoundary>,
    /// Predicted bytes crossing into this group.
    pub transfer_in_bytes: u64,
    /// Predicted retained bytes for the group.
    pub predicted_output_bytes: u64,
}

/// One scheduling plan over the single compiled graph IR.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphExecutionPlan {
    /// Connected domain groups in canonical topological/module order.
    pub groups: Vec<GraphExecutionGroup>,
    /// Total predicted transfer traffic.
    pub predicted_transfer_bytes: u64,
}

impl GraphExecutionPlan {
    /// Returns the selected domain for one fully qualified compiled node.
    #[must_use]
    pub fn domain_for(&self, module_path: &[u128], node: u128) -> Option<GraphExecutionDomain> {
        let address = GraphNodeAddress {
            module_path: module_path.to_vec(),
            node,
        };
        self.groups.iter().find_map(|group| {
            group
                .nodes
                .iter()
                .any(|candidate| candidate.address == address)
                .then_some(group.domain)
        })
    }

    /// Returns the complete connected group containing one node.
    #[must_use]
    pub fn group_for(&self, address: &GraphNodeAddress) -> Option<&GraphExecutionGroup> {
        self.groups
            .iter()
            .find(|group| group.nodes.iter().any(|node| &node.address == address))
    }
}

/// Exact device evidence available to execution-plan selection.
#[derive(Clone, Copy)]
pub struct GraphGpuScheduling<'a> {
    /// Active runtime device profile.
    pub profile: &'a GpuExecutionProfile,
    /// Qualification evidence for authoritative resident nodes.
    pub qualifications: &'a GpuQualificationRegistry,
}

/// Builds connected CPU/resident-GPU groups; incompatible topology remains split.
pub fn build_execution_plan(
    graph: &CompiledBiomeGraph,
    parallel_cpu: bool,
    gpu: Option<GraphGpuScheduling<'_>>,
) -> Result<GraphExecutionPlan> {
    let demand_plan = graph.demand_plan();
    let demand = demand_plan.execution_slice();
    fn compiled_node_count(
        unit: &CompiledGraphUnit,
        demand: &crate::graph::CompiledDemandSlice,
    ) -> Result<usize> {
        let unit_demand = demand
            .unit(
                unit.nodes
                    .first()
                    .map_or(&[][..], |node| node.debug_symbol.module_path.as_slice()),
            )
            .ok_or_else(|| program_error("schedule.demand", "compiled demand slice is missing"))?;
        unit.nodes
            .iter()
            .filter(|node| unit_demand.contains_node(node.definition.guid))
            .try_fold(unit_demand.nodes.len(), |total, node| {
                node.module.as_deref().map_or(Ok(total), |module| {
                    total
                        .checked_add(compiled_node_count(module, demand)?)
                        .ok_or(Error::NumericOverflow)
                })
            })
    }

    let mut groups = Vec::new();
    reserve_exact(
        &mut groups,
        compiled_node_count(&graph.root, demand)?,
        "graph execution groups",
    )?;
    schedule_unit(
        &graph.root,
        demand_plan,
        demand,
        parallel_cpu,
        gpu,
        &mut groups,
    )?;
    let predicted_transfer_bytes = groups.iter().try_fold(0_u64, |total, group| {
        total
            .checked_add(group.transfer_in_bytes)
            .ok_or(Error::NumericOverflow)
    })?;
    Ok(GraphExecutionPlan {
        groups,
        predicted_transfer_bytes,
    })
}

fn schedule_unit(
    unit: &CompiledGraphUnit,
    demand_plan: &crate::graph::CompiledDemandPlan,
    demand: &crate::graph::CompiledDemandSlice,
    parallel_cpu: bool,
    gpu: Option<GraphGpuScheduling<'_>>,
    groups: &mut Vec<GraphExecutionGroup>,
) -> Result<()> {
    let module_path = unit
        .nodes
        .first()
        .map_or(&[][..], |node| node.debug_symbol.module_path.as_slice());
    let unit_demand = demand
        .unit(module_path)
        .ok_or_else(|| program_error("schedule.demand", "compiled demand slice is missing"))?;
    for node in unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
    {
        if let Some(module) = node.module.as_deref() {
            schedule_unit(module, demand_plan, demand, parallel_cpu, gpu, groups)?;
        }
    }
    let nodes = unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
        .map(|node| (node.definition.guid, node))
        .collect::<BTreeMap<_, _>>();
    let gpu_nodes = unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
        .filter(|node| node_gpu_admitted(node, gpu))
        .map(|node| node.definition.guid)
        .collect::<BTreeSet<_>>();
    let mut components = Vec::new();
    reserve_exact(
        &mut components,
        gpu_nodes.len(),
        "GPU scheduling components",
    )?;
    for node in gpu_nodes.iter().copied() {
        let mut component = Vec::new();
        reserve_exact(&mut component, 1, "GPU scheduling component")?;
        component.push(node);
        components.push(component);
    }
    let mut component_by_node = components
        .iter()
        .enumerate()
        .map(|(index, component)| (component[0], index))
        .collect::<BTreeMap<_, _>>();
    loop {
        let mut merged = false;
        for edge in unit
            .edges
            .iter()
            .filter(|edge| unit_demand.contains_edge(edge))
        {
            if gpu_nodes.contains(&edge.from_node)
                && gpu_nodes.contains(&edge.to_node)
                && demand_plan.same_scope_membership(
                    &nodes[&edge.from_node].address(),
                    &nodes[&edge.to_node].address(),
                )
                && program_edge_compatible(nodes[&edge.from_node], nodes[&edge.to_node], edge)
            {
                let source = component_by_node[&edge.from_node];
                let destination = component_by_node[&edge.to_node];
                if source == destination {
                    continue;
                }
                let proposed_capacity = components[source]
                    .len()
                    .checked_add(components[destination].len())
                    .ok_or(Error::NumericOverflow)?;
                let mut proposed = Vec::new();
                reserve_exact(
                    &mut proposed,
                    proposed_capacity,
                    "GPU scheduling proposed component",
                )?;
                proposed.extend(unit.nodes.iter().map(|node| node.definition.guid).filter(
                    |node| {
                        components[source].contains(node) || components[destination].contains(node)
                    },
                ));
                if component_topology_supported(unit, unit_demand, &nodes, &proposed) {
                    components[source] = proposed;
                    components[destination].clear();
                    for node in &components[source] {
                        component_by_node.insert(*node, source);
                    }
                    merged = true;
                }
            }
        }
        if !merged {
            break;
        }
    }
    components.retain(|component| !component.is_empty());
    component_by_node.clear();
    for (index, component) in components.iter().enumerate() {
        for node in component {
            component_by_node.insert(*node, index);
        }
    }

    let mut emitted_components = BTreeSet::new();
    for node in unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
    {
        let guid = node.definition.guid;
        if let Some(component) = component_by_node.get(&guid).copied() {
            if !emitted_components.insert(component) {
                continue;
            }
            groups.push(execution_group(
                unit,
                demand,
                unit_demand,
                &nodes,
                &components[component],
                GraphExecutionDomain::SlangCompute,
            )?);
        } else {
            let domain = if parallel_cpu {
                GraphExecutionDomain::ParallelCpu
            } else {
                GraphExecutionDomain::ReferenceCpu
            };
            let singleton = [guid];
            let group = execution_group(unit, demand, unit_demand, &nodes, &singleton, domain)?;
            if let Some(previous) = groups.last_mut().filter(|previous| {
                previous.domain == domain
                    && previous
                        .outputs
                        .iter()
                        .any(|output| group.inputs.iter().any(|input| input.pin == output.pin))
            }) {
                reserve_exact(
                    &mut previous.nodes,
                    group.nodes.len(),
                    "coalesced CPU execution group nodes",
                )?;
                previous.nodes.extend(group.nodes);
                previous.outputs = group.outputs;
                previous.predicted_output_bytes = previous
                    .predicted_output_bytes
                    .max(group.predicted_output_bytes);
            } else {
                groups.push(group);
            }
        }
    }
    Ok(())
}

fn node_gpu_admitted(node: &CompiledGraphNode, gpu: Option<GraphGpuScheduling<'_>>) -> bool {
    let Some(gpu) = gpu else {
        return false;
    };
    if !node.capabilities.slang_compute || !node_program_compatible(node) {
        return false;
    }
    node.definition.authority == GraphAuthority::Cosmetic
        || (node.definition.authority == GraphAuthority::EquivalentGpu
            && gpu.qualifications.contains(
                node.definition.operator,
                node.definition.version,
                gpu.profile,
            ))
}

fn node_program_compatible(node: &CompiledGraphNode) -> bool {
    if GraphGpuOperator::from_graph(node.definition.operator).is_none() {
        return false;
    }
    if node.definition.operator == GraphOperator::Curve {
        return node
            .definition
            .parameter("curve")
            .and_then(|value| match value {
                crate::GraphParameterValue::Curve(points) => Some(points.len()),
                _ => None,
            })
            .is_some_and(|count| (1..=GRAPH_GPU_MAX_CURVE_POINTS).contains(&count));
    }
    true
}

fn program_edge_compatible(
    source: &CompiledGraphNode,
    destination: &CompiledGraphNode,
    edge: &crate::GraphEdge,
) -> bool {
    if source.definition.spatial != destination.definition.spatial {
        return false;
    }
    let source_domain = source
        .outputs
        .iter()
        .find(|pin| pin.name == edge.from_pin)
        .map(|pin| pin.domain);
    let destination_domain = destination
        .inputs
        .iter()
        .find(|pin| pin.name == edge.to_pin)
        .map(|pin| pin.domain);
    source_domain == destination_domain
        && source_domain.is_some_and(|domain| {
            matches!(domain, GraphDomain::ScalarField | GraphDomain::Candidates)
        })
}

fn component_topology_supported(
    unit: &CompiledGraphUnit,
    demand: &crate::graph::CompiledDemandUnitSlice,
    nodes: &BTreeMap<u128, &CompiledGraphNode>,
    component: &[u128],
) -> bool {
    let is_member = |node| component.contains(&node);
    if component.len() > GRAPH_GPU_MAX_INSTRUCTIONS
        || component
            .iter()
            .filter(|node| nodes[node].definition.operator == GraphOperator::FieldImportance)
            .count()
            > 1
    {
        return false;
    }
    let mut input_lineages = BTreeSet::new();
    let mut external_scalar_inputs = BTreeSet::new();
    let mut external_candidate_inputs = BTreeSet::new();
    let mut synthetic_inputs = 1_usize;
    let mut field_outputs = BTreeSet::new();
    let mut candidate_outputs = BTreeSet::new();
    for node in component {
        synthetic_inputs += match nodes[node].definition.operator {
            GraphOperator::Noise => 11,
            GraphOperator::Gradient => 6,
            _ => 0,
        };
    }
    for edge in unit.edges.iter().filter(|edge| demand.contains_edge(edge)) {
        if !is_member(edge.from_node) && is_member(edge.to_node) {
            let Some(pin) = nodes[&edge.from_node]
                .outputs
                .iter()
                .find(|pin| pin.name == edge.from_pin)
            else {
                return false;
            };
            match pin.domain {
                GraphDomain::ScalarField => {
                    external_scalar_inputs.insert((edge.from_node, edge.from_pin.as_str()));
                }
                GraphDomain::Candidates => {
                    external_candidate_inputs.insert((edge.from_node, edge.from_pin.as_str()));
                }
                _ => return false,
            }
            let Some(lineage) = nodes[&edge.from_node].output_lineage.get(&edge.from_pin) else {
                return false;
            };
            input_lineages.insert(lineage);
        }
        if is_member(edge.from_node) && !is_member(edge.to_node) {
            let Some(pin) = nodes[&edge.from_node]
                .outputs
                .iter()
                .find(|pin| pin.name == edge.from_pin)
            else {
                return false;
            };
            match pin.domain {
                GraphDomain::ScalarField => {
                    field_outputs.insert((edge.from_node, edge.from_pin.as_str()));
                }
                GraphDomain::Candidates => {
                    candidate_outputs.insert((edge.from_node, edge.from_pin.as_str()));
                }
                _ => return false,
            }
        }
    }
    for output in unit
        .outputs
        .iter()
        .filter(|output| demand.outputs.contains(&output.name))
    {
        if is_member(output.node) {
            match output.domain {
                GraphDomain::ScalarField => {
                    field_outputs.insert((output.node, output.pin.as_str()));
                }
                GraphDomain::Candidates => {
                    candidate_outputs.insert((output.node, output.pin.as_str()));
                }
                _ => return false,
            }
        }
    }
    let input_count = synthetic_inputs + external_scalar_inputs.len();
    external_candidate_inputs.len() <= 1
        && input_lineages.len() <= 1
        && field_outputs.len() <= 1
        && candidate_outputs.len() <= 1
        && input_count <= GRAPH_GPU_MAX_INPUTS
        && input_count + component.len() <= GRAPH_GPU_MAX_REGISTERS
}

fn execution_group(
    unit: &CompiledGraphUnit,
    live: &crate::graph::CompiledDemandSlice,
    demand: &crate::graph::CompiledDemandUnitSlice,
    nodes: &BTreeMap<u128, &CompiledGraphNode>,
    members: &[u128],
    domain: GraphExecutionDomain,
) -> Result<GraphExecutionGroup> {
    let is_member = |node| members.contains(&node);
    let mut inputs = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    for edge in unit.edges.iter().filter(|edge| demand.contains_edge(edge)) {
        if !is_member(edge.from_node) && is_member(edge.to_node) {
            let source = nodes[&edge.from_node];
            let pin = source
                .outputs
                .iter()
                .find(|pin| pin.name == edge.from_pin)
                .ok_or_else(|| program_error("schedule.inputs", "source pin is missing"))?;
            inputs.insert(GraphExecutionBoundary {
                pin: QualifiedGraphPin {
                    node: source.address(),
                    pin: edge.from_pin.clone(),
                },
                domain: pin.domain,
            });
        }
        if is_member(edge.from_node) && !is_member(edge.to_node) {
            let source = nodes[&edge.from_node];
            let pin = source
                .outputs
                .iter()
                .find(|pin| pin.name == edge.from_pin)
                .ok_or_else(|| program_error("schedule.outputs", "source pin is missing"))?;
            outputs.insert(GraphExecutionBoundary {
                pin: QualifiedGraphPin {
                    node: source.address(),
                    pin: edge.from_pin.clone(),
                },
                domain: pin.domain,
            });
        }
    }
    for output in unit
        .outputs
        .iter()
        .filter(|output| demand.outputs.contains(&output.name))
    {
        if is_member(output.node) {
            let source = nodes[&output.node];
            outputs.insert(GraphExecutionBoundary {
                pin: QualifiedGraphPin {
                    node: source.address(),
                    pin: output.pin.clone(),
                },
                domain: output.domain,
            });
        }
    }
    let predicted_output_bytes = members
        .iter()
        .map(|node| live.node_estimate(&nodes[node].address()).memory_bytes)
        .max()
        .unwrap_or(0);
    let transfer_in_bytes = if domain == GraphExecutionDomain::SlangCompute {
        predicted_output_bytes
    } else {
        0
    };
    let mut scheduled_nodes = Vec::new();
    reserve_exact(
        &mut scheduled_nodes,
        members.len(),
        "scheduled execution nodes",
    )?;
    for guid in members {
        let node = nodes[guid];
        scheduled_nodes.push(GraphExecutionNode {
            address: node.address(),
            operator: node.definition.operator,
            authority: node.definition.authority,
            node_hash: node.definition_hash,
        });
    }
    let mut scheduled_inputs = Vec::new();
    reserve_exact(
        &mut scheduled_inputs,
        inputs.len(),
        "scheduled execution inputs",
    )?;
    scheduled_inputs.extend(inputs);
    let mut scheduled_outputs = Vec::new();
    reserve_exact(
        &mut scheduled_outputs,
        outputs.len(),
        "scheduled execution outputs",
    )?;
    scheduled_outputs.extend(outputs);
    Ok(GraphExecutionGroup {
        domain,
        nodes: scheduled_nodes,
        inputs: scheduled_inputs,
        outputs: scheduled_outputs,
        transfer_in_bytes,
        predicted_output_bytes,
    })
}

fn validate_instruction(
    instruction: &GraphGpuInstruction,
    registers: &[GraphGpuRegisterType],
) -> Result<()> {
    let require = |register, expected, path| {
        if register_type(registers, register, path)? != expected {
            return Err(program_error(path, "register has the wrong type"));
        }
        Ok(())
    };
    match instruction {
        GraphGpuInstruction::Noise { corners, blend, .. } => {
            for register in corners {
                require(
                    *register,
                    GraphGpuRegisterType::FixedScalar,
                    "noise.corners",
                )?;
            }
            for register in blend {
                require(*register, GraphGpuRegisterType::Unit, "noise.blend")?;
            }
        }
        GraphGpuInstruction::Gradient {
            position,
            exact_origin,
            direction,
            ..
        } => {
            for register in position.iter().chain(exact_origin) {
                require(
                    *register,
                    GraphGpuRegisterType::WorldTick,
                    "gradient.position",
                )?;
            }
            if direction.iter().all(|lane| *lane == 0) {
                return Err(program_error(
                    "gradient.direction",
                    "gradient direction must be non-zero",
                ));
            }
        }
        GraphGpuInstruction::Curve { input, points, .. } => {
            require(*input, GraphGpuRegisterType::FixedScalar, "curve.input")?;
            if points.is_empty()
                || points.len() > GRAPH_GPU_MAX_CURVE_POINTS
                || points.windows(2).any(|pair| pair[0].0 >= pair[1].0)
            {
                return Err(program_error(
                    "curve.points",
                    "curve points are not canonical or bounded",
                ));
            }
        }
        GraphGpuInstruction::Remap {
            input,
            input_min,
            input_max,
            ..
        } => {
            require(*input, GraphGpuRegisterType::FixedScalar, "remap.input")?;
            if input_min >= input_max {
                return Err(program_error("remap.range", "remap input range is empty"));
            }
        }
        GraphGpuInstruction::Combine { left, right, .. } => {
            require(*left, GraphGpuRegisterType::FixedScalar, "combine.left")?;
            require(*right, GraphGpuRegisterType::FixedScalar, "combine.right")?;
        }
        GraphGpuInstruction::Clamp {
            input,
            minimum,
            maximum,
            ..
        } => {
            require(*input, GraphGpuRegisterType::FixedScalar, "clamp.input")?;
            if minimum > maximum {
                return Err(program_error("clamp.range", "clamp range is inverted"));
            }
        }
        GraphGpuInstruction::FieldImportance {
            candidates,
            weights,
            ..
        } => {
            require(
                *candidates,
                GraphGpuRegisterType::CandidateMask,
                "fieldImportance.candidates",
            )?;
            require(
                *weights,
                GraphGpuRegisterType::FixedScalar,
                "fieldImportance.weights",
            )?;
        }
    }
    Ok(())
}

fn register_type(
    registers: &[GraphGpuRegisterType],
    register: GraphGpuRegister,
    path: &'static str,
) -> Result<GraphGpuRegisterType> {
    registers
        .get(register.0 as usize)
        .copied()
        .ok_or_else(|| program_error(path, "register is undefined or forward-referenced"))
}

const fn combine_tag(operation: GraphCombineOperation) -> u32 {
    match operation {
        GraphCombineOperation::Add => 0,
        GraphCombineOperation::Multiply => 1,
        GraphCombineOperation::Minimum => 2,
        GraphCombineOperation::Maximum => 3,
    }
}

fn program_error(path: &'static str, reason: &'static str) -> Error {
    Error::GraphDocument {
        path: format!("graphGpuProgram.{path}"),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn qualification_artifact() -> crate::GpuShaderArtifactIdentity {
        crate::GpuShaderArtifactIdentity {
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
            assert_eq!(
                operator.has_slang_executor(),
                GraphGpuOperator::from_graph(*operator).is_some(),
                "{} capability drift",
                operator.as_wire()
            );
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
    fn register_validation_rejects_forward_types_reassignment_and_bad_bounds() {
        let forward = GraphGpuProgram::new(
            vec![
                GraphGpuRegisterType::FixedScalar,
                GraphGpuRegisterType::CandidateMask,
            ],
            vec![GraphGpuInstruction::Clamp {
                destination: GraphGpuRegister(2),
                input: GraphGpuRegister(3),
                minimum: 0,
                maximum: 1,
            }],
            Some(GraphGpuRegister(2)),
            GraphGpuRegister(1),
        );
        assert!(matches!(forward, Err(Error::GraphDocument { .. })));

        let reassigned = GraphGpuProgram::new(
            vec![
                GraphGpuRegisterType::FixedScalar,
                GraphGpuRegisterType::CandidateMask,
            ],
            vec![GraphGpuInstruction::Clamp {
                destination: GraphGpuRegister(0),
                input: GraphGpuRegister(0),
                minimum: 0,
                maximum: 1,
            }],
            Some(GraphGpuRegister(0)),
            GraphGpuRegister(1),
        );
        assert!(matches!(reassigned, Err(Error::GraphDocument { .. })));

        let inverted = GraphGpuProgram::new(
            vec![
                GraphGpuRegisterType::FixedScalar,
                GraphGpuRegisterType::CandidateMask,
            ],
            vec![GraphGpuInstruction::Remap {
                destination: GraphGpuRegister(2),
                input: GraphGpuRegister(0),
                input_min: 1,
                input_max: 1,
                output_min: 0,
                output_max: 1,
            }],
            Some(GraphGpuRegister(2)),
            GraphGpuRegister(1),
        );
        assert!(matches!(inverted, Err(Error::GraphDocument { .. })));
    }

    #[test]
    fn flat_invocation_batch_enforces_one_schema_capacity_and_encoding() {
        let corpus = qualification_corpus();
        let program = &corpus[3].program;
        let mut batch = GraphGpuInvocationBatch::with_capacity(program, 2).unwrap();
        assert_eq!(batch.input_stride(), 3);
        assert_eq!(batch.invocation_count(), 0);
        assert_eq!(batch.invocation_capacity(), 2);
        assert_eq!(batch.encoded_word_count(), 0);
        assert_eq!(batch.encoded_byte_count().unwrap(), 0);
        assert!(
            batch
                .push(
                    [
                        GraphGpuValue::FixedScalar(1),
                        GraphGpuValue::CandidateMask(true),
                        GraphGpuValue::CandidateMask(true),
                    ]
                    .map(Ok)
                )
                .is_err()
        );
        assert!(
            batch
                .push([
                    Ok(GraphGpuValue::FixedScalar(1)),
                    Ok(GraphGpuValue::FixedScalar(2)),
                ])
                .is_err()
        );
        assert_eq!(batch.invocation_count(), 0);
        for left in [1, 2] {
            batch
                .push(
                    [
                        GraphGpuValue::FixedScalar(left),
                        GraphGpuValue::FixedScalar(3),
                        GraphGpuValue::CandidateMask(true),
                    ]
                    .map(Ok),
                )
                .unwrap();
        }
        assert!(
            batch
                .push(
                    [
                        GraphGpuValue::FixedScalar(4),
                        GraphGpuValue::FixedScalar(5),
                        GraphGpuValue::CandidateMask(true),
                    ]
                    .map(Ok)
                )
                .is_err()
        );
        assert_eq!(batch.invocation_count(), 2);
        assert_eq!(batch.invocations().len(), 2);
        assert_eq!(batch.flat_inputs().len(), 6);
        assert_eq!(batch.encoded_word_count(), 24);
        assert_eq!(batch.encoded_byte_count().unwrap(), 96);
        assert!(batch.validate_program(&corpus[0].program).is_err());
        let mut failed_batch = GraphGpuInvocationBatch::with_capacity(program, 1).unwrap();
        assert!(matches!(
            failed_batch.push(std::iter::once(Err(Error::NumericOverflow))),
            Err(Error::NumericOverflow)
        ));
        assert_eq!(failed_batch.invocation_count(), 0);
    }

    #[test]
    fn gpu_program_and_batch_report_checked_retained_capacity_bytes() {
        let corpus = qualification_corpus();
        let program = &corpus[0].program;
        let mut expected_program_bytes =
            requested_vec_bytes::<GraphGpuRegisterType>(program.input_types.capacity())
                .unwrap()
                .checked_add(
                    requested_vec_bytes::<GraphGpuInstruction>(program.instructions.capacity())
                        .unwrap(),
                )
                .and_then(|bytes| {
                    bytes.checked_add(
                        requested_vec_bytes::<GraphGpuRegisterType>(
                            program.register_types.capacity(),
                        )
                        .unwrap(),
                    )
                })
                .unwrap();
        for instruction in &program.instructions {
            if let GraphGpuInstruction::Curve { points, .. } = instruction {
                expected_program_bytes = expected_program_bytes
                    .checked_add(requested_vec_bytes::<(u16, i32)>(points.capacity()).unwrap())
                    .unwrap();
            }
        }
        assert_eq!(
            program.requested_memory_bytes().unwrap(),
            expected_program_bytes
        );
        assert_eq!(
            program.encoded_byte_count().unwrap(),
            program.encoded_word_count() * size_of::<u32>()
        );
        let requested =
            GraphGpuInvocationBatch::requested_memory_bytes_for_capacity(program, 3).unwrap();
        let batch = GraphGpuInvocationBatch::with_capacity(program, 3).unwrap();
        assert!(batch.requested_memory_bytes().unwrap() >= requested);
        assert_eq!(
            batch.requested_memory_bytes().unwrap(),
            requested_vec_bytes::<GraphGpuValue>(batch.value_capacity()).unwrap()
        );
        assert!(matches!(
            GraphGpuInvocationBatch::requested_memory_bytes_for_capacity(program, usize::MAX),
            Err(Error::NumericOverflow)
        ));
    }

    #[test]
    fn branching_and_terminal_masks_preserve_exact_semantics() {
        let corpus = qualification_corpus();
        let batch = &corpus[1];
        let outputs =
            evaluate_gpu_program_reference(&batch.program, &batch.invocation_batch).unwrap();
        assert!(outputs[0].valid);
        assert!(outputs[1].valid);
        assert!(!outputs[2].candidate_mask);
        let overflow =
            evaluate_gpu_program_reference(&corpus[3].program, &corpus[3].invocation_batch)
                .unwrap();
        assert!(
            !overflow[1].valid,
            "overflow invalidates the complete invocation"
        );
        let gradient =
            evaluate_gpu_program_reference(&corpus[2].program, &corpus[2].invocation_batch)
                .unwrap();
        assert!(gradient[..4].iter().all(|output| output.valid));
        assert_eq!(gradient[3].value as i32, 1_024);
        assert!(!gradient[4].valid);
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
