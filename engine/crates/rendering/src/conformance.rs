//! Generic Vulkan profile, shader-artifact, and spatial-numeric conformance evidence.

use std::mem::size_of;
use std::sync::Arc;

use saffron_spatial::{
    DecisionCurve, DecisionScalar, PHILOX4X32_ZERO_VECTOR, RandomDomain, RandomStream,
    UnitInterval, WorldCellKey, div_round_ties_even,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    ComputeBuffer, ComputeDispatch, ComputeDispatchOutcome, Device, Error, Result,
    ShaderArtifactContract, ShaderArtifactIdentity, VulkanDeviceIdentity,
};

const SPATIAL_GOLDEN_WORDS: usize = 32;
const SPATIAL_NUMERIC_ARTIFACT: ShaderArtifactContract = ShaderArtifactContract::new(
    "spatial_numeric_test",
    "spatial_numeric_test.slang",
    "spatial_numeric_test.spv",
    &["spatial_numeric.slang", "spatial_numeric_test.slang"],
    &[],
);

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
    /// SHA-256 of flags, definitions, source names, and source bytes.
    pub compile_input_sha256: String,
    /// SHA-256 of the exact loaded SPIR-V bytes.
    pub spirv_sha256: String,
    /// SHA-256 identity of the complete canonical manifest record.
    pub record_sha256: String,
}

/// Shared RNG and fixed-numeric Rust/Slang golden evidence.
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

/// Captures the generic spatial-numeric Rust/Slang evidence.
pub fn capture_spatial_numeric(device: Arc<Device>) -> Result<SpatialNumericEvidence> {
    let mut dispatcher = ComputeDispatch::new_verified(device, SPATIAL_NUMERIC_ARTIFACT, 1)?;
    let artifact = shader_artifact_evidence(dispatcher.shader_artifact_identity());
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

/// Converts a verified shader identity into serializable evidence.
#[must_use]
pub fn shader_artifact_evidence(identity: &ShaderArtifactIdentity) -> ShaderArtifactEvidence {
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

/// Converts the renderer's Vulkan identity into serializable profile evidence.
#[must_use]
pub fn vulkan_profile_evidence(identity: &VulkanDeviceIdentity) -> VulkanProfileEvidence {
    VulkanProfileEvidence {
        name: identity.name.clone(),
        device_type: identity.device_type_name().to_owned(),
        vendor_id: identity.vendor_id,
        device_id: identity.device_id,
        driver_version: identity.driver_version,
        api_version: identity.api_version,
        driver_id: identity.driver_id,
        device_uuid: hex_bytes(&identity.device_uuid),
        driver_uuid: hex_bytes(&identity.driver_uuid),
        molten_vk: identity.is_molten_vk(),
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
    fn evidence_hashes_are_lowercase_hexadecimal() {
        assert_eq!(hex_bytes(&[0x00, 0x7f, 0xff]), "007fff");
        assert_eq!(hash_words(&[0x0102_0304]).len(), 64);
    }
}
