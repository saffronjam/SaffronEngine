//! The canonical byte encoding of one global stage's inputs, hashed into the snapshot a
//! cook key depends on.

use saffron_spatial::{FieldChannel, FieldDerivative, WorldBounds, WorldCellKey};
use saffron_vegetation::{
    CompiledGlobalStage, EvaluationFieldSource, EvaluationRegionKind, FieldBlendOperator,
    GraphDependencySource, GraphEvaluationInputs, PlantPointColumns, QuantizedFieldTileValues,
    vegetation_content_hash,
};

use crate::Result;

pub(super) fn global_stage_input_snapshot(
    stage: &CompiledGlobalStage,
    inputs: &GraphEvaluationInputs,
    prerequisite_snapshots: &[([u8; 32], WorldCellKey, [u8; 32])],
) -> Result<[u8; 32]> {
    let mut bytes = b"saffron-anima/vegetation-global-stage-input/v1\0".to_vec();
    bytes.extend_from_slice(&stage.id);
    bytes.push(stage.owner_level);
    bytes.push(stage.minimum_input_level);
    bytes.extend_from_slice(&stage.upstream_halo.bits().to_be_bytes());
    bytes.extend_from_slice(&inputs.map.value().to_be_bytes());
    bytes.extend_from_slice(&inputs.biome_instance.to_be_bytes());
    bytes.extend_from_slice(&inputs.output_cell.canonical_bytes());
    append_bounds(&mut bytes, inputs.output_bounds);
    append_bounds(&mut bytes, inputs.read_bounds);
    bytes.extend_from_slice(&inputs.ecology_tick.to_be_bytes());

    bytes.extend_from_slice(&(stage.dependencies.len() as u64).to_be_bytes());
    for dependency in &stage.dependencies {
        append_dependency_source(&mut bytes, dependency.source);
        bytes.extend_from_slice(&dependency.content_hash);
    }
    bytes.extend_from_slice(&(prerequisite_snapshots.len() as u64).to_be_bytes());
    for (prerequisite_stage, prerequisite_owner, prerequisite_snapshot) in prerequisite_snapshots {
        bytes.extend_from_slice(prerequisite_stage);
        bytes.extend_from_slice(&prerequisite_owner.canonical_bytes());
        bytes.extend_from_slice(prerequisite_snapshot);
    }

    bytes.extend_from_slice(&(inputs.regions.len() as u64).to_be_bytes());
    for region in &inputs.regions {
        bytes.extend_from_slice(&region.id.to_be_bytes());
        bytes.push(match region.kind {
            EvaluationRegionKind::Biome => 0,
            EvaluationRegionKind::Shape => 1,
        });
        bytes.extend_from_slice(&region.layer.to_be_bytes());
        append_optional_u128(&mut bytes, region.hierarchy_namespace);
        bytes.extend_from_slice(&region.seed_cell.canonical_bytes());
        append_bounds(&mut bytes, region.bounds);
    }

    bytes.extend_from_slice(&(inputs.splines.len() as u64).to_be_bytes());
    for spline in &inputs.splines {
        bytes.extend_from_slice(&spline.id.to_be_bytes());
        bytes.extend_from_slice(&spline.layer.to_be_bytes());
        bytes.extend_from_slice(&(spline.points.len() as u64).to_be_bytes());
        for point in &spline.points {
            append_world_ticks(&mut bytes, point.global_ticks());
        }
    }

    bytes.extend_from_slice(&(inputs.anchors.len() as u64).to_be_bytes());
    for anchor in &inputs.anchors {
        bytes.extend_from_slice(&anchor.layer.to_be_bytes());
    }
    let anchor_points = inputs
        .anchors
        .iter()
        .map(|anchor| anchor.point.clone())
        .collect::<Vec<_>>();
    let anchor_bytes = PlantPointColumns::from_points(anchor_points)?.canonical_bytes()?;
    bytes.extend_from_slice(&(anchor_bytes.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&anchor_bytes);

    bytes.extend_from_slice(&(inputs.plant_prototypes.len() as u64).to_be_bytes());
    for prototype in &inputs.plant_prototypes {
        bytes.extend_from_slice(&prototype.family.value().to_be_bytes());
        for radius in prototype.crown_radius {
            bytes.extend_from_slice(&radius.bits().to_be_bytes());
        }
        for radius in prototype.root_radius {
            bytes.extend_from_slice(&radius.bits().to_be_bytes());
        }
        for bound in prototype.local_bounds_min {
            bytes.extend_from_slice(&bound.bits().to_be_bytes());
        }
        for bound in prototype.local_bounds_max {
            bytes.extend_from_slice(&bound.bits().to_be_bytes());
        }
        bytes.extend_from_slice(&prototype.shade_tolerance.bits().to_be_bytes());
    }

    bytes.extend_from_slice(&(inputs.fields.len() as u64).to_be_bytes());
    for field in &inputs.fields {
        append_field_source(&mut bytes, field.source);
        append_field_channel(&mut bytes, field.channel);
        bytes.push(match field.derivative {
            FieldDerivative::Value => 0,
            FieldDerivative::Gradient => 1,
            FieldDerivative::Hessian => 2,
        });
        bytes.push(match field.blend {
            FieldBlendOperator::Replace => 0,
            FieldBlendOperator::Add => 1,
            FieldBlendOperator::Multiply => 2,
            FieldBlendOperator::Minimum => 3,
            FieldBlendOperator::Maximum => 4,
        });
        bytes.extend_from_slice(&field.weight.bits().to_be_bytes());
        bytes.extend_from_slice(&field.layer_order.0.to_be_bytes());
        bytes.extend_from_slice(&field.layer_order.1.to_be_bytes());
        bytes.extend_from_slice(&field.source_hash);
        append_bounds(&mut bytes, field.bounds);
        for dimension in field.dimensions {
            bytes.extend_from_slice(&dimension.to_be_bytes());
        }
        match &field.values {
            QuantizedFieldTileValues::Scalar(values) => {
                bytes.push(0);
                bytes.extend_from_slice(&(values.len() as u64).to_be_bytes());
                for value in values {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
            QuantizedFieldTileValues::Gradient(values) => {
                bytes.push(1);
                bytes.extend_from_slice(&(values.len() as u64).to_be_bytes());
                for value in values {
                    for lane in value {
                        bytes.extend_from_slice(&lane.to_be_bytes());
                    }
                }
            }
            QuantizedFieldTileValues::Hessian(values) => {
                bytes.push(2);
                bytes.extend_from_slice(&(values.len() as u64).to_be_bytes());
                for value in values {
                    for lane in value {
                        bytes.extend_from_slice(&lane.to_be_bytes());
                    }
                }
            }
        }
    }

    bytes.extend_from_slice(&(inputs.surface_projection_tiles.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&(inputs.surface_field_query_tiles.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&inputs.surface_provider_set_hash);
    Ok(vegetation_content_hash(&bytes))
}

fn append_dependency_source(bytes: &mut Vec<u8>, source: GraphDependencySource) {
    match source {
        GraphDependencySource::Asset(id) => {
            bytes.push(0);
            bytes.extend_from_slice(&id.value().to_be_bytes());
        }
        GraphDependencySource::Field(channel) => {
            bytes.push(1);
            append_field_channel(bytes, channel);
        }
        GraphDependencySource::SurfaceProvider(provider) => {
            bytes.push(2);
            bytes.extend_from_slice(&provider.to_be_bytes());
        }
        GraphDependencySource::MapLayer(layer) => {
            bytes.push(3);
            bytes.extend_from_slice(&layer.to_be_bytes());
        }
    }
}

fn append_field_source(bytes: &mut Vec<u8>, source: EvaluationFieldSource) {
    match source {
        EvaluationFieldSource::MapLayer(layer) => {
            bytes.push(0);
            bytes.extend_from_slice(&layer.to_be_bytes());
        }
        EvaluationFieldSource::SurfaceProvider { provider, revision } => {
            bytes.push(1);
            bytes.extend_from_slice(&provider.0.to_be_bytes());
            bytes.extend_from_slice(&revision.0.to_be_bytes());
        }
    }
}

/// The channel tag, and the user id only for [`FieldChannel::User`] — a built-in channel encodes as
/// its tag alone.
fn append_field_channel(bytes: &mut Vec<u8>, channel: FieldChannel) {
    let (tag, user) = channel.canonical_code();
    bytes.push(tag);
    if matches!(channel, FieldChannel::User(_)) {
        bytes.extend_from_slice(&user.to_be_bytes());
    }
}

fn append_optional_u128(bytes: &mut Vec<u8>, value: Option<u128>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        None => bytes.push(0),
    }
}

fn append_bounds(bytes: &mut Vec<u8>, bounds: WorldBounds) {
    append_world_ticks(bytes, bounds.min_ticks());
    append_world_ticks(bytes, bounds.max_ticks_exclusive());
}

fn append_world_ticks(bytes: &mut Vec<u8>, ticks: [i128; 3]) {
    for tick in ticks {
        bytes.extend_from_slice(&tick.to_be_bytes());
    }
}
