//! Resident-program ABI: typed registers, SSA instructions, and validated batches.

use std::mem::size_of;

use crate::memory::{requested_vec_bytes, reserve_exact};
use crate::{Error, GraphCombineOperation, GraphOperator, Result};

use super::*;

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
        input: GraphGpuRegister,
        input_min: i32,
        input_max: i32,
        output_min: i32,
        output_max: i32,
    },
    /// Binary scalar combination, including branch convergence.
    Combine {
        /// New scalar register.
        destination: GraphGpuRegister,
        left: GraphGpuRegister,
        right: GraphGpuRegister,
        operation: GraphCombineOperation,
    },
    /// Exact scalar clamp.
    Clamp {
        /// New scalar register.
        destination: GraphGpuRegister,
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
    pub(super) input_types: Vec<GraphGpuRegisterType>,
    pub(super) instructions: Vec<GraphGpuInstruction>,
    pub(super) output: Option<GraphGpuRegister>,
    pub(super) candidate_mask: GraphGpuRegister,
    pub(super) register_types: Vec<GraphGpuRegisterType>,
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

    pub(super) fn for_each_encoded_word(&self, mut append: impl FnMut(u32)) {
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

pub(super) fn program_error(path: &'static str, reason: &'static str) -> Error {
    Error::GraphDocument {
        path: format!("graphGpuProgram.{path}"),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::qualification::qualification_corpus;
    use super::*;

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
}
