//! The value primitives the state and save payloads share.

use saffron_spatial::{DecisionScalar, FieldChannel, UnitInterval, WorldPosition};

use crate::binary::{BinaryReader, BinaryWriter, field_channel_from_tag, field_channel_tag};
use crate::{
    Error, PlantId, PlantPoint, PlantPointColumns, PromotionOriginState, QuantizedOrientation,
    Result,
};

pub(super) fn encode_point(writer: &mut BinaryWriter, point: &PlantPoint) -> Result<()> {
    point.validate()?;
    let bytes = PlantPointColumns::from_points(vec![point.clone()])?.canonical_bytes()?;
    writer.length(bytes.len())?;
    writer.bytes(&bytes);
    Ok(())
}

pub(super) fn decode_point(
    reader: &mut BinaryReader<'_>,
    format: &'static str,
) -> Result<PlantPoint> {
    let length = reader.length()?;
    let columns = PlantPointColumns::from_canonical_bytes(reader.take(length)?)?;
    if columns.row_count()? != 1 {
        return Err(Error::ArtifactFormat {
            format,
            field: "point.rows".to_owned(),
        });
    }
    columns.point(0)
}

pub(super) fn encode_optional_point(
    writer: &mut BinaryWriter,
    point: Option<&PlantPoint>,
) -> Result<()> {
    writer.bool(point.is_some());
    if let Some(point) = point {
        encode_point(writer, point)?;
    }
    Ok(())
}

pub(super) fn decode_optional_point(
    reader: &mut BinaryReader<'_>,
    format: &'static str,
) -> Result<Option<PlantPoint>> {
    if reader.bool()? {
        Ok(Some(decode_point(reader, format)?))
    } else {
        Ok(None)
    }
}

pub(super) fn encode_plant_id(writer: &mut BinaryWriter, plant: PlantId) {
    writer.bytes(&plant.bytes());
}

pub(super) fn decode_plant_id(reader: &mut BinaryReader<'_>) -> Result<PlantId> {
    PlantId::from_bytes(reader.array()?).map_err(|_| Error::InvalidPlantId)
}

pub(super) fn encode_position(writer: &mut BinaryWriter, position: WorldPosition) {
    for tick in position.global_ticks() {
        writer.i128(tick);
    }
}

pub(super) fn decode_position(reader: &mut BinaryReader<'_>) -> Result<WorldPosition> {
    WorldPosition::from_global_ticks([reader.i128()?, reader.i128()?, reader.i128()?])
        .map_err(Into::into)
}

pub(super) fn encode_orientation(writer: &mut BinaryWriter, orientation: QuantizedOrientation) {
    for lane in orientation.bits() {
        writer.u16(lane as u16);
    }
}

pub(super) fn decode_orientation(reader: &mut BinaryReader<'_>) -> Result<QuantizedOrientation> {
    Ok(QuantizedOrientation::new([
        reader.u16()? as i16,
        reader.u16()? as i16,
        reader.u16()? as i16,
        reader.u16()? as i16,
    ])?)
}

pub(super) fn encode_vec3(writer: &mut BinaryWriter, values: [DecisionScalar; 3]) {
    for value in values {
        writer.i32(value.bits());
    }
}

pub(super) fn decode_vec3(reader: &mut BinaryReader<'_>) -> Result<[DecisionScalar; 3]> {
    Ok([
        DecisionScalar::from_bits(reader.i32()?),
        DecisionScalar::from_bits(reader.i32()?),
        DecisionScalar::from_bits(reader.i32()?),
    ])
}

pub(super) fn encode_promotion(writer: &mut BinaryWriter, state: PromotionOriginState) {
    encode_position(writer, state.position);
    encode_orientation(writer, state.orientation);
    encode_vec3(writer, state.scale);
    encode_vec3(writer, state.linear_velocity);
    encode_vec3(writer, state.angular_velocity);
}

pub(super) fn decode_promotion(reader: &mut BinaryReader<'_>) -> Result<PromotionOriginState> {
    Ok(PromotionOriginState {
        position: decode_position(reader)?,
        orientation: decode_orientation(reader)?,
        scale: decode_vec3(reader)?,
        linear_velocity: decode_vec3(reader)?,
        angular_velocity: decode_vec3(reader)?,
    })
}

pub(super) fn encode_field_channel(writer: &mut BinaryWriter, channel: FieldChannel) {
    let (tag, user) = field_channel_tag(channel);
    writer.u8(tag);
    writer.u64(user);
}

pub(super) fn decode_field_channel(
    reader: &mut BinaryReader<'_>,
    format: &'static str,
) -> Result<FieldChannel> {
    let tag = reader.u8()?;
    let user = reader.u64()?;
    field_channel_from_tag(tag, user).ok_or_else(|| Error::ArtifactFormat {
        format,
        field: "fieldChannel".to_owned(),
    })
}

pub(super) fn read_i32_values(reader: &mut BinaryReader<'_>) -> Result<Vec<i32>> {
    let count = reader.count(4)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(reader.i32()?);
    }
    Ok(values)
}

pub(super) fn encode_option_u32(writer: &mut BinaryWriter, value: Option<u32>) {
    writer.bool(value.is_some());
    if let Some(value) = value {
        writer.u32(value);
    }
}

pub(super) fn decode_option_u32(reader: &mut BinaryReader<'_>) -> Result<Option<u32>> {
    if reader.bool()? {
        Ok(Some(reader.u32()?))
    } else {
        Ok(None)
    }
}

pub(super) fn encode_option_u64(writer: &mut BinaryWriter, value: Option<u64>) {
    writer.bool(value.is_some());
    if let Some(value) = value {
        writer.u64(value);
    }
}

pub(super) fn decode_option_u64(reader: &mut BinaryReader<'_>) -> Result<Option<u64>> {
    if reader.bool()? {
        Ok(Some(reader.u64()?))
    } else {
        Ok(None)
    }
}

pub(super) fn encode_option_unit(writer: &mut BinaryWriter, value: Option<UnitInterval>) {
    writer.bool(value.is_some());
    if let Some(value) = value {
        writer.u16(value.bits());
    }
}

pub(super) fn decode_option_unit(reader: &mut BinaryReader<'_>) -> Result<Option<UnitInterval>> {
    if reader.bool()? {
        Ok(Some(UnitInterval::from_bits(reader.u16()?)))
    } else {
        Ok(None)
    }
}

pub(super) fn non_canonical<T>(format: &'static str, field: &str) -> Result<T> {
    Err(Error::ArtifactFormat {
        format,
        field: field.to_owned(),
    })
}
