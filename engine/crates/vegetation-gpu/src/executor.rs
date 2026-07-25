//! Qualified Vulkan executor for the resident vegetation graph-program ABI.

use std::mem::size_of;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use saffron_vegetation::{
    GRAPH_GPU_INVOCATION_WORDS, GRAPH_GPU_OUTPUT_WORDS, GpuExecutionProfile,
    GpuQualificationRegistry, GpuShaderArtifactIdentity, GraphCancellationToken,
    GraphComputeExecutor, GraphGpuInvocationBatch, GraphGpuOutput, GraphGpuProgram,
    GraphGpuRegisterType,
};

use saffron_rendering::{
    ComputeBuffer, ComputeDispatch, ComputeDispatchAbort, ComputeDispatchLimits,
    ComputeDispatchOutcome, Device, ShaderArtifactContract, ShaderArtifactIdentity,
    compute_dispatch_limits,
};

const GRAPH_GPU_WORKGROUP_SIZE: u64 = 64;
const MAX_INVOCATIONS_PER_DISPATCH: u64 = 65_536;
const QUALIFICATION_TIMEOUT: Duration = Duration::from_secs(30);
const DISPATCH_LOCK_POLL: Duration = Duration::from_millis(1);

const VEGETATION_GRAPH_ARTIFACT: ShaderArtifactContract = ShaderArtifactContract::new(
    "vegetation_graph",
    "vegetation_graph.slang",
    "vegetation_graph.spv",
    &["spatial_numeric.slang", "vegetation_graph.slang"],
    &[],
);

/// Thread-safe Vulkan executor qualified against its exact device and shader artifact.
pub struct VulkanGraphComputeExecutor {
    dispatcher: Mutex<ComputeDispatch>,
    profile: GpuExecutionProfile,
    qualifications: GpuQualificationRegistry,
    shader_artifact_identity: ShaderArtifactIdentity,
    dispatch_limits: ComputeDispatchLimits,
}

impl VulkanGraphComputeExecutor {
    /// Creates an executor and requires every canonical resident program to match Rust.
    pub fn new(device: Arc<Device>) -> crate::Result<Self> {
        let identity = device.device_identity();
        if !identity.is_physical_gpu() {
            return Err(crate::Error::NonPhysicalDevice {
                device_type: identity.device_type_name().to_owned(),
            });
        }
        let molten_vk = identity.is_molten_vk();
        let profile = GpuExecutionProfile {
            name: identity.name,
            vendor_id: identity.vendor_id,
            device_id: identity.device_id,
            driver_version: identity.driver_version,
            api_version: identity.api_version,
            driver_id: identity.driver_id,
            device_uuid: identity.device_uuid,
            driver_uuid: identity.driver_uuid,
            molten_vk,
        };
        let dispatch_limits = compute_dispatch_limits(&device);
        let mut dispatcher =
            ComputeDispatch::new_verified(Arc::clone(&device), VEGETATION_GRAPH_ARTIFACT, 3)?;
        let shader_artifact_identity = dispatcher.shader_artifact_identity().clone();
        let qualification_artifact = GpuShaderArtifactIdentity {
            record_hash: shader_artifact_identity.record_sha256().bytes(),
            compile_input_hash: shader_artifact_identity.compile_input_sha256().bytes(),
            spirv_hash: shader_artifact_identity.spirv_sha256().bytes(),
            compiler_identity_hash: shader_artifact_identity.compiler_identity_sha256().bytes(),
        };
        let qualification_deadline = Instant::now()
            .checked_add(QUALIFICATION_TIMEOUT)
            .ok_or(crate::Error::QualificationDeadlineOverflow)?;
        let qualification_cancellation = GraphCancellationToken::default();
        let qualifications = GpuQualificationRegistry::qualify(
            profile.clone(),
            qualification_artifact,
            |program, invocations| {
                invocations.validate_program(program)?;
                execute_program(
                    &mut dispatcher,
                    &profile.name,
                    program,
                    invocations,
                    dispatch_limits,
                    &qualification_cancellation,
                    qualification_deadline,
                )
            },
        )
        .map_err(crate::Error::Vegetation)?;
        Ok(Self {
            dispatcher: Mutex::new(dispatcher),
            profile,
            qualifications,
            shader_artifact_identity,
            dispatch_limits,
        })
    }

    /// Exact compiler, source-closure, and SPIR-V identity qualified by this executor.
    #[must_use]
    pub fn shader_artifact_identity(&self) -> &ShaderArtifactIdentity {
        &self.shader_artifact_identity
    }

    fn lock_dispatcher(
        &self,
        cancellation: &GraphCancellationToken,
        deadline: Instant,
    ) -> saffron_vegetation::Result<MutexGuard<'_, ComputeDispatch>> {
        loop {
            check_abort(cancellation, deadline)?;
            match self.dispatcher.try_lock() {
                Ok(dispatcher) => return Ok(dispatcher),
                Err(TryLockError::WouldBlock) => std::thread::park_timeout(DISPATCH_LOCK_POLL),
                Err(TryLockError::Poisoned(_)) => {
                    return Err(execution_error(
                        &self.profile.name,
                        "compute dispatcher lock is poisoned",
                    ));
                }
            }
        }
    }
}

impl GraphComputeExecutor for VulkanGraphComputeExecutor {
    fn profile(&self) -> &GpuExecutionProfile {
        &self.profile
    }

    fn qualifications(&self) -> &GpuQualificationRegistry {
        &self.qualifications
    }

    fn execute_program(
        &self,
        program: &GraphGpuProgram,
        invocation_batch: &GraphGpuInvocationBatch,
        cancellation: &GraphCancellationToken,
        deadline: Instant,
    ) -> saffron_vegetation::Result<Vec<GraphGpuOutput>> {
        check_abort(cancellation, deadline)?;
        invocation_batch.validate_program(program)?;
        if invocation_batch.invocation_count() == 0 {
            return Ok(Vec::new());
        }
        let mut dispatcher = self.lock_dispatcher(cancellation, deadline)?;
        execute_program(
            &mut dispatcher,
            &self.profile.name,
            program,
            invocation_batch,
            self.dispatch_limits,
            cancellation,
            deadline,
        )
    }
}

fn execute_program(
    dispatcher: &mut ComputeDispatch,
    profile: &str,
    program: &GraphGpuProgram,
    invocation_batch: &GraphGpuInvocationBatch,
    limits: ComputeDispatchLimits,
    cancellation: &GraphCancellationToken,
    deadline: Instant,
) -> saffron_vegetation::Result<Vec<GraphGpuOutput>> {
    if invocation_batch.invocation_count() == 0 {
        return Ok(Vec::new());
    }
    let program_bytes = words_to_le_bytes(profile, &program.encoded_words()?)?;
    let max_invocations = dispatch_invocation_limit(
        limits,
        program_bytes.len(),
        program.input_types().len(),
    )
    .ok_or_else(|| {
        execution_error(
            profile,
            "Vulkan limits cannot accommodate the resident graph program and one invocation",
        )
    })?;
    let mut outputs = Vec::new();
    outputs
        .try_reserve_exact(invocation_batch.invocation_count())
        .map_err(|error| {
            execution_error(profile, format!("cannot reserve GPU results: {error}"))
        })?;
    let values_per_chunk = max_invocations
        .checked_mul(invocation_batch.input_stride())
        .ok_or(saffron_vegetation::Error::NumericOverflow)?;
    for chunk in invocation_batch.flat_inputs().chunks(values_per_chunk) {
        check_abort(cancellation, deadline)?;
        outputs.extend(execute_program_chunk(
            dispatcher,
            profile,
            program,
            &program_bytes,
            chunk,
            cancellation,
            deadline,
        )?);
    }
    Ok(outputs)
}

fn execute_program_chunk(
    dispatcher: &mut ComputeDispatch,
    profile: &str,
    program: &GraphGpuProgram,
    program_bytes: &[u8],
    invocation_inputs: &[saffron_vegetation::GraphGpuValue],
    cancellation: &GraphCancellationToken,
    deadline: Instant,
) -> saffron_vegetation::Result<Vec<GraphGpuOutput>> {
    let words_per_invocation = program
        .input_types()
        .len()
        .checked_mul(GRAPH_GPU_INVOCATION_WORDS)
        .ok_or(saffron_vegetation::Error::NumericOverflow)?;
    if !invocation_inputs
        .len()
        .is_multiple_of(program.input_types().len())
    {
        return Err(execution_error(
            profile,
            "flat invocation chunk does not match the resident program stride",
        ));
    }
    let invocation_count = invocation_inputs.len() / program.input_types().len();
    let invocation_byte_count = invocation_count
        .checked_mul(words_per_invocation)
        .and_then(|words| words.checked_mul(size_of::<u32>()))
        .ok_or(saffron_vegetation::Error::NumericOverflow)?;
    let mut invocation_bytes = Vec::new();
    invocation_bytes
        .try_reserve_exact(invocation_byte_count)
        .map_err(|error| {
            execution_error(profile, format!("cannot reserve GPU invocations: {error}"))
        })?;
    for input in invocation_inputs {
        for word in input.words() {
            invocation_bytes.extend_from_slice(&word.to_le_bytes());
        }
    }
    if invocation_bytes.len() != invocation_byte_count {
        return Err(execution_error(
            profile,
            "packed invocation byte length does not match the resident program signature",
        ));
    }
    let output_byte_count = invocation_count
        .checked_mul(GRAPH_GPU_OUTPUT_WORDS)
        .and_then(|words| words.checked_mul(size_of::<u32>()))
        .ok_or(saffron_vegetation::Error::NumericOverflow)?;
    let invocation_count_u32 = u32::try_from(invocation_count)
        .map_err(|_| execution_error(profile, "GPU invocation chunk exceeds Vulkan dimensions"))?;
    let workgroups = invocation_count_u32.div_ceil(GRAPH_GPU_WORKGROUP_SIZE as u32);
    let outcome = dispatcher
        .run_interruptible(
            vec![
                ComputeBuffer::from_bytes(program_bytes.to_vec()),
                ComputeBuffer::from_bytes(invocation_bytes),
                ComputeBuffer::zeroed(output_byte_count),
            ],
            [workgroups, 1, 1],
            || abort_reason(cancellation, deadline),
        )
        .map_err(|error| execution_error(profile, error.to_string()))?;
    let buffers = match outcome {
        ComputeDispatchOutcome::Complete(buffers) => buffers,
        ComputeDispatchOutcome::Aborted(reason) => return Err(abort_error(reason, deadline)),
    };
    let output_bytes = buffers
        .get(2)
        .ok_or_else(|| execution_error(profile, "compute executor omitted its output buffer"))?;
    decode_outputs(profile, program, output_bytes, invocation_count)
}

fn decode_outputs(
    profile: &str,
    program: &GraphGpuProgram,
    bytes: &[u8],
    invocation_count: usize,
) -> saffron_vegetation::Result<Vec<GraphGpuOutput>> {
    let expected_bytes = invocation_count
        .checked_mul(GRAPH_GPU_OUTPUT_WORDS)
        .and_then(|words| words.checked_mul(size_of::<u32>()))
        .ok_or(saffron_vegetation::Error::NumericOverflow)?;
    if bytes.len() != expected_bytes {
        return Err(execution_error(
            profile,
            "compute output has the wrong byte length",
        ));
    }
    let expected_type = program.output_type();
    bytes
        .chunks_exact(GRAPH_GPU_OUTPUT_WORDS * size_of::<u32>())
        .enumerate()
        .map(|(index, record)| {
            let mut words = [0_u32; GRAPH_GPU_OUTPUT_WORDS];
            for (word, bytes) in words.iter_mut().zip(record.chunks_exact(size_of::<u32>())) {
                *word =
                    u32::from_le_bytes(bytes.try_into().map_err(|_| {
                        execution_error(profile, "compute output word is truncated")
                    })?);
            }
            decode_output(expected_type, words).map_err(|error| {
                execution_error(profile, format!("output {index} is not canonical: {error}"))
            })
        })
        .collect()
}

fn decode_output(
    expected_type: Option<GraphGpuRegisterType>,
    words: [u32; GRAPH_GPU_OUTPUT_WORDS],
) -> std::result::Result<GraphGpuOutput, &'static str> {
    let valid = decode_bool(words[0]).ok_or("valid flag is not Boolean")?;
    let candidate_mask = decode_bool(words[1]).ok_or("candidate mask is not Boolean")?;
    let value_type = decode_register_type(words[2]).ok_or("value type is unknown")?;
    if value_type != expected_type {
        return Err("value type differs from the resident program");
    }
    let value = words[3];
    if !valid && (candidate_mask || value != 0) {
        return Err("invalid result contains live output data");
    }
    match value_type {
        None if value != 0 => return Err("valueless program returned a value"),
        Some(GraphGpuRegisterType::Unit) if value > u32::from(u16::MAX) => {
            return Err("unit output is outside its canonical representation");
        }
        Some(GraphGpuRegisterType::CandidateMask) => {
            return Err("candidate masks cannot use the value output");
        }
        _ => {}
    }
    Ok(GraphGpuOutput {
        valid,
        candidate_mask,
        value_type,
        value,
    })
}

const fn decode_bool(word: u32) -> Option<bool> {
    match word {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

const fn decode_register_type(word: u32) -> Option<Option<GraphGpuRegisterType>> {
    Some(match word {
        0 => None,
        1 => Some(GraphGpuRegisterType::FixedScalar),
        2 => Some(GraphGpuRegisterType::Unit),
        3 => Some(GraphGpuRegisterType::CandidateMask),
        4 => Some(GraphGpuRegisterType::WorldTick),
        _ => return None,
    })
}

fn words_to_le_bytes(profile: &str, words: &[u32]) -> saffron_vegetation::Result<Vec<u8>> {
    let byte_count = words
        .len()
        .checked_mul(size_of::<u32>())
        .ok_or(saffron_vegetation::Error::NumericOverflow)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(byte_count).map_err(|error| {
        execution_error(
            profile,
            format!("cannot reserve resident program bytes: {error}"),
        )
    })?;
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    Ok(bytes)
}

fn dispatch_invocation_limit(
    limits: ComputeDispatchLimits,
    program_bytes: usize,
    input_count: usize,
) -> Option<usize> {
    let program_bytes = u64::try_from(program_bytes).ok()?;
    if program_bytes == 0 || program_bytes > limits.max_storage_buffer_range {
        return None;
    }
    let invocation_bytes = u64::try_from(
        input_count
            .checked_mul(GRAPH_GPU_INVOCATION_WORDS)?
            .checked_mul(4)?,
    )
    .ok()?;
    let output_bytes = u64::try_from(GRAPH_GPU_OUTPUT_WORDS.checked_mul(4)?).ok()?;
    let storage_invocations = (limits.max_storage_buffer_range / invocation_bytes)
        .min(limits.max_storage_buffer_range / output_bytes);
    let dispatch_invocations =
        u64::from(limits.max_work_group_count_x).checked_mul(GRAPH_GPU_WORKGROUP_SIZE)?;
    usize::try_from(
        storage_invocations
            .min(dispatch_invocations)
            .min(MAX_INVOCATIONS_PER_DISPATCH),
    )
    .ok()
    .filter(|limit| *limit != 0)
}

fn execution_error(profile: &str, reason: impl Into<String>) -> saffron_vegetation::Error {
    saffron_vegetation::Error::GraphGpuExecution {
        profile: profile.to_owned(),
        reason: reason.into(),
    }
}

fn abort_reason(
    cancellation: &GraphCancellationToken,
    deadline: Instant,
) -> Option<ComputeDispatchAbort> {
    if cancellation.is_cancelled() {
        Some(ComputeDispatchAbort::Cancelled)
    } else if Instant::now() >= deadline {
        Some(ComputeDispatchAbort::DeadlineExceeded)
    } else {
        None
    }
}

fn check_abort(
    cancellation: &GraphCancellationToken,
    deadline: Instant,
) -> saffron_vegetation::Result<()> {
    abort_reason(cancellation, deadline).map_or(Ok(()), |reason| Err(abort_error(reason, deadline)))
}

fn abort_error(reason: ComputeDispatchAbort, deadline: Instant) -> saffron_vegetation::Error {
    match reason {
        ComputeDispatchAbort::Cancelled => saffron_vegetation::Error::GraphCancelled,
        ComputeDispatchAbort::DeadlineExceeded => {
            let overrun = Instant::now()
                .saturating_duration_since(deadline)
                .as_millis();
            saffron_vegetation::Error::GraphLimit {
                resource: "time milliseconds",
                requested: u64::try_from(overrun).unwrap_or(u64::MAX).max(1),
                limit: 0,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_limit_obeys_program_storage_invocation_storage_and_grid_bounds() {
        let limits = ComputeDispatchLimits {
            max_storage_buffer_range: 1_024,
            max_work_group_count_x: u32::MAX,
        };
        assert_eq!(dispatch_invocation_limit(limits, 1_025, 2), None);
        assert_eq!(dispatch_invocation_limit(limits, 128, 2), Some(32));
        assert_eq!(
            dispatch_invocation_limit(
                ComputeDispatchLimits {
                    max_storage_buffer_range: u64::MAX,
                    max_work_group_count_x: 1,
                },
                128,
                2,
            ),
            Some(64)
        );
        assert_eq!(
            dispatch_invocation_limit(
                ComputeDispatchLimits {
                    max_storage_buffer_range: u64::MAX,
                    max_work_group_count_x: u32::MAX,
                },
                128,
                2,
            ),
            Some(MAX_INVOCATIONS_PER_DISPATCH as usize)
        );
    }

    #[test]
    fn output_decoder_rejects_noncanonical_tags_ranges_and_invalid_data() {
        assert!(decode_output(None, [1, 1, 0, 0]).is_ok());
        assert!(decode_output(None, [2, 0, 0, 0]).is_err());
        assert!(decode_output(None, [1, 0, 9, 0]).is_err());
        assert!(
            decode_output(
                Some(GraphGpuRegisterType::Unit),
                [1, 1, 2, u32::from(u16::MAX) + 1]
            )
            .is_err()
        );
        assert!(decode_output(Some(GraphGpuRegisterType::FixedScalar), [0, 0, 1, 1]).is_err());
        assert!(decode_output(Some(GraphGpuRegisterType::WorldTick), [1, 1, 4, u32::MAX]).is_ok());
    }
}
