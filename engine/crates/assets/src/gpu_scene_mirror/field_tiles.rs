use super::*;

/// Retires one cell's packed micro field tiles from the fields arena.
pub(super) fn remove_cell_fields(
    world_state: &mut WorldMirror,
    cell: WorldCellKey,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    if let Some(entry) = world_state.plant_fields.remove(&cell) {
        target.gpu_data.fields.retire(entry.range)?;
    }
    Ok(())
}

/// The packed field-tile blob plus each tile's family and byte offset within it.
type PackedFieldTiles = (Vec<u8>, Vec<(u64, u32, u32)>);

/// Packs one cell's micro field tiles for the fields arena: per tile, a
/// [`GpuFieldTileRecord`] header, the `u16` density samples (padded to four bytes),
/// then each attribute channel's 16-byte id + `i32` values.
pub(super) fn pack_field_tiles(tiles: &[MicroFieldTile]) -> Result<PackedFieldTiles> {
    let mut packed = Vec::new();
    let mut offsets = Vec::with_capacity(tiles.len());
    for tile in tiles {
        let offset = u32::try_from(packed.len())
            .map_err(|_| mirror_error("micro field tiles exceed the fields arena"))?;
        // The predicted budget: blades a fully visible frame would reconstruct,
        // summed from the same density → blade-count derivation the GPU uses.
        let predicted = tile
            .density
            .iter()
            .map(|density| (u32::from(*density) * 4 + 0xffff) >> 16)
            .sum::<u32>();
        offsets.push((tile.family.value(), offset, predicted));
        let sample_count = u32::try_from(tile.density.len())
            .map_err(|_| mirror_error("micro field tile density exceeds u32 samples"))?;
        let attribute_count = u32::try_from(tile.attributes.len())
            .map_err(|_| mirror_error("micro field tile channels exceed u32"))?;
        let seed = tile.reconstruction_seed.to_le_bytes();
        let header = GpuFieldTileRecord {
            cell: tile.cell.coordinates(),
            dims: tile.dimensions,
            sample_count,
            seed: [
                u32::from_le_bytes(seed[0..4].try_into().expect("4 bytes")),
                u32::from_le_bytes(seed[4..8].try_into().expect("4 bytes")),
                u32::from_le_bytes(seed[8..12].try_into().expect("4 bytes")),
                u32::from_le_bytes(seed[12..16].try_into().expect("4 bytes")),
            ],
            attribute_count,
            reserved: 0,
        };
        packed.extend_from_slice(bytemuck::bytes_of(&header));
        packed.extend_from_slice(bytemuck::cast_slice(&tile.density));
        if tile.density.len() % 2 != 0 {
            packed.extend_from_slice(&[0, 0]);
        }
        for (channel, values) in &tile.attributes {
            packed.extend_from_slice(&channel.to_le_bytes());
            packed.extend_from_slice(bytemuck::cast_slice(values));
        }
    }
    Ok((packed, offsets))
}

/// Reconciles the per-family field instances against the resident tiles: families that
/// gained tiles get one flagged identity instance; families with none left release it.
pub(super) fn reconcile_field_instances(
    shared: &mut SharedMirror,
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    families: &HashMap<u64, ContentHash>,
    assets: &mut AssetServer,
    gpu: &dyn GpuUploader,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    let referenced: HashSet<u64> = world_state
        .plant_fields
        .values()
        .flat_map(|entry| entry.tiles.iter().map(|(family, _, _)| *family))
        .collect();
    let stale: Vec<u64> = world_state
        .field_instances
        .keys()
        .filter(|family| !referenced.contains(family))
        .copied()
        .collect();
    for family in stale {
        let handle = world_state
            .field_instances
            .remove(&family)
            .expect("field instance present");
        target
            .gpu_scene
            .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(handle))
            .map_err(gpu_scene_error)?;
        shared.unref_mesh(family);
    }
    for family in referenced {
        if world_state.field_instances.contains_key(&family) {
            continue;
        }
        let Some(artifact) = families.get(&family) else {
            tracing::warn!("gpu scene mirror: field family {family} is not in the manifest");
            continue;
        };
        if assets
            .load_plant_family(gpu, Uuid(family), *artifact)
            .is_none()
        {
            continue;
        }
        if !shared.ensure_mesh(family, assets, gpu, target)? {
            continue;
        }
        let record = GpuSceneInstanceRecord {
            prototype: shared.meshes[&family].prototype,
            transform: GpuSceneTransform::Static(GpuSceneStaticTransform::new(
                saffron_spatial::WorldPosition::origin(),
                saffron_vegetation::QuantizedOrientation::identity(),
                [saffron_spatial::DecisionScalar::from_bits(1 << 16); 3],
                0,
            )),
            material_overrides: Arc::from([]),
            deformation: None,
            source_generation: shared.generation,
            flags: GPU_SCENE_INSTANCE_FLAG_MICRO_FIELD,
            vegetation: None,
            combination: 0,
        };
        shared.ref_mesh(family);
        let result = target
            .gpu_scene
            .apply_world_delta(world, GpuSceneWorldDelta::CreateInstance(record))
            .map_err(gpu_scene_error)?;
        let GpuSceneWorldDeltaResult::InstanceCreated(handle) = result else {
            unreachable!("create instance returns InstanceCreated");
        };
        world_state.field_instances.insert(family, handle);
    }
    Ok(())
}

/// Rebuilds the packed resident-tile directory the micro pass dispatches over, in
/// cell order for a deterministic stream.
pub(super) fn rebuild_field_directory(
    world_state: &mut WorldMirror,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    if let Some((range, _)) = world_state.field_directory.take() {
        target.gpu_data.fields.retire(range)?;
    }
    let mut cells: Vec<(&WorldCellKey, &CellFieldEntry)> =
        world_state.plant_fields.iter().collect();
    cells.sort_by_key(|(cell, _)| **cell);
    let mut entries = Vec::new();
    let mut predicted_total: u64 = 0;
    for (_, entry) in cells {
        for (family, offset, predicted) in &entry.tiles {
            let Some(instance) = world_state.field_instances.get(family) else {
                continue;
            };
            predicted_total += u64::from(*predicted);
            entries.push(GpuFieldDirectoryEntry {
                instance: instance.raw(),
                tile_offset: entry.range.first + offset,
                predicted: *predicted,
            });
        }
    }
    world_state.micro_predicted = predicted_total;
    if entries.is_empty() {
        return Ok(());
    }
    let bytes = bytemuck::cast_slice(&entries).to_vec();
    let byte_len = u32::try_from(bytes.len())
        .map_err(|_| mirror_error("micro field directory exceeds the fields arena"))?;
    let (range, _) = target.gpu_data.fields.allocate(byte_len, 16)?;
    target
        .pending
        .upload_arena(GpuArenaUploadRequest::Fields { range, data: bytes });
    let count = u32::try_from(entries.len())
        .map_err(|_| mirror_error("micro field directory exceeds u32 entries"))?;
    world_state.field_directory = Some((range, count));
    Ok(())
}
