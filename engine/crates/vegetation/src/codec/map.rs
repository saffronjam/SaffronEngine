//! The `.svegmap` root manifest and its sparse authored chunks.

use saffron_spatial::{
    DecisionVec3, SurfaceAttachment, SurfacePrimitiveId, SurfaceProviderId, SurfaceRevision,
};

use super::enums::{
    field_blend, field_blend_tag, inclusion, inclusion_tag, provenance_outcome,
    provenance_outcome_tag,
};
use super::stream::{Reader, Writer, invalid_enum};
use crate::hash::sha256;
use crate::*;

const MAP_MAGIC: &[u8; 8] = b"SVEGMAP1";
const MAP_CHUNK_MAGIC: &[u8; 8] = b"SVEGCH01";

/// SHA-256 identity of the `.svegmap` manifest field vocabulary.
#[must_use]
pub fn vegetation_map_schema_hash() -> [u8; 32] {
    sha256(b"saffron-anima/svegmap/schema/v2/identity+bounds+chunk-layout+generation+canonical-content-addressed-inventory")
}

/// SHA-256 identity of one authored sparse map-chunk field vocabulary.
#[must_use]
pub fn vegetation_map_chunk_schema_hash() -> [u8; 32] {
    sha256(b"saffron-anima/svegmap-object/schema/v3/map+layer+global-or-cell+typed-field-anchor-graph-layer-editor-payload+revision+provenance")
}

/// Writes one vegetation-map root to canonical `.svegmap` bytes.
pub fn write_vegetation_map_asset(asset: &VegetationMapAsset) -> Result<Vec<u8>> {
    validate_vegetation_map(asset)?;
    let mut writer = Writer::with_header(MAP_MAGIC, asset.version, vegetation_map_schema_hash());
    writer.uuid(asset.id);
    writer.string(&asset.name)?;
    writer.bounds(asset.bounds);
    writer.u8(asset.chunk_layout.level);
    writer.bytes(&asset.chunk_layout.schema_hash);
    writer.u64(asset.generation);
    writer.vec(&asset.inventory, |writer, reference| {
        write_map_chunk_key(writer, reference.key);
        writer.bytes(&reference.content_hash);
        writer.u64(reference.byte_length);
        writer.u64(reference.revision);
        Ok(())
    })?;
    Ok(writer.finish())
}

/// Reads and strictly validates one canonical `.svegmap` root byte stream.
pub fn read_vegetation_map_asset(bytes: &[u8]) -> Result<VegetationMapAsset> {
    let mut reader = Reader::with_header(
        bytes,
        MAP_MAGIC,
        VEGETATION_MAP_VERSION,
        vegetation_map_schema_hash(),
        ".svegmap",
    )?;
    let asset = VegetationMapAsset {
        version: VEGETATION_MAP_VERSION,
        id: reader.uuid()?,
        name: reader.string()?,
        bounds: reader.bounds()?,
        chunk_layout: VegetationMapChunkLayout {
            level: reader.u8()?,
            schema_hash: reader.array()?,
        },
        generation: reader.u64()?,
        inventory: reader.vec(|reader| {
            Ok(VegetationMapChunkReference {
                key: read_map_chunk_key(reader)?,
                content_hash: reader.array()?,
                byte_length: reader.u64()?,
                revision: reader.u64()?,
            })
        })?,
    };
    reader.complete()?;
    validate_vegetation_map(&asset)?;
    Ok(asset)
}

/// Writes one typed immutable authored map object to canonical internal bytes.
pub fn write_vegetation_map_chunk(chunk: &VegetationMapChunk) -> Result<Vec<u8>> {
    validate_map_chunk(chunk)?;
    let mut chunk = chunk.clone();
    canonicalize_map_chunk(&mut chunk);
    let mut writer = Writer::with_header(
        MAP_CHUNK_MAGIC,
        chunk.version,
        vegetation_map_chunk_schema_hash(),
    );
    writer.uuid(chunk.map);
    write_map_chunk_key(&mut writer, chunk.key);
    writer.u64(chunk.revision);
    match &chunk.payload {
        VegetationMapChunkPayload::Field(payload) => {
            writer.vec(&payload.fields, write_authored_field)?;
            writer.vec(&payload.blockers, write_authored_field)?;
        }
        VegetationMapChunkPayload::AnchorOverride(payload) => {
            writer.vec(&payload.explicit_plants, |writer, anchor| {
                writer.plant_id(anchor.id);
                writer.u128(anchor.layer);
                writer.uuid(anchor.family);
                write_plant_point(writer, &anchor.point)
            })?;
            writer.vec(&payload.pins, |writer, plant| {
                writer.plant_id(*plant);
                Ok(())
            })?;
            writer.vec(&payload.transform_overrides, |writer, value| {
                writer.plant_id(value.plant);
                writer.position(value.position);
                writer.fixed3(value.scale);
                Ok(())
            })?;
            writer.vec(&payload.state_overrides, |writer, value| {
                writer.plant_id(value.plant);
                writer.option(value.health, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.moisture, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.fuel, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.interaction_policy, |writer, value| {
                    writer.u32(value as u32);
                    Ok(())
                })
            })?;
            write_provenance(&mut writer, &payload.provenance)?;
        }
        VegetationMapChunkPayload::GraphInstance(instance) => {
            write_local_biome_instance(&mut writer, instance)?;
        }
        VegetationMapChunkPayload::LayerMetadata(layer) => write_layer(&mut writer, layer)?,
        VegetationMapChunkPayload::EditorMetadata(gestures) => {
            writer.vec(gestures, |writer, gesture| {
                writer.u128(gesture.gesture);
                writer.u128(gesture.layer);
                writer.vec(&gesture.samples, |writer, sample| {
                    writer.position(*sample);
                    Ok(())
                })
            })?;
        }
    }
    Ok(writer.finish())
}

fn write_provenance(writer: &mut Writer, provenance: &ProvenanceTable) -> Result<()> {
    writer.vec(provenance.decisions(), |writer, decision| {
        writer.vec(&decision.parents, |writer, parent| {
            writer.u32(parent.0);
            Ok(())
        })?;
        writer.vec(&decision.subgraph_path, |writer, call| {
            writer.u128(*call);
            Ok(())
        })?;
        writer.u128(decision.node);
        writer.string(decision.operator.as_wire())?;
        writer.u64(decision.candidate);
        writer.u8(provenance_outcome_tag(decision.outcome));
        Ok(())
    })?;
    writer.vec(provenance.records(), |writer, record| {
        writer.uuid(record.map);
        writer.u128(record.layer);
        writer.uuid(record.biome);
        writer.u32(record.decision.0);
        writer.u64(record.candidate);
        writer.option(record.family, |writer, family| {
            writer.uuid(family);
            Ok(())
        })?;
        writer.option(record.plant, |writer, plant| {
            writer.plant_id(plant);
            Ok(())
        })?;
        writer.u32(record.variation);
        Ok(())
    })
}

/// Reads and strictly validates one canonical immutable authored map object.
pub fn read_vegetation_map_chunk(bytes: &[u8]) -> Result<VegetationMapChunk> {
    let mut reader = Reader::with_header(
        bytes,
        MAP_CHUNK_MAGIC,
        VEGETATION_MAP_CHUNK_VERSION,
        vegetation_map_chunk_schema_hash(),
        ".svegmap chunk",
    )?;
    let map = reader.uuid()?;
    let key = read_map_chunk_key(&mut reader)?;
    let revision = reader.u64()?;
    let payload = match key.kind {
        VegetationMapChunkKind::Field => {
            VegetationMapChunkPayload::Field(VegetationMapFieldChunk {
                fields: reader.vec(read_authored_field)?,
                blockers: reader.vec(read_authored_field)?,
            })
        }
        VegetationMapChunkKind::AnchorOverride => {
            let explicit_plants = reader.vec(|reader| {
                Ok(ExplicitPlantAnchor {
                    id: reader.plant_id()?,
                    layer: reader.u128()?,
                    family: reader.uuid()?,
                    point: read_plant_point(reader)?,
                })
            })?;
            let pins = reader.vec(Reader::plant_id)?;
            let transform_overrides = reader.vec(|reader| {
                Ok(PlantTransformOverride {
                    plant: reader.plant_id()?,
                    position: reader.position()?,
                    scale: reader.fixed3()?,
                })
            })?;
            let state_overrides = reader.vec(|reader| {
                Ok(PlantStateOverride {
                    plant: reader.plant_id()?,
                    health: reader.option(Reader::unit)?,
                    moisture: reader.option(Reader::unit)?,
                    fuel: reader.option(Reader::unit)?,
                    interaction_policy: reader
                        .option(|reader| InteractionPolicy::try_from(reader.u32()?))?,
                })
            })?;
            VegetationMapChunkPayload::AnchorOverride(VegetationMapAnchorChunk {
                explicit_plants,
                pins,
                transform_overrides,
                state_overrides,
                provenance: read_provenance(&mut reader)?,
            })
        }
        VegetationMapChunkKind::GraphInstance => {
            VegetationMapChunkPayload::GraphInstance(read_local_biome_instance(&mut reader)?)
        }
        VegetationMapChunkKind::LayerMetadata => {
            VegetationMapChunkPayload::LayerMetadata(read_layer(&mut reader)?)
        }
        VegetationMapChunkKind::EditorMetadata => {
            VegetationMapChunkPayload::EditorMetadata(reader.vec(|reader| {
                Ok(BrushGestureMetadata {
                    gesture: reader.u128()?,
                    layer: reader.u128()?,
                    samples: reader.vec(Reader::position)?,
                })
            })?)
        }
    };
    let chunk = VegetationMapChunk {
        version: VEGETATION_MAP_CHUNK_VERSION,
        map,
        key,
        revision,
        payload,
    };
    reader.complete()?;
    validate_map_chunk(&chunk)?;
    Ok(chunk)
}

fn read_provenance(reader: &mut Reader<'_>) -> Result<ProvenanceTable> {
    let decisions: Vec<ProvenanceDecision> = reader.vec(|reader| {
        Ok(ProvenanceDecision {
            parents: reader.vec(|reader| Ok(ProvenanceDecisionHandle(reader.u32()?)))?,
            subgraph_path: reader.vec(Reader::u128)?,
            node: reader.u128()?,
            operator: GraphOperator::from_wire(&reader.string()?)
                .ok_or_else(|| reader.invalid("provenance.decision.operator"))?,
            candidate: reader.u64()?,
            outcome: provenance_outcome(reader.u8()?)?,
        })
    })?;
    let records: Vec<ProvenanceRecord> = reader.vec(|reader| {
        Ok(ProvenanceRecord {
            map: reader.uuid()?,
            layer: reader.u128()?,
            biome: reader.uuid()?,
            decision: ProvenanceDecisionHandle(reader.u32()?),
            candidate: reader.u64()?,
            family: reader.option(Reader::uuid)?,
            plant: reader.option(Reader::plant_id)?,
            variation: reader.u32()?,
        })
    })?;
    let mut provenance = ProvenanceTable::default();
    for (index, decision) in decisions.into_iter().enumerate() {
        let handle = provenance.intern_decision(decision);
        if usize::try_from(handle.0).ok() != Some(index) {
            return Err(reader.invalid("provenance.decisions"));
        }
    }
    for (index, record) in records.into_iter().enumerate() {
        let handle = provenance.intern(record);
        if usize::try_from(handle.0).ok() != Some(index) {
            return Err(reader.invalid("provenance.records"));
        }
    }
    Ok(provenance)
}

fn validate_map_chunk(chunk: &VegetationMapChunk) -> Result<()> {
    if chunk.version != VEGETATION_MAP_CHUNK_VERSION {
        return Err(Error::FormatVersion {
            format: ".svegmap chunk",
            found: chunk.version,
            expected: VEGETATION_MAP_CHUNK_VERSION,
        });
    }
    if chunk.map.value() == 0
        || chunk.key.layer == 0
        || chunk.key.kind != chunk.payload.kind()
        || !matches!(
            (chunk.key.kind, chunk.key.tile),
            (
                VegetationMapChunkKind::Field | VegetationMapChunkKind::AnchorOverride,
                VegetationMapTileKey::Cell(_)
            ) | (
                VegetationMapChunkKind::GraphInstance
                    | VegetationMapChunkKind::LayerMetadata
                    | VegetationMapChunkKind::EditorMetadata,
                VegetationMapTileKey::Global
            )
        )
    {
        return Err(Error::InvalidFormat {
            format: ".svegmap chunk",
            field: "map/key/payload".to_owned(),
        });
    }
    match &chunk.payload {
        VegetationMapChunkPayload::Field(payload) => {
            if payload.fields.is_empty() && payload.blockers.is_empty() {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "fields/blockers".to_owned(),
                });
            }
            let mut field_keys = std::collections::BTreeSet::new();
            for (blocker, field) in payload
                .fields
                .iter()
                .map(|field| (false, field))
                .chain(payload.blockers.iter().map(|field| (true, field)))
            {
                let sample_count =
                    field
                        .dimensions
                        .iter()
                        .try_fold(1_u64, |product, dimension| {
                            product
                                .checked_mul(u64::from(*dimension))
                                .ok_or(Error::NumericOverflow)
                        })?;
                if field.layer != chunk.key.layer
                    || field.quantum_bits <= 0
                    || field.dimensions.contains(&0)
                    || usize::try_from(sample_count).ok() != Some(field.values.len())
                    || !field_keys.insert((blocker, field.channel))
                {
                    return Err(Error::InvalidFormat {
                        format: ".svegmap chunk",
                        field: "fields/blockers".to_owned(),
                    });
                }
            }
        }
        VegetationMapChunkPayload::AnchorOverride(payload) => {
            let VegetationMapTileKey::Cell(cell) = chunk.key.tile else {
                unreachable!();
            };
            if payload.explicit_plants.is_empty()
                && payload.pins.is_empty()
                && payload.transform_overrides.is_empty()
                && payload.state_overrides.is_empty()
                && payload.provenance.records().is_empty()
            {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "anchorOverrides".to_owned(),
                });
            }
            let mut anchor_ids = std::collections::BTreeSet::new();
            for anchor in &payload.explicit_plants {
                anchor.point.validate()?;
                if anchor.id != anchor.point.id
                    || anchor.layer != chunk.key.layer
                    || anchor.family != anchor.point.family
                    || !cell.bounds().contains(anchor.point.position)
                    || anchor.id.namespace()? != PlantIdNamespace::Explicit
                    || payload
                        .provenance
                        .get(ProvenanceHandle(anchor.point.provenance))
                        .is_none()
                    || !anchor_ids.insert(anchor.id)
                {
                    return Err(Error::InvalidFormat {
                        format: ".svegmap chunk",
                        field: "explicitPlants".to_owned(),
                    });
                }
            }
            if !all_unique(payload.pins.iter().copied())
                || !all_unique(payload.transform_overrides.iter().map(|value| value.plant))
                || !all_unique(payload.state_overrides.iter().map(|value| value.plant))
            {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "pins/transformOverrides/stateOverrides".to_owned(),
                });
            }
            for (index, decision) in payload.provenance.decisions().iter().enumerate() {
                if decision.node == 0
                    || decision.parents.iter().any(|parent| {
                        usize::try_from(parent.0).map_or(true, |parent| parent >= index)
                    })
                {
                    return Err(Error::InvalidFormat {
                        format: ".svegmap chunk",
                        field: "provenance.decisions".to_owned(),
                    });
                }
            }
            for record in payload.provenance.records() {
                if record.map != chunk.map
                    || record.layer != chunk.key.layer
                    || payload.provenance.decision(record.decision).is_none()
                    || record.plant.is_some() && record.family.is_none()
                {
                    return Err(Error::InvalidFormat {
                        format: ".svegmap chunk",
                        field: "provenance.records".to_owned(),
                    });
                }
            }
        }
        VegetationMapChunkPayload::GraphInstance(instance) => {
            if instance.id != chunk.key.layer
                || instance.biome.value() == 0
                || !all_unique(instance.bindings.iter().map(|(parameter, _)| *parameter))
            {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "graphInstance".to_owned(),
                });
            }
        }
        VegetationMapChunkPayload::LayerMetadata(layer) => {
            if layer.id != chunk.key.layer
                || !all_unique(layer.dependencies.iter().copied())
                || layer.dependencies.contains(&layer.id)
            {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "layerMetadata.id".to_owned(),
                });
            }
        }
        VegetationMapChunkPayload::EditorMetadata(gestures) => {
            let mut ids = std::collections::BTreeSet::new();
            if gestures.is_empty()
                || gestures.iter().any(|gesture| {
                    gesture.gesture == 0
                        || gesture.layer != chunk.key.layer
                        || !ids.insert(gesture.gesture)
                })
            {
                return Err(Error::InvalidFormat {
                    format: ".svegmap chunk",
                    field: "editorMetadata".to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn canonicalize_map_chunk(chunk: &mut VegetationMapChunk) {
    match &mut chunk.payload {
        VegetationMapChunkPayload::Field(payload) => {
            payload.fields.sort_by_key(|field| field.channel);
            payload.blockers.sort_by_key(|field| field.channel);
        }
        VegetationMapChunkPayload::AnchorOverride(payload) => {
            payload.explicit_plants.sort_by_key(|anchor| anchor.id);
            payload.pins.sort_unstable();
            payload.transform_overrides.sort_by_key(|value| value.plant);
            payload.state_overrides.sort_by_key(|value| value.plant);
        }
        VegetationMapChunkPayload::GraphInstance(instance) => {
            instance.bindings.sort_by_key(|(parameter, _)| *parameter);
        }
        VegetationMapChunkPayload::LayerMetadata(layer) => {
            layer.dependencies.sort_unstable();
        }
        VegetationMapChunkPayload::EditorMetadata(gestures) => {
            gestures.sort_by_key(|gesture| gesture.gesture);
        }
    }
}

fn all_unique<T: Ord>(values: impl IntoIterator<Item = T>) -> bool {
    let mut unique = std::collections::BTreeSet::new();
    values.into_iter().all(|value| unique.insert(value))
}

fn write_local_biome_instance(writer: &mut Writer, instance: &LocalBiomeInstance) -> Result<()> {
    writer.u128(instance.id);
    writer.uuid(instance.biome);
    writer.bounds(instance.bounds);
    writer.vec(&instance.bindings, |writer, (parameter, value)| {
        writer.u128(*parameter);
        writer.value(value)
    })?;
    writer.u64(instance.revision);
    Ok(())
}

fn read_local_biome_instance(reader: &mut Reader<'_>) -> Result<LocalBiomeInstance> {
    Ok(LocalBiomeInstance {
        id: reader.u128()?,
        biome: reader.uuid()?,
        bounds: reader.bounds()?,
        bindings: reader.vec(|reader| Ok((reader.u128()?, reader.value()?)))?,
        revision: reader.u64()?,
    })
}

fn write_map_chunk_key(writer: &mut Writer, key: VegetationMapChunkKey) {
    writer.u128(key.layer);
    match key.tile {
        VegetationMapTileKey::Global => writer.u8(0),
        VegetationMapTileKey::Cell(cell) => {
            writer.u8(1);
            writer.cell(cell);
        }
    }
    writer.u8(match key.kind {
        VegetationMapChunkKind::Field => 0,
        VegetationMapChunkKind::AnchorOverride => 1,
        VegetationMapChunkKind::GraphInstance => 2,
        VegetationMapChunkKind::LayerMetadata => 3,
        VegetationMapChunkKind::EditorMetadata => 4,
    });
}

fn read_map_chunk_key(reader: &mut Reader<'_>) -> Result<VegetationMapChunkKey> {
    let layer = reader.u128()?;
    let tile = match reader.u8()? {
        0 => VegetationMapTileKey::Global,
        1 => VegetationMapTileKey::Cell(reader.cell()?),
        _ => return Err(invalid_enum(".svegmap chunk", "key.tile")),
    };
    let kind = match reader.u8()? {
        0 => VegetationMapChunkKind::Field,
        1 => VegetationMapChunkKind::AnchorOverride,
        2 => VegetationMapChunkKind::GraphInstance,
        3 => VegetationMapChunkKind::LayerMetadata,
        4 => VegetationMapChunkKind::EditorMetadata,
        _ => return Err(invalid_enum(".svegmap chunk", "key.kind")),
    };
    Ok(VegetationMapChunkKey { layer, tile, kind })
}

fn write_authored_field(writer: &mut Writer, field: &AuthoredFieldTile) -> Result<()> {
    writer.field_channel(field.channel);
    writer.u128(field.layer);
    for dimension in field.dimensions {
        writer.u32(dimension);
    }
    writer.i32(field.quantum_bits);
    writer.vec(&field.values, |writer, value| {
        writer.i32(*value);
        Ok(())
    })
}

fn read_authored_field(reader: &mut Reader<'_>) -> Result<AuthoredFieldTile> {
    Ok(AuthoredFieldTile {
        channel: reader.field_channel()?,
        layer: reader.u128()?,
        dimensions: [reader.u32()?, reader.u32()?, reader.u32()?],
        quantum_bits: reader.i32()?,
        values: reader.vec(Reader::i32)?,
    })
}

fn write_layer(writer: &mut Writer, layer: &VegetationLayer) -> Result<()> {
    writer.u128(layer.id);
    writer.string(&layer.name)?;
    writer.u8(match layer.coordinate_space {
        LayerCoordinateSpace::World => 0,
        LayerCoordinateSpace::Surface => 1,
        LayerCoordinateSpace::OwnerLocal => 2,
    });
    writer.bounds(layer.bounds);
    write_layer_operator(writer, &layer.operator)?;
    writer.vec(&layer.dependencies, |writer, dependency| {
        writer.u128(*dependency);
        Ok(())
    })?;
    writer.i32(layer.order);
    writer.bool(layer.locked);
    writer.bool(layer.muted);
    writer.u64(layer.revision);
    Ok(())
}

fn read_layer(reader: &mut Reader<'_>) -> Result<VegetationLayer> {
    Ok(VegetationLayer {
        id: reader.u128()?,
        name: reader.string()?,
        coordinate_space: match reader.u8()? {
            0 => LayerCoordinateSpace::World,
            1 => LayerCoordinateSpace::Surface,
            2 => LayerCoordinateSpace::OwnerLocal,
            _ => return Err(reader.invalid("layers.coordinateSpace")),
        },
        bounds: reader.bounds()?,
        operator: read_layer_operator(reader)?,
        dependencies: reader.vec(Reader::u128)?,
        order: reader.i32()?,
        locked: reader.bool()?,
        muted: reader.bool()?,
        revision: reader.u64()?,
    })
}

fn write_layer_operator(writer: &mut Writer, operator: &VegetationLayerOperator) -> Result<()> {
    match operator {
        VegetationLayerOperator::ScalarField(field) => {
            writer.u8(0);
            write_field_layer(writer, field);
        }
        VegetationLayerOperator::VectorField {
            channel,
            tile_set,
            value,
            blend,
        } => {
            writer.u8(1);
            writer.field_channel(*channel);
            writer.u128(*tile_set);
            writer.fixed3([value.x, value.y, value.z]);
            writer.u8(field_blend_tag(*blend));
        }
        VegetationLayerOperator::SpeciesWeights(weights) => {
            writer.u8(2);
            writer.vec(weights, |writer, weight| {
                writer.uuid(weight.family);
                writer.unit(weight.weight);
                Ok(())
            })?;
        }
        VegetationLayerOperator::Density(field) => {
            writer.u8(3);
            write_field_layer(writer, field);
        }
        VegetationLayerOperator::Mask {
            tile_set,
            operation,
        } => {
            writer.u8(4);
            writer.u128(*tile_set);
            writer.u8(inclusion_tag(*operation));
        }
        VegetationLayerOperator::Volume(volume) => {
            writer.u8(5);
            writer.bounds(volume.bounds);
            writer.u8(inclusion_tag(volume.operation));
            writer.fixed(volume.falloff);
        }
        VegetationLayerOperator::Spline(spline) => {
            writer.u8(6);
            writer.u128(spline.spline);
            writer.vec(&spline.points, |writer, point| {
                writer.position(*point);
                Ok(())
            })?;
            writer.fixed(spline.radius);
            writer.u8(inclusion_tag(spline.operation));
        }
        VegetationLayerOperator::Anchors(plants) => {
            writer.u8(7);
            write_plant_ids(writer, plants)?;
        }
        VegetationLayerOperator::Pins(plants) => {
            writer.u8(8);
            write_plant_ids(writer, plants)?;
        }
        VegetationLayerOperator::TransformOverrides(values) => {
            writer.u8(9);
            writer.vec(values, |writer, value| {
                writer.plant_id(value.plant);
                writer.position(value.position);
                writer.fixed3(value.scale);
                Ok(())
            })?;
        }
        VegetationLayerOperator::StateOverrides(values) => {
            writer.u8(10);
            writer.vec(values, |writer, value| {
                writer.plant_id(value.plant);
                writer.option(value.health, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.moisture, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.fuel, |writer, value| {
                    writer.unit(value);
                    Ok(())
                })?;
                writer.option(value.interaction_policy, |writer, value| {
                    writer.u32(value as u32);
                    Ok(())
                })
            })?;
        }
        VegetationLayerOperator::Blocker {
            tile_set,
            categories,
        } => {
            writer.u8(11);
            writer.u128(*tile_set);
            writer.u32(*categories);
        }
    }
    Ok(())
}

fn read_layer_operator(reader: &mut Reader<'_>) -> Result<VegetationLayerOperator> {
    match reader.u8()? {
        0 => Ok(VegetationLayerOperator::ScalarField(read_field_layer(
            reader,
        )?)),
        1 => {
            let channel = reader.field_channel()?;
            let tile_set = reader.u128()?;
            let fixed = reader.fixed3()?;
            Ok(VegetationLayerOperator::VectorField {
                channel,
                tile_set,
                value: DecisionVec3 {
                    x: fixed[0],
                    y: fixed[1],
                    z: fixed[2],
                },
                blend: field_blend(reader.u8()?)?,
            })
        }
        2 => Ok(VegetationLayerOperator::SpeciesWeights(reader.vec(
            |reader| {
                Ok(SpeciesWeight {
                    family: reader.uuid()?,
                    weight: reader.unit()?,
                })
            },
        )?)),
        3 => Ok(VegetationLayerOperator::Density(read_field_layer(reader)?)),
        4 => Ok(VegetationLayerOperator::Mask {
            tile_set: reader.u128()?,
            operation: inclusion(reader.u8()?)?,
        }),
        5 => Ok(VegetationLayerOperator::Volume(VolumeLayer {
            bounds: reader.bounds()?,
            operation: inclusion(reader.u8()?)?,
            falloff: reader.fixed()?,
        })),
        6 => Ok(VegetationLayerOperator::Spline(SplineLayer {
            spline: reader.u128()?,
            points: reader.vec(Reader::position)?,
            radius: reader.fixed()?,
            operation: inclusion(reader.u8()?)?,
        })),
        7 => Ok(VegetationLayerOperator::Anchors(
            reader.vec(Reader::plant_id)?,
        )),
        8 => Ok(VegetationLayerOperator::Pins(reader.vec(Reader::plant_id)?)),
        9 => Ok(VegetationLayerOperator::TransformOverrides(reader.vec(
            |reader| {
                Ok(PlantTransformOverride {
                    plant: reader.plant_id()?,
                    position: reader.position()?,
                    scale: reader.fixed3()?,
                })
            },
        )?)),
        10 => Ok(VegetationLayerOperator::StateOverrides(reader.vec(
            |reader| {
                Ok(PlantStateOverride {
                    plant: reader.plant_id()?,
                    health: reader.option(Reader::unit)?,
                    moisture: reader.option(Reader::unit)?,
                    fuel: reader.option(Reader::unit)?,
                    interaction_policy: reader
                        .option(|reader| InteractionPolicy::try_from(reader.u32()?))?,
                })
            },
        )?)),
        11 => Ok(VegetationLayerOperator::Blocker {
            tile_set: reader.u128()?,
            categories: reader.u32()?,
        }),
        _ => Err(reader.invalid("layers.operator")),
    }
}

fn write_field_layer(writer: &mut Writer, field: &FieldTileLayer) {
    writer.field_channel(field.channel);
    writer.u128(field.tile_set);
    writer.u8(field_blend_tag(field.blend));
    writer.unit(field.weight);
}

fn read_field_layer(reader: &mut Reader<'_>) -> Result<FieldTileLayer> {
    Ok(FieldTileLayer {
        channel: reader.field_channel()?,
        tile_set: reader.u128()?,
        blend: field_blend(reader.u8()?)?,
        weight: reader.unit()?,
    })
}

fn write_plant_ids(writer: &mut Writer, plants: &[PlantId]) -> Result<()> {
    writer.vec(plants, |writer, plant| {
        writer.plant_id(*plant);
        Ok(())
    })
}

fn write_plant_point(writer: &mut Writer, point: &PlantPoint) -> Result<()> {
    point.validate()?;
    writer.plant_id(point.id);
    writer.cell(point.owner);
    writer.position(point.position);
    for lane in point.orientation.bits() {
        writer.i16(lane);
    }
    writer.fixed3(point.scale);
    writer.bounds(point.bounds);
    writer.uuid(point.family);
    writer.u32(point.variation);
    writer.u32(point.lifecycle as u32);
    writer.u32(point.phenotype);
    writer.u32(point.representation_class);
    writer.u128(point.deterministic_key);
    writer.u64(point.candidate);
    writer.option(point.parent, |writer, value| {
        writer.plant_id(value);
        Ok(())
    })?;
    writer.option(point.colony, |writer, value| {
        writer.plant_id(value);
        Ok(())
    })?;
    writer.u64(point.ecology_tick);
    writer.unit(point.health);
    writer.unit(point.moisture);
    writer.unit(point.fuel);
    writer.unit(point.phenology);
    writer.u32(point.flags.bits());
    writer.u32(point.interaction_policy as u32);
    writer.u32(point.provenance);
    writer.option(point.attachment, |writer, value| {
        writer.u64(value.provider.0);
        writer.u64(value.primitive.0);
        for barycentric in value.barycentric {
            writer.unit(barycentric);
        }
        writer.u64(value.revision.0);
        Ok(())
    })?;
    writer.fixed3(point.surface_projection);
    Ok(())
}

fn read_plant_point(reader: &mut Reader<'_>) -> Result<PlantPoint> {
    let point = PlantPoint {
        id: reader.plant_id()?,
        owner: reader.cell()?,
        position: reader.position()?,
        orientation: QuantizedOrientation::new([
            reader.i16()?,
            reader.i16()?,
            reader.i16()?,
            reader.i16()?,
        ])?,
        scale: reader.fixed3()?,
        bounds: reader.bounds()?,
        family: reader.uuid()?,
        variation: reader.u32()?,
        lifecycle: PlantLifecycle::try_from(reader.u32()?)?,
        phenotype: reader.u32()?,
        representation_class: reader.u32()?,
        deterministic_key: reader.u128()?,
        candidate: reader.u64()?,
        parent: reader.option(Reader::plant_id)?,
        colony: reader.option(Reader::plant_id)?,
        ecology_tick: reader.u64()?,
        health: reader.unit()?,
        moisture: reader.unit()?,
        fuel: reader.unit()?,
        phenology: reader.unit()?,
        flags: PlantFlags::from_bits(reader.u32()?)?,
        interaction_policy: InteractionPolicy::try_from(reader.u32()?)?,
        provenance: reader.u32()?,
        attachment: reader.option(|reader| {
            Ok(SurfaceAttachment::new(
                SurfaceProviderId(reader.u64()?),
                SurfacePrimitiveId(reader.u64()?),
                [reader.unit()?, reader.unit()?, reader.unit()?],
                SurfaceRevision(reader.u64()?),
            )?)
        })?,
        surface_projection: reader.fixed3()?,
    };
    point.validate()?;
    Ok(point)
}

#[cfg(test)]
mod tests {
    use saffron_core::Uuid;
    use saffron_spatial::{FieldChannel, UnitInterval, WorldBounds, WorldCellKey};

    use super::*;

    #[test]
    fn map_root_and_typed_objects_round_trip_canonical_bytes() {
        let bounds = WorldBounds::new([0; 3], [1024; 3]).unwrap();
        let layer = VegetationLayer {
            id: 32,
            name: "Density".to_owned(),
            coordinate_space: LayerCoordinateSpace::World,
            bounds,
            operator: VegetationLayerOperator::Density(FieldTileLayer {
                channel: FieldChannel::Moisture,
                tile_set: 33,
                blend: FieldBlendOperator::Multiply,
                weight: UnitInterval::ONE,
            }),
            dependencies: Vec::new(),
            order: 0,
            locked: false,
            muted: false,
            revision: 1,
        };
        let layer_chunk = VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map: Uuid(31),
            key: VegetationMapChunkKey {
                layer: layer.id,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::LayerMetadata,
            },
            revision: layer.revision,
            payload: VegetationMapChunkPayload::LayerMetadata(layer),
        };
        let field_chunk = VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map: Uuid(31),
            key: VegetationMapChunkKey {
                layer: 32,
                tile: VegetationMapTileKey::Cell(WorldCellKey::base(0, 0, 0)),
                kind: VegetationMapChunkKind::Field,
            },
            revision: 2,
            payload: VegetationMapChunkPayload::Field(VegetationMapFieldChunk {
                fields: vec![AuthoredFieldTile {
                    channel: FieldChannel::Moisture,
                    layer: 32,
                    dimensions: [2, 1, 1],
                    quantum_bits: 1,
                    values: vec![4, 5],
                }],
                blockers: Vec::new(),
            }),
        };
        for chunk in [&layer_chunk, &field_chunk] {
            let bytes = write_vegetation_map_chunk(chunk).unwrap();
            let decoded = read_vegetation_map_chunk(&bytes).unwrap();
            assert_eq!(&decoded, chunk);
            assert_eq!(write_vegetation_map_chunk(&decoded).unwrap(), bytes);
        }
        let mut inventory = vec![
            field_chunk.reference().unwrap(),
            layer_chunk.reference().unwrap(),
        ];
        inventory.sort_by_key(VegetationMapChunkReference::order_key);
        let map = VegetationMapAsset {
            version: VEGETATION_MAP_VERSION,
            id: Uuid(31),
            name: "World vegetation".to_owned(),
            bounds,
            chunk_layout: VegetationMapChunkLayout {
                level: 0,
                schema_hash: vegetation_map_chunk_schema_hash(),
            },
            generation: 1,
            inventory,
        };
        let map_bytes = write_vegetation_map_asset(&map).unwrap();
        assert_eq!(read_vegetation_map_asset(&map_bytes).unwrap(), map);
        assert_eq!(write_vegetation_map_asset(&map).unwrap(), map_bytes);
    }

    #[test]
    fn map_codecs_reject_truncation_corruption_and_old_versions() {
        let root = VegetationMapAsset {
            version: VEGETATION_MAP_VERSION,
            id: Uuid(31),
            name: "World vegetation".to_owned(),
            bounds: WorldBounds::new([0; 3], [1024; 3]).unwrap(),
            chunk_layout: VegetationMapChunkLayout {
                level: 0,
                schema_hash: vegetation_map_chunk_schema_hash(),
            },
            generation: 0,
            inventory: Vec::new(),
        };
        let root_bytes = write_vegetation_map_asset(&root).unwrap();
        assert!(read_vegetation_map_asset(&root_bytes[..root_bytes.len() - 1]).is_err());
        let mut old_root = root_bytes;
        old_root[11] = (VEGETATION_MAP_VERSION - 1) as u8;
        assert!(matches!(
            read_vegetation_map_asset(&old_root),
            Err(Error::FormatVersion { .. })
        ));

        let chunk = VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map: Uuid(31),
            key: VegetationMapChunkKey {
                layer: 32,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::EditorMetadata,
            },
            revision: 1,
            payload: VegetationMapChunkPayload::EditorMetadata(vec![BrushGestureMetadata {
                gesture: 1,
                layer: 32,
                samples: Vec::new(),
            }]),
        };
        let bytes = write_vegetation_map_chunk(&chunk).unwrap();
        assert!(read_vegetation_map_chunk(&bytes[..bytes.len() - 1]).is_err());
        let mut corrupt_schema = bytes.clone();
        corrupt_schema[12] ^= 1;
        assert!(read_vegetation_map_chunk(&corrupt_schema).is_err());
        let mut old_version = bytes;
        old_version[11] = (VEGETATION_MAP_CHUNK_VERSION - 1) as u8;
        assert!(matches!(
            read_vegetation_map_chunk(&old_version),
            Err(Error::FormatVersion { .. })
        ));
    }
}
