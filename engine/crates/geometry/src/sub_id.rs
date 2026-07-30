//! The stable sub-asset id hash.
//!
//! The asset catalog resolves baked sub-assets by an id derived from the model key, the
//! sub-asset kind, the source name, and a duplicate-disambiguation index. The id must be
//! stable across reimports, since a drifting hash silently orphans every baked sub-asset.
//! The hash is FNV-1a over the three string fields with an extra mix round between them,
//! then a four-byte little-endian mix of `dup_index`, then the fold into `[1024, u64::MAX]`.

use saffron_core::Uuid;

/// The FNV-1a offset basis (64-bit).
const FNV_OFFSET: u64 = 1469598103934665603;
/// The FNV-1a prime (64-bit).
const FNV_PRIME: u64 = 1099511628211;

/// Compute the stable sub-asset id for `(model_key, kind, source_name, dup_index)`.
///
/// The same tuple always yields the same id (so a reimport resolves a baked
/// sub-asset to its prior identity); distinct tuples almost always differ. The
/// result is folded into `[1024, u64::MAX]`, never the reserved range below 1024.
pub fn sub_id_for(model_key: &str, kind: &str, source_name: &str, dup_index: u32) -> Uuid {
    let mut hash = FNV_OFFSET;
    // The extra multiply between fields keeps "ab|c" != "a|bc".
    let mut mix = |part: &str| {
        for ch in part.bytes() {
            hash ^= u64::from(ch);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash = hash.wrapping_mul(FNV_PRIME);
    };
    mix(model_key);
    mix(kind);
    mix(source_name);
    for i in 0..4 {
        hash ^= u64::from((dup_index >> (i * 8)) as u8);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    if hash < 1024 {
        hash += 1024;
    }
    Uuid(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Stability, distinctness across each field, and the `>= 1024` fold.
    #[test]
    fn sub_id_is_stable_distinct_and_folded() {
        let stone = sub_id_for("town", "material", "stone", 0);
        let stone_again = sub_id_for("town", "material", "stone", 0);
        let stone_dup = sub_id_for("town", "material", "stone", 1);
        let stone_mesh = sub_id_for("town", "mesh", "stone", 0);
        let marble = sub_id_for("town", "material", "marble", 0);

        assert_eq!(stone, stone_again, "same tuple must be stable");
        assert_ne!(stone, stone_dup, "dup_index must change the id");
        assert_ne!(stone, stone_mesh, "kind must change the id");
        assert_ne!(stone, marble, "source_name must change the id");
        assert!(
            stone.value() >= 1024,
            "id must fold past the reserved range"
        );
    }

    #[test]
    fn field_boundary_is_not_associative() {
        // The extra mix round between fields makes "ab|c" != "a|bc": concatenating
        // across the field boundary must not collide.
        assert_ne!(sub_id_for("ab", "c", "", 0), sub_id_for("a", "bc", "", 0),);
    }

    #[test]
    fn golden_value_pins_the_hash() {
        // A hardcoded golden pins the exact u64 so any drift in the constants or the
        // mix sequence fails here, catching the silent sub-asset-orphan regression.
        assert_eq!(
            sub_id_for("town", "material", "stone", 0).value(),
            4369703328172768551
        );
    }
}
