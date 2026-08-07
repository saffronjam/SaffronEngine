//! Canonical precomputation of surface projection and field tiles.

use super::*;

use std::sync::Arc;

use saffron_spatial::{
    DecisionHessian3, DecisionScalar, DecisionVec3, FieldChannel, FieldDerivative, SurfaceField,
    SurfaceHit, SurfaceRevision, SurfaceTileDescriptor, UnitInterval, WorldPosition,
};

use crate::hash::VegetationContentHasher;
use crate::{CompiledGraphNode, Error, FieldBlendOperator, Result};

/// Computes the canonical immutable identity of a complete surface-provider set.
pub fn canonical_surface_provider_set_hash(
    providers: &[Arc<dyn SurfaceField>],
    max_providers: u64,
) -> Result<[u8; 32]> {
    canonical_surface_provider_set_hash_guarded(providers, max_providers, None)
}

pub(super) fn canonical_surface_provider_set_hash_guarded(
    providers: &[Arc<dyn SurfaceField>],
    max_providers: u64,
    guard: Option<PreflightGuard<'_>>,
) -> Result<[u8; 32]> {
    if providers.is_empty() {
        return Err(Error::GraphDocument {
            path: "evaluation.surfaceProviders".to_owned(),
            reason: "surface provider set cannot be empty".to_owned(),
        });
    }
    check_limit("input tiles", providers.len() as u64, max_providers)?;
    let mut descriptors = Vec::new();
    crate::memory::reserve_exact(
        &mut descriptors,
        providers.len(),
        "surface provider descriptors",
    )?;
    for provider in providers {
        if let Some(guard) = guard {
            guard.check()?;
        }
        descriptors.push(provider.descriptor());
    }
    descriptors.sort_unstable_by_key(|descriptor| descriptor.id);
    for pair in descriptors.windows(2) {
        if let Some(guard) = guard {
            guard.check()?;
        }
        if pair[0].id == pair[1].id {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceProviders".to_owned(),
                reason: "surface provider identities must be unique".to_owned(),
            });
        }
    }
    let mut hasher = VegetationContentHasher::new();
    hasher.update(b"saffron-anima/surface-provider-set/v1\0")?;
    for descriptor in descriptors {
        if let Some(guard) = guard {
            guard.check()?;
        }
        hasher.update(&descriptor.id.0.to_be_bytes())?;
        hasher.update(&descriptor.revision.0.to_be_bytes())?;
        for value in descriptor.bounds.min_ticks() {
            hasher.update(&value.to_be_bytes())?;
        }
        for value in descriptor.bounds.max_ticks_exclusive() {
            hasher.update(&value.to_be_bytes())?;
        }
        hasher.update(&descriptor.primitive_count.to_be_bytes())?;
        hasher.update(&descriptor.max_tags_per_hit.to_be_bytes())?;
        let capabilities = descriptor.capabilities;
        hasher.update(&[
            u8::from(capabilities.ray),
            u8::from(capabilities.project),
            u8::from(capabilities.nearest),
            u8::from(capabilities.uv),
            u8::from(capabilities.authoritative_attachments),
            u8::from(capabilities.authoritative_fields),
        ])?;
    }
    hasher.finalize()
}

impl QuantizedSurfaceProjectionTile {
    /// Validates exact query ordering, tags, and node identity.
    pub fn validate(&self) -> Result<()> {
        if self.node == 0
            || self.node_semantic_revision == 0
            || self.provider_set_hash == [0; 32]
            || self
                .samples
                .windows(2)
                .any(|pair| pair[0].query >= pair[1].query)
        {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceProjectionTiles".to_owned(),
                reason: "projection tile identity or exact query ordering is invalid".to_owned(),
            });
        }
        for sample in self
            .samples
            .iter()
            .filter_map(|entry| entry.sample.as_ref())
        {
            if sample
                .tags
                .windows(2)
                .any(|pair| pair[0].tag >= pair[1].tag)
            {
                return Err(Error::GraphDocument {
                    path: "evaluation.surfaceProjectionTiles.tags".to_owned(),
                    reason: "projection sample tags must be sorted and unique".to_owned(),
                });
            }
        }
        Ok(())
    }

    pub(super) fn sample(
        &self,
        position: WorldPosition,
    ) -> Option<&Option<QuantizedSurfaceProjectionSample>> {
        let index = self
            .samples
            .binary_search_by_key(&position, |entry| entry.query)
            .ok()?;
        self.samples.get(index).map(|entry| &entry.sample)
    }
}

pub(super) fn quantize_authoritative_surface_hit(
    node: &CompiledGraphNode,
    hit: SurfaceHit,
) -> Result<QuantizedSurfaceProjectionSample> {
    let attachment = hit
        .attachment
        .ok_or_else(|| Error::GraphAuthoritativeInput {
            node: node.definition.guid,
            input: format!("surface attachment for provider {}", hit.provider.0),
        })?;
    let normal = quantize_surface_normal(hit.frame.normal.to_array())?;
    let projection = quantize_surface_projection(hit.coordinates.projection.to_array())?;
    validate_canonical_tags(&hit.tags)?;
    Ok(QuantizedSurfaceProjectionSample {
        position: hit.position,
        attachment,
        normal,
        projection,
        tags: hit.tags,
    })
}

/// Precomputes one canonical typed field tile from a surface provider descriptor.
pub fn precompute_surface_field_tile(
    provider: &dyn SurfaceField,
    descriptor: SurfaceTileDescriptor,
    channel: FieldChannel,
    derivative: FieldDerivative,
    source_hash: [u8; 32],
    cancellation: &GraphCancellationToken,
) -> Result<EvaluationFieldTile> {
    let provider_descriptor = provider.descriptor();
    if !provider_descriptor.capabilities.authoritative_fields
        || provider_descriptor.id != descriptor.provider
        || provider_descriptor.revision != descriptor.revision
        || source_hash == [0; 32]
    {
        return Err(Error::GraphDocument {
            path: "surfaceFieldPrecompute".to_owned(),
            reason: "provider descriptor or canonical content identity does not match".to_owned(),
        });
    }
    let count = packed_sample_count(descriptor.dimensions)?;
    let capacity = usize::try_from(count).map_err(|_| Error::NumericOverflow)?;
    let values = match derivative {
        FieldDerivative::Value => {
            let mut values = Vec::new();
            crate::memory::reserve_exact(&mut values, capacity, "surface scalar field samples")?;
            for index in 0..count {
                if cancellation.is_cancelled() {
                    return Err(Error::GraphCancelled);
                }
                let position =
                    tile_sample_position(descriptor.bounds, descriptor.dimensions, index)?;
                let sample = provider.sample_scalar(channel, derivative, position)?;
                validate_field_sample_identity(
                    sample.channel,
                    sample.derivative,
                    sample.revision,
                    channel,
                    derivative,
                    descriptor.revision,
                )?;
                values.push(sample.value.bits());
            }
            QuantizedFieldTileValues::Scalar(values)
        }
        FieldDerivative::Gradient => {
            let mut values = Vec::new();
            crate::memory::reserve_exact(&mut values, capacity, "surface gradient field samples")?;
            for index in 0..count {
                if cancellation.is_cancelled() {
                    return Err(Error::GraphCancelled);
                }
                let position =
                    tile_sample_position(descriptor.bounds, descriptor.dimensions, index)?;
                let sample = provider.sample_vector(channel, derivative, position)?;
                validate_field_sample_identity(
                    sample.channel,
                    sample.derivative,
                    sample.revision,
                    channel,
                    derivative,
                    descriptor.revision,
                )?;
                values.push([
                    sample.value.x.bits(),
                    sample.value.y.bits(),
                    sample.value.z.bits(),
                ]);
            }
            QuantizedFieldTileValues::Gradient(values)
        }
        FieldDerivative::Hessian => {
            let mut values = Vec::new();
            crate::memory::reserve_exact(&mut values, capacity, "surface Hessian field samples")?;
            for index in 0..count {
                if cancellation.is_cancelled() {
                    return Err(Error::GraphCancelled);
                }
                let position =
                    tile_sample_position(descriptor.bounds, descriptor.dimensions, index)?;
                let sample = provider.sample_hessian(channel, position)?;
                validate_field_sample_identity(
                    sample.channel,
                    sample.derivative,
                    sample.revision,
                    channel,
                    derivative,
                    descriptor.revision,
                )?;
                values.push([
                    sample.value.xx.bits(),
                    sample.value.xy.bits(),
                    sample.value.xz.bits(),
                    sample.value.yy.bits(),
                    sample.value.yz.bits(),
                    sample.value.zz.bits(),
                ]);
            }
            QuantizedFieldTileValues::Hessian(values)
        }
    };
    let tile = EvaluationFieldTile {
        source: EvaluationFieldSource::SurfaceProvider {
            provider: descriptor.provider,
            revision: descriptor.revision,
        },
        channel,
        derivative,
        blend: FieldBlendOperator::Replace,
        weight: UnitInterval::ONE,
        layer_order: (i32::MIN, u128::from(descriptor.provider.0)),
        source_hash,
        bounds: descriptor.bounds,
        dimensions: descriptor.dimensions,
        values,
    };
    tile.validate()?;
    Ok(tile)
}

fn validate_field_sample_identity(
    sample_channel: FieldChannel,
    sample_derivative: FieldDerivative,
    sample_revision: SurfaceRevision,
    expected_channel: FieldChannel,
    expected_derivative: FieldDerivative,
    expected_revision: SurfaceRevision,
) -> Result<()> {
    if sample_channel == expected_channel
        && sample_derivative == expected_derivative
        && sample_revision == expected_revision
    {
        return Ok(());
    }
    Err(Error::GraphDocument {
        path: "surfaceFieldPrecompute".to_owned(),
        reason: "provider returned a mismatched canonical field sample".to_owned(),
    })
}

impl EvaluationFieldTile {
    /// Validates dimensions and packed row count.
    pub fn validate(&self) -> Result<()> {
        let count = self.dimensions.iter().try_fold(1_u64, |product, value| {
            product
                .checked_mul(u64::from(*value))
                .ok_or(Error::NumericOverflow)
        })?;
        let value_count = match &self.values {
            QuantizedFieldTileValues::Scalar(values) => values.len(),
            QuantizedFieldTileValues::Gradient(values) => values.len(),
            QuantizedFieldTileValues::Hessian(values) => values.len(),
        };
        let shape_matches = matches!(
            (&self.values, self.derivative),
            (QuantizedFieldTileValues::Scalar(_), FieldDerivative::Value)
                | (
                    QuantizedFieldTileValues::Gradient(_),
                    FieldDerivative::Gradient
                )
                | (
                    QuantizedFieldTileValues::Hessian(_),
                    FieldDerivative::Hessian
                )
        );
        if count == 0
            || usize::try_from(count).ok() != Some(value_count)
            || !shape_matches
            || self.source_hash == [0; 32]
        {
            return Err(Error::GraphDocument {
                path: "evaluation.fields".to_owned(),
                reason: "tile dimensions do not match packed values".to_owned(),
            });
        }
        Ok(())
    }

    pub(super) fn sample_index(&self, position: WorldPosition) -> Option<usize> {
        if !self.bounds.contains(position) {
            return None;
        }
        let minimum = self.bounds.min_ticks();
        let maximum = self.bounds.max_ticks_exclusive();
        let point = position.global_ticks();
        let mut coordinate = [0_u32; 3];
        for axis in 0..3 {
            let span = maximum[axis] - minimum[axis];
            let offset = point[axis] - minimum[axis];
            let scaled = offset.checked_mul(i128::from(self.dimensions[axis]))?;
            let index = scaled.div_euclid(span);
            coordinate[axis] = u32::try_from(index)
                .ok()?
                .min(self.dimensions[axis].saturating_sub(1));
        }
        let index = u64::from(coordinate[0])
            .checked_mul(u64::from(self.dimensions[1]))?
            .checked_add(u64::from(coordinate[1]))?
            .checked_mul(u64::from(self.dimensions[2]))?
            .checked_add(u64::from(coordinate[2]))?;
        usize::try_from(index).ok()
    }

    pub(super) fn sample_scalar(&self, position: WorldPosition) -> Option<DecisionScalar> {
        let index = self.sample_index(position)?;
        let QuantizedFieldTileValues::Scalar(values) = &self.values else {
            return None;
        };
        values.get(index).copied().map(DecisionScalar::from_bits)
    }

    pub(super) fn sample_vector(&self, position: WorldPosition) -> Option<DecisionVec3> {
        let index = self.sample_index(position)?;
        let QuantizedFieldTileValues::Gradient(values) = &self.values else {
            return None;
        };
        values.get(index).copied().map(|value| DecisionVec3 {
            x: DecisionScalar::from_bits(value[0]),
            y: DecisionScalar::from_bits(value[1]),
            z: DecisionScalar::from_bits(value[2]),
        })
    }

    pub(super) fn sample_hessian(&self, position: WorldPosition) -> Option<DecisionHessian3> {
        let index = self.sample_index(position)?;
        let QuantizedFieldTileValues::Hessian(values) = &self.values else {
            return None;
        };
        values.get(index).copied().map(|value| DecisionHessian3 {
            xx: DecisionScalar::from_bits(value[0]),
            xy: DecisionScalar::from_bits(value[1]),
            xz: DecisionScalar::from_bits(value[2]),
            yy: DecisionScalar::from_bits(value[3]),
            yz: DecisionScalar::from_bits(value[4]),
            zz: DecisionScalar::from_bits(value[5]),
        })
    }
}
