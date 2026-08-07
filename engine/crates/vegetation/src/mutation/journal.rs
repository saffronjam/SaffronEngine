//! The editor journal envelope: one gesture, the records it applies, and their exact inverse.

use std::collections::BTreeSet;

use saffron_spatial::WorldCellKey;

use crate::hash::sha256;
use crate::{Error, PlantId, Result};

use super::encode::transaction_signature;
use super::{
    DisturbanceTileKey, FieldTileKey, MutationHeader, VegetationMutation, VegetationMutationRecord,
    VegetationState,
};

/// One editor gesture over the shared reducer: the records it applies and the records that undo it.
///
/// Retention is what separates this envelope from the other two. A save keeps a snapshot plus a
/// tail it may fold away whenever it likes, and a network stream keeps a sequence window it may
/// drop once acknowledged; a journal entry keeps both directions verbatim for as long as the
/// gesture sits on an undo stack, and never reaches durable storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorJournalEnvelope {
    /// Editor gesture identity: the grouping every record here shares.
    pub gesture: u128,
    /// Forward records passed to the reducer.
    pub forward: Vec<VegetationMutationRecord>,
    /// One atomic transaction returning every address the gesture touched to its preimage.
    pub inverse: Vec<VegetationMutationRecord>,
}

impl EditorJournalEnvelope {
    /// Captures the preimage of every address `forward` touches and derives the exact inverse.
    ///
    /// Call before the forward records reduce: the inverse is read from the state they are about
    /// to change. The inverse is absolute rather than incremental — one record per touched
    /// address, restoring the whole value — so it commutes and applies once as a single
    /// transaction whatever order the gesture wrote in.
    ///
    /// # Errors
    ///
    /// [`Error::NumericOverflow`] when the gesture exceeds the canonical record bound.
    pub fn capture(
        state: &VegetationState,
        gesture: u128,
        forward: Vec<VegetationMutationRecord>,
    ) -> Result<Self> {
        let Some(first) = forward.first() else {
            return Ok(Self {
                gesture,
                forward,
                inverse: Vec::new(),
            });
        };
        let authority = first.header.authority;
        let logical_tick = forward
            .iter()
            .map(|record| record.header.logical_tick)
            .max()
            .unwrap_or_default();
        let borrowed = forward.iter().collect::<Vec<_>>();
        let signature = transaction_signature(&borrowed)?;
        let transaction = derive_id(
            b"saffron-anima/vegetation-journal-transaction/v1\0",
            gesture,
            &signature,
            0,
        );
        let addresses = forward.iter().map(address).collect::<BTreeSet<_>>();
        let inverse = addresses
            .into_iter()
            .enumerate()
            .map(|(index, address)| {
                let index = u64::try_from(index).map_err(|_| Error::NumericOverflow)?;
                Ok(VegetationMutationRecord {
                    header: MutationHeader {
                        cell: address.cell(),
                        transaction,
                        authority,
                        logical_tick,
                        idempotency_key: derive_id(
                            b"saffron-anima/vegetation-journal-operation/v1\0",
                            gesture,
                            &signature,
                            index,
                        ),
                        base_revision: None,
                    },
                    mutation: inverse_mutation(state, address),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            gesture,
            forward,
            inverse,
        })
    }
}

/// A persistent address a gesture touched: the granularity a preimage is captured at.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Address {
    Plant(WorldCellKey, PlantId),
    Field(WorldCellKey, FieldTileKey),
    Disturbance(WorldCellKey, DisturbanceTileKey),
}

impl Address {
    const fn cell(self) -> WorldCellKey {
        match self {
            Self::Plant(cell, _) | Self::Field(cell, _) | Self::Disturbance(cell, _) => cell,
        }
    }
}

fn address(record: &VegetationMutationRecord) -> Address {
    let cell = record.header.cell;
    match &record.mutation {
        VegetationMutation::FieldTilePatch {
            layer,
            channel,
            tile,
            ..
        }
        | VegetationMutation::FieldTileClear {
            layer,
            channel,
            tile,
        } => Address::Field(
            cell,
            FieldTileKey {
                layer: *layer,
                channel: *channel,
                tile: *tile,
            },
        ),
        VegetationMutation::DisturbanceMask {
            categories, tile, ..
        }
        | VegetationMutation::DisturbanceMaskClear { categories, tile } => Address::Disturbance(
            cell,
            DisturbanceTileKey {
                categories: *categories,
                tile: *tile,
            },
        ),
        VegetationMutation::AnchorAddition(point) | VegetationMutation::Planting(point) => {
            Address::Plant(cell, point.id)
        }
        VegetationMutation::Tombstone { plant }
        | VegetationMutation::TransformOverride { plant, .. }
        | VegetationMutation::StateOverride { plant, .. }
        | VegetationMutation::Damage { plant, .. }
        | VegetationMutation::MoistureFuel { plant, .. }
        | VegetationMutation::LifecycleTransition { plant, .. }
        | VegetationMutation::Harvest { plant, .. }
        | VegetationMutation::Burn { plant, .. }
        | VegetationMutation::Ignite { plant }
        | VegetationMutation::Extinguish { plant }
        | VegetationMutation::Regrow { plant, .. }
        | VegetationMutation::PromotionOriginState { plant, .. }
        | VegetationMutation::PlantDeltaRestore { plant, .. } => Address::Plant(cell, *plant),
    }
}

fn inverse_mutation(state: &VegetationState, address: Address) -> VegetationMutation {
    let cell = state.cells.get(&address.cell());
    match address {
        Address::Plant(_, plant) => VegetationMutation::PlantDeltaRestore {
            plant,
            delta: cell
                .and_then(|cell| cell.plants.get(&plant))
                .cloned()
                .map(Box::new),
        },
        Address::Field(_, key) => match cell.and_then(|cell| cell.field_tiles.get(&key)) {
            Some(tile) => VegetationMutation::FieldTilePatch {
                layer: key.layer,
                channel: key.channel,
                tile: key.tile,
                dimensions: tile.dimensions,
                quantum_bits: tile.quantum_bits,
                values: tile.values.clone(),
            },
            None => VegetationMutation::FieldTileClear {
                layer: key.layer,
                channel: key.channel,
                tile: key.tile,
            },
        },
        Address::Disturbance(_, key) => {
            match cell.and_then(|cell| cell.disturbance_masks.get(&key)) {
                Some(values) => VegetationMutation::DisturbanceMask {
                    categories: key.categories,
                    tile: key.tile,
                    values: values.clone(),
                },
                None => VegetationMutation::DisturbanceMaskClear {
                    categories: key.categories,
                    tile: key.tile,
                },
            }
        }
    }
}

/// A domain-separated derived id. The reducer rejects a zero transaction or idempotency key, so
/// the top bit is set and the derivation can never produce one.
fn derive_id(domain: &[u8], gesture: u128, signature: &[u8; 32], index: u64) -> u128 {
    let mut preimage = domain.to_vec();
    preimage.extend_from_slice(&gesture.to_be_bytes());
    preimage.extend_from_slice(signature);
    preimage.extend_from_slice(&index.to_be_bytes());
    let digest = sha256(&preimage);
    let mut lanes = [0_u8; 16];
    lanes.copy_from_slice(&digest[..16]);
    u128::from_be_bytes(lanes) | (1 << 127)
}
