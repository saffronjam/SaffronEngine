//! The closed tag tables of the authored `.splant`, `.sbiome`, and `.svegmap` documents.

use super::stream::invalid_enum;
use crate::*;

pub(super) fn biome_parameter_type_tag(value: BiomeParameterType) -> u8 {
    match value {
        BiomeParameterType::Scalar => 0,
        BiomeParameterType::Vector => 1,
        BiomeParameterType::Unit => 2,
        BiomeParameterType::Plant => 3,
        BiomeParameterType::Field => 4,
        BiomeParameterType::Boolean => 5,
    }
}

pub(super) fn biome_parameter_type(value: u8) -> Result<BiomeParameterType> {
    match value {
        0 => Ok(BiomeParameterType::Scalar),
        1 => Ok(BiomeParameterType::Vector),
        2 => Ok(BiomeParameterType::Unit),
        3 => Ok(BiomeParameterType::Plant),
        4 => Ok(BiomeParameterType::Field),
        5 => Ok(BiomeParameterType::Boolean),
        _ => Err(invalid_enum(".sbiome", "parameters.type")),
    }
}

pub(super) fn field_blend_tag(value: FieldBlendOperator) -> u8 {
    match value {
        FieldBlendOperator::Replace => 0,
        FieldBlendOperator::Add => 1,
        FieldBlendOperator::Multiply => 2,
        FieldBlendOperator::Minimum => 3,
        FieldBlendOperator::Maximum => 4,
    }
}

pub(super) fn field_blend(value: u8) -> Result<FieldBlendOperator> {
    match value {
        0 => Ok(FieldBlendOperator::Replace),
        1 => Ok(FieldBlendOperator::Add),
        2 => Ok(FieldBlendOperator::Multiply),
        3 => Ok(FieldBlendOperator::Minimum),
        4 => Ok(FieldBlendOperator::Maximum),
        _ => Err(invalid_enum(".svegmap", "layers.blend")),
    }
}

pub(super) fn inclusion_tag(value: InclusionOperator) -> u8 {
    match value {
        InclusionOperator::Include => 0,
        InclusionOperator::Exclude => 1,
    }
}

pub(super) fn inclusion(value: u8) -> Result<InclusionOperator> {
    match value {
        0 => Ok(InclusionOperator::Include),
        1 => Ok(InclusionOperator::Exclude),
        _ => Err(invalid_enum(".svegmap", "layers.inclusion")),
    }
}

pub(super) fn provenance_outcome_tag(value: ProvenanceDecisionOutcome) -> u8 {
    match value {
        ProvenanceDecisionOutcome::Produced => 0,
        ProvenanceDecisionOutcome::Retained => 1,
        ProvenanceDecisionOutcome::Accepted => 2,
        ProvenanceDecisionOutcome::Rejected => 3,
    }
}

pub(super) fn provenance_outcome(value: u8) -> Result<ProvenanceDecisionOutcome> {
    match value {
        0 => Ok(ProvenanceDecisionOutcome::Produced),
        1 => Ok(ProvenanceDecisionOutcome::Retained),
        2 => Ok(ProvenanceDecisionOutcome::Accepted),
        3 => Ok(ProvenanceDecisionOutcome::Rejected),
        _ => Err(invalid_enum(
            ".svegmap chunk",
            "provenance.decision.outcome",
        )),
    }
}

pub(super) fn source_units_tag(value: SourceUnits) -> u8 {
    match value {
        SourceUnits::Meters => 0,
        SourceUnits::Centimeters => 1,
        SourceUnits::Millimeters => 2,
        SourceUnits::Feet => 3,
    }
}

pub(super) fn source_units(value: u8) -> Result<SourceUnits> {
    match value {
        0 => Ok(SourceUnits::Meters),
        1 => Ok(SourceUnits::Centimeters),
        2 => Ok(SourceUnits::Millimeters),
        3 => Ok(SourceUnits::Feet),
        _ => Err(invalid_enum(".splant", "source.units")),
    }
}

pub(super) fn source_axis_tag(value: SourceAxis) -> u8 {
    match value {
        SourceAxis::PositiveX => 0,
        SourceAxis::NegativeX => 1,
        SourceAxis::PositiveY => 2,
        SourceAxis::NegativeY => 3,
        SourceAxis::PositiveZ => 4,
        SourceAxis::NegativeZ => 5,
    }
}

pub(super) fn source_axis(value: u8) -> Result<SourceAxis> {
    match value {
        0 => Ok(SourceAxis::PositiveX),
        1 => Ok(SourceAxis::NegativeX),
        2 => Ok(SourceAxis::PositiveY),
        3 => Ok(SourceAxis::NegativeY),
        4 => Ok(SourceAxis::PositiveZ),
        5 => Ok(SourceAxis::NegativeZ),
        _ => Err(invalid_enum(".splant", "source.axis")),
    }
}

pub(super) fn source_role_tag(value: PlantSourceRole) -> u8 {
    match value {
        PlantSourceRole::Geometry => 0,
        PlantSourceRole::Material => 1,
        PlantSourceRole::Skeleton => 2,
        PlantSourceRole::Collision => 3,
        PlantSourceRole::Navigation => 4,
    }
}

pub(super) fn source_role(value: u8) -> Result<PlantSourceRole> {
    match value {
        0 => Ok(PlantSourceRole::Geometry),
        1 => Ok(PlantSourceRole::Material),
        2 => Ok(PlantSourceRole::Skeleton),
        3 => Ok(PlantSourceRole::Collision),
        4 => Ok(PlantSourceRole::Navigation),
        _ => Err(invalid_enum(".splant", "source.role")),
    }
}

pub(super) fn source_handedness_tag(value: SourceHandedness) -> u8 {
    match value {
        SourceHandedness::Right => 0,
        SourceHandedness::Left => 1,
    }
}

pub(super) fn source_handedness(value: u8) -> Result<SourceHandedness> {
    match value {
        0 => Ok(SourceHandedness::Right),
        1 => Ok(SourceHandedness::Left),
        _ => Err(invalid_enum(".splant", "source.settings.handedness")),
    }
}

pub(super) fn source_winding_tag(value: SourceWinding) -> u8 {
    match value {
        SourceWinding::CounterClockwise => 0,
        SourceWinding::Clockwise => 1,
    }
}

pub(super) fn source_winding(value: u8) -> Result<SourceWinding> {
    match value {
        0 => Ok(SourceWinding::CounterClockwise),
        1 => Ok(SourceWinding::Clockwise),
        _ => Err(invalid_enum(".splant", "source.settings.winding")),
    }
}

pub(super) fn source_uv_origin_tag(value: SourceUvOrigin) -> u8 {
    match value {
        SourceUvOrigin::TopLeft => 0,
        SourceUvOrigin::BottomLeft => 1,
    }
}

pub(super) fn source_uv_origin(value: u8) -> Result<SourceUvOrigin> {
    match value {
        0 => Ok(SourceUvOrigin::TopLeft),
        1 => Ok(SourceUvOrigin::BottomLeft),
        _ => Err(invalid_enum(".splant", "source.settings.uvOrigin")),
    }
}

pub(super) fn tangent_policy_tag(value: PlantTangentPolicy) -> u8 {
    match value {
        PlantTangentPolicy::Require => 0,
        PlantTangentPolicy::GenerateMissing => 1,
        PlantTangentPolicy::Regenerate => 2,
    }
}

pub(super) fn tangent_policy(value: u8) -> Result<PlantTangentPolicy> {
    match value {
        0 => Ok(PlantTangentPolicy::Require),
        1 => Ok(PlantTangentPolicy::GenerateMissing),
        2 => Ok(PlantTangentPolicy::Regenerate),
        _ => Err(invalid_enum(".splant", "source.settings.tangentPolicy")),
    }
}

pub(super) fn plant_part_semantic_tag(value: PlantPartSemantic) -> u8 {
    match value {
        PlantPartSemantic::Trunk => 0,
        PlantPartSemantic::Branch => 1,
        PlantPartSemantic::Root => 2,
        PlantPartSemantic::Frond => 3,
        PlantPartSemantic::Leaf => 4,
        PlantPartSemantic::Flower => 5,
        PlantPartSemantic::Fruit => 6,
        PlantPartSemantic::Blade => 7,
    }
}

pub(super) fn plant_part_semantic(value: u8) -> Result<PlantPartSemantic> {
    match value {
        0 => Ok(PlantPartSemantic::Trunk),
        1 => Ok(PlantPartSemantic::Branch),
        2 => Ok(PlantPartSemantic::Root),
        3 => Ok(PlantPartSemantic::Frond),
        4 => Ok(PlantPartSemantic::Leaf),
        5 => Ok(PlantPartSemantic::Flower),
        6 => Ok(PlantPartSemantic::Fruit),
        7 => Ok(PlantPartSemantic::Blade),
        _ => Err(invalid_enum(".splant", "parts.semantic")),
    }
}

pub(super) fn phenotype_role_tag(value: PhenotypeRole) -> u8 {
    match value {
        PhenotypeRole::Healthy => 0,
        PhenotypeRole::Harvested => 1,
        PhenotypeRole::Damaged => 2,
        PhenotypeRole::Burned => 3,
        PhenotypeRole::Dead => 4,
        PhenotypeRole::Flowering => 5,
        PhenotypeRole::Fruiting => 6,
        PhenotypeRole::Senescent => 7,
        PhenotypeRole::Wet => 8,
    }
}

pub(super) fn phenotype_role(value: u8) -> Result<PhenotypeRole> {
    match value {
        0 => Ok(PhenotypeRole::Healthy),
        1 => Ok(PhenotypeRole::Harvested),
        2 => Ok(PhenotypeRole::Damaged),
        3 => Ok(PhenotypeRole::Burned),
        4 => Ok(PhenotypeRole::Dead),
        5 => Ok(PhenotypeRole::Flowering),
        6 => Ok(PhenotypeRole::Fruiting),
        7 => Ok(PhenotypeRole::Senescent),
        8 => Ok(PhenotypeRole::Wet),
        _ => Err(invalid_enum(".splant", "phenotypes.role")),
    }
}

pub(super) fn collision_shape_tag(value: PlantCollisionShape) -> u8 {
    match value {
        PlantCollisionShape::Box => 0,
        PlantCollisionShape::Sphere => 1,
        PlantCollisionShape::Capsule => 2,
        PlantCollisionShape::ConvexHull => 3,
    }
}

pub(super) fn collision_shape(value: u8) -> Result<PlantCollisionShape> {
    match value {
        0 => Ok(PlantCollisionShape::Box),
        1 => Ok(PlantCollisionShape::Sphere),
        2 => Ok(PlantCollisionShape::Capsule),
        3 => Ok(PlantCollisionShape::ConvexHull),
        _ => Err(invalid_enum(".splant", "collision.shape")),
    }
}
