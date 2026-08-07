use super::SCENE_EXECUTOR_BUCKET_CAPACITY;

/// One CPU-enumerated draw bucket: the dense identity the kernels scatter into and the
/// draw site selects a PSO for.
#[derive(Clone, Copy, Debug)]
pub struct ExecutorBucket {
    /// Registered executor shader index (0 = the übershader).
    pub shader_index: u32,
    /// The bucket's full psoBin (representation + material class + deformation).
    pub pso_bin: u32,
    /// First command slot of the bucket's slice.
    pub base: u32,
    /// Slice capacity in commands.
    pub capacity: u32,
}

/// The executor [`crate::Material`] a draw bucket's identity decodes to (the PSO
/// request key for the bucket's draws).
#[must_use]
pub fn bucket_material(
    shaders: &crate::ExecutorShaderRegistry,
    bucket: ExecutorBucket,
) -> crate::Material {
    let class = crate::GpuMaterialClass::from_bits(
        (bucket.pso_bin >> crate::GPU_PSO_MATERIAL_SHIFT) & 0x3f,
    )
    .unwrap_or_default();
    crate::Material {
        shader: shaders.get(bucket.shader_index).to_owned(),
        unlit: class.unlit(),
        blend: class.transparency() == crate::GpuTransparency::AlphaBlended,
        masked: class.coverage() == saffron_material::AlphaClassification::Masked,
    }
}

/// Builds the frame's dense bucket set from the live (shader, material-class) pairs:
/// each pair expands over the representations the frame can produce (rigid deformation),
/// buckets sort by key, and the command buffer partitions into equal slices. Returns the
/// buckets plus the GPU table bytes (`SceneBucketTable` in `scene_bin_common.slang`).
///
/// `displaced` adds the displacement-arena representation. It costs every bucket a third
/// of its command slice, so it expands only on a frame that amplifies something — a
/// record whose bucket is absent raises the pressure flag rather than drawing wrong, and
/// a bucket with no records draws nothing.
#[must_use]
pub fn build_executor_buckets(
    live: &[(u32, u32)],
    record_capacity: u32,
    displaced: bool,
) -> (Vec<ExecutorBucket>, Vec<u8>) {
    let mut representations = vec![
        crate::GpuRepresentation::TriangleCluster as u32,
        crate::GpuRepresentation::AggregateVoxel as u32,
    ];
    if displaced {
        representations.push(crate::GpuRepresentation::DisplacedMicro as u32);
    }
    let mut keyed: Vec<(u32, u32, u32)> = Vec::new();
    for (shader_index, class_bits) in live {
        for representation in representations.iter().copied() {
            let pso_bin = (representation << crate::GPU_PSO_REPRESENTATION_SHIFT)
                | (class_bits << crate::GPU_PSO_MATERIAL_SHIFT);
            let key = (shader_index << 16) | (pso_bin & 0xFFFF);
            keyed.push((key, *shader_index, pso_bin));
        }
    }
    keyed.sort_unstable_by_key(|entry| entry.0);
    keyed.dedup_by_key(|entry| entry.0);
    keyed.truncate(SCENE_EXECUTOR_BUCKET_CAPACITY as usize);
    let count = keyed.len() as u32;
    let slice = record_capacity.checked_div(count).unwrap_or(0).max(1);

    let mut buckets = Vec::with_capacity(keyed.len());
    let mut table = vec![0_u8; 16 + 512 * 8 + 512 * 4 + 512 * 4];
    table[0..4].copy_from_slice(&count.to_le_bytes());
    for (bucket, (key, shader_index, pso_bin)) in keyed.into_iter().enumerate() {
        let base = bucket as u32 * slice;
        let capacity = slice.min(record_capacity.saturating_sub(base));
        buckets.push(ExecutorBucket {
            shader_index,
            pso_bin,
            base,
            capacity,
        });
        let lookup = 16 + bucket * 8;
        table[lookup..lookup + 4].copy_from_slice(&key.to_le_bytes());
        table[lookup + 4..lookup + 8].copy_from_slice(&(bucket as u32).to_le_bytes());
        let bases = 16 + 512 * 8 + bucket * 4;
        table[bases..bases + 4].copy_from_slice(&base.to_le_bytes());
        let ends = 16 + 512 * 8 + 512 * 4 + bucket * 4;
        table[ends..ends + 4].copy_from_slice(&(base + capacity).to_le_bytes());
    }
    (buckets, table)
}
