//! CPU reference for the energy-conserving thin-sheet optical partition.

/// Reflection and transmission budgets for light incident on one side of a sheet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThinSheetEnergyPartition {
    /// Hemispherical reflection budget.
    pub reflection: f32,
    /// Per-channel transmission budget after Beer-Lambert absorption.
    pub transmission: [f32; 3],
}

/// Partitions incident energy between reflection and transmission for one sheet face.
#[must_use]
pub fn thin_sheet_energy_partition(
    face_response: f32,
    thickness: f32,
    absorption: [f32; 3],
    transmission_tint: [f32; 3],
    energy_limit: f32,
    cosine: f32,
) -> ThinSheetEnergyPartition {
    let energy_limit = energy_limit.clamp(0.0, 1.0);
    let reflection = face_response.clamp(0.0, energy_limit);
    let remaining = energy_limit - reflection;
    let optical_depth = thickness.max(0.0) / cosine.abs().max(0.08);
    let transmission = std::array::from_fn(|channel| {
        let beer = (-absorption[channel].max(0.0) * optical_depth).exp();
        remaining * transmission_tint[channel].clamp(0.0, 1.0) * beer
    });
    ThinSheetEnergyPartition {
        reflection,
        transmission,
    }
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;
    use std::sync::Arc;

    use super::*;
    use crate::compute_dispatch::{ComputeBuffer, run_compute};
    use crate::{Device, SurfaceSource, validation_issue_count};

    #[test]
    fn every_channel_respects_the_authored_energy_limit() {
        for response in [0.0, 0.2, 0.8, 1.0] {
            for cosine in [0.01, 0.2, 0.5, 1.0] {
                let partition = thin_sheet_energy_partition(
                    response,
                    0.003,
                    [0.1, 0.5, 2.0],
                    [1.0, 0.8, 0.6],
                    0.85,
                    cosine,
                );
                for transmission in partition.transmission {
                    assert!(partition.reflection + transmission <= 0.85 + f32::EPSILON);
                }
            }
        }
    }

    #[test]
    fn thickness_and_grazing_incidence_increase_absorption() {
        let normal = thin_sheet_energy_partition(0.2, 0.001, [2.0; 3], [1.0; 3], 1.0, 1.0);
        let thick = thin_sheet_energy_partition(0.2, 0.01, [2.0; 3], [1.0; 3], 1.0, 1.0);
        let grazing = thin_sheet_energy_partition(0.2, 0.001, [2.0; 3], [1.0; 3], 1.0, 0.08);
        assert!(thick.transmission[0] < normal.transmission[0]);
        assert!(grazing.transmission[0] < normal.transmission[0]);
    }

    #[test]
    fn front_and_back_responses_produce_distinct_partitions() {
        let front = thin_sheet_energy_partition(0.7, 0.002, [0.5; 3], [1.0; 3], 1.0, 0.5);
        let back = thin_sheet_energy_partition(0.2, 0.002, [0.5; 3], [1.0; 3], 1.0, 0.5);
        assert_ne!(front.reflection, back.reflection);
        assert_ne!(front.transmission, back.transmission);
    }

    #[test]
    fn rust_and_slang_optical_partitions_match_on_gpu() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => Arc::new(device),
            Err(error) => {
                eprintln!("skipping: no Vulkan device obtainable ({error})");
                return;
            }
        };
        let front =
            thin_sheet_energy_partition(0.625, 0.003, [0.5, 1.0, 2.0], [1.0, 0.75, 0.5], 0.9, 0.25);
        let back =
            thin_sheet_energy_partition(0.25, 0.003, [0.5, 1.0, 2.0], [1.0, 0.75, 0.5], 0.9, 0.25);
        let expected = [
            front.reflection,
            front.transmission[0],
            front.transmission[1],
            front.transmission[2],
            back.reflection,
            back.transmission[0],
            back.transmission[1],
            back.transmission[2],
        ];
        let before = validation_issue_count();
        let buffers = run_compute(
            Arc::clone(&device),
            "thin_sheet_test",
            vec![ComputeBuffer::zeroed(expected.len() * size_of::<f32>())],
            [1, 1, 1],
        )
        .expect("thin-sheet fixture GPU dispatch");
        let actual = buffers[0]
            .chunks_exact(size_of::<f32>())
            .map(|bytes| f32::from_bits(u32::from_le_bytes(bytes.try_into().unwrap())))
            .collect::<Vec<_>>();
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 2.0e-6,
                "{actual} != {expected}"
            );
        }
        device.wait_idle().expect("idle before teardown");
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }
}
