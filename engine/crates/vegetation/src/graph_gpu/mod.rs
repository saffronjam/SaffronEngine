//! Resident graph-program ABI, Rust reference execution, qualification, and scheduling.

mod program;
mod qualification;
mod reference;
mod schedule;

pub use program::*;
pub use qualification::*;
pub use reference::*;
pub use schedule::*;

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
