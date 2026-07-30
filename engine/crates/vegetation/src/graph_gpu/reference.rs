//! Canonical Rust execution of one resident program.

use saffron_spatial::{
    DecisionCurve, DecisionScalar, LOCAL_TICKS_PER_METER, UnitInterval, div_round_ties_even,
};

use crate::{Error, GraphCombineOperation, Result};

use super::*;

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

#[cfg(test)]
mod tests {
    use super::super::qualification::qualification_corpus;
    use super::*;

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
}
