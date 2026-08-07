//! Checked vegetation-owned allocation accounting.

use std::collections::BTreeMap;
use std::mem::size_of;

use crate::{Error, Result};

pub(crate) const ALLOCATION_OVERHEAD_BYTES: u64 = 64;
pub(crate) const BTREE_ENTRY_OVERHEAD_BYTES: u64 = 256;

pub(crate) fn requested_vec_bytes<T>(capacity: usize) -> Result<u64> {
    if capacity == 0 {
        return Ok(0);
    }
    u64::try_from(capacity)
        .map_err(|_| Error::NumericOverflow)?
        .checked_mul(u64::try_from(size_of::<T>()).map_err(|_| Error::NumericOverflow)?)
        .and_then(|bytes| bytes.checked_add(ALLOCATION_OVERHEAD_BYTES))
        .ok_or(Error::NumericOverflow)
}

pub(crate) fn requested_vec_bytes_for_len<T>(items: u64) -> Result<u64> {
    if items == 0 {
        return Ok(0);
    }
    items
        .checked_mul(size_of::<T>() as u64)
        .and_then(|bytes| bytes.checked_add(ALLOCATION_OVERHEAD_BYTES))
        .ok_or(Error::NumericOverflow)
}

pub(crate) fn requested_string_bytes(value: &String) -> Result<u64> {
    requested_vec_bytes::<u8>(value.capacity())
}

pub(crate) fn requested_vec_with<T>(
    values: &Vec<T>,
    mut nested: impl FnMut(&T) -> Result<u64>,
) -> Result<u64> {
    values.iter().try_fold(
        requested_vec_bytes::<T>(values.capacity())?,
        |total, value| {
            total
                .checked_add(nested(value)?)
                .ok_or(Error::NumericOverflow)
        },
    )
}

pub(crate) fn requested_btree_bytes<K, V>(entries: usize) -> Result<u64> {
    requested_btree_bytes_for_len::<K, V>(
        u64::try_from(entries).map_err(|_| Error::NumericOverflow)?,
    )
}

pub(crate) fn requested_btree_bytes_for_len<K, V>(entries: u64) -> Result<u64> {
    if entries == 0 {
        return Ok(0);
    }
    let entry_bytes = (size_of::<K>() as u64)
        .checked_add(size_of::<V>() as u64)
        .and_then(|bytes| bytes.checked_add(BTREE_ENTRY_OVERHEAD_BYTES))
        .ok_or(Error::NumericOverflow)?;
    entries
        .checked_mul(entry_bytes)
        .and_then(|bytes| bytes.checked_add(ALLOCATION_OVERHEAD_BYTES))
        .ok_or(Error::NumericOverflow)
}

pub(crate) fn requested_btree_with<K, V>(
    values: &BTreeMap<K, V>,
    mut nested_key: impl FnMut(&K) -> Result<u64>,
    mut nested_value: impl FnMut(&V) -> Result<u64>,
) -> Result<u64> {
    values.iter().try_fold(
        requested_btree_bytes::<K, V>(values.len())?,
        |total, (key, value)| {
            let key_bytes = nested_key(key)?;
            let value_bytes = nested_value(value)?;
            total
                .checked_add(key_bytes)
                .and_then(|bytes| bytes.checked_add(value_bytes))
                .ok_or(Error::NumericOverflow)
        },
    )
}

pub(crate) fn checked_memory_sum(values: impl IntoIterator<Item = u64>) -> Result<u64> {
    values.into_iter().try_fold(0_u64, |total, value| {
        total.checked_add(value).ok_or(Error::NumericOverflow)
    })
}

pub(crate) fn reserve_exact<T>(
    values: &mut Vec<T>,
    additional: usize,
    resource: &'static str,
) -> Result<()> {
    values
        .try_reserve_exact(additional)
        .map_err(|source| Error::MemoryReservation { resource, source })
}
