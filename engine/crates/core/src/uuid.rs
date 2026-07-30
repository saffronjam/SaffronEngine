//! The stable 64-bit identity newtype.

use std::cell::RefCell;
use std::fmt;
use std::str::FromStr;

/// Ids below this value are reserved for built-in / synthetic assets (e.g. the
/// default material), so a minted id never collides with a reserved one.
const RESERVED_BELOW: u64 = 1024;

thread_local! {
    static RNG: RefCell<SplitMix64> = RefCell::new(SplitMix64::seeded());
}

/// A stable 64-bit identity, carried by anything serialized because ECS entity
/// values are not stable across runs.
///
/// On the JSON wire a `Uuid` is a **decimal string**, not a number: ids span the
/// full `u64` range past JavaScript's `2^53` safe-integer limit. That encoding is
/// decided in exactly one place, the `serde_with` field attribute in
/// `saffron-protocol`, so this newtype carries no serde derive of its own.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Uuid(pub u64);

impl Uuid {
    /// Mints a fresh id, uniformly drawn from `[1024, u64::MAX]`.
    #[must_use]
    pub fn new() -> Self {
        let raw = RNG.with(|rng| rng.borrow_mut().next());
        let span = u64::MAX - RESERVED_BELOW;
        Self(RESERVED_BELOW + raw % span)
    }

    /// The raw 64-bit value.
    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Uuid {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        s.parse::<u64>().map(Uuid)
    }
}

/// A per-thread SplitMix64 generator for minting ids. Ids need to be unique, not
/// reproducible, so the exact bit-stream carries no contract.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn seeded() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let addr = &nanos as *const u64 as u64;
        Self {
            state: nanos ^ addr.rotate_left(32) ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_respects_reservation() {
        for _ in 0..10_000 {
            assert!(Uuid::new().value() >= RESERVED_BELOW);
        }
    }

    #[test]
    fn two_mints_differ() {
        assert_ne!(Uuid::new(), Uuid::new());
    }

    #[test]
    fn value_round_trips() {
        let id = Uuid(42);
        assert_eq!(id.value(), 42);
        assert_eq!(id, Uuid(42));
    }

    #[test]
    fn decimal_string_round_trip() {
        for id in [Uuid(0), Uuid(1023), Uuid(1024), Uuid(42), Uuid(u64::MAX)] {
            let s = id.to_string();
            assert!(s.chars().all(|c| c.is_ascii_digit()));
            assert_eq!(s.parse::<Uuid>().unwrap(), id);
        }
        assert_eq!(Uuid(u64::MAX).to_string(), "18446744073709551615");
        assert_eq!(
            "18446744073709551615".parse::<Uuid>().unwrap(),
            Uuid(u64::MAX)
        );
    }

    #[test]
    fn usable_as_map_key() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert(Uuid(7), "a");
        map.insert(Uuid(8), "b");
        assert_eq!(map.get(&Uuid(7)), Some(&"a"));
        assert_eq!(map.len(), 2);
    }
}
