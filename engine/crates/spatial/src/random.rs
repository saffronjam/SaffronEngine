//! Pinned Philox counter randomness and its domain-separated key vocabulary.

use crate::WorldCellKey;

const PHILOX_M0: u32 = 0xD251_1F53;
const PHILOX_M1: u32 = 0xCD9E_8D57;
const PHILOX_W0: u32 = 0x9E37_79B9;
const PHILOX_W1: u32 = 0xBB67_AE85;
const DOMAIN_COUNTER: [u32; 4] = [0x5350_4154, 0x4941_4C31, 0xA511_E9B3, 0x63D8_35F1];
const DOMAIN_KEY: [u32; 2] = [0xC0DE_CAFE, 0x9E37_79B9];

/// The published Random123 Philox4x32-10 zero-counter/zero-key vector.
pub const PHILOX4X32_ZERO_VECTOR: [u32; 4] = [0x6627_E8D5, 0xE169_C58D, 0xBC57_AC4C, 0x9B00_DBD8];

/// Evaluates Philox4x32-10 with the Random123 constants and round schedule.
#[must_use]
pub fn philox4x32_10(mut counter: [u32; 4], mut key: [u32; 2]) -> [u32; 4] {
    for round in 0..10 {
        let product0 = u64::from(PHILOX_M0) * u64::from(counter[0]);
        let product1 = u64::from(PHILOX_M1) * u64::from(counter[2]);
        counter = [
            (product1 >> 32) as u32 ^ counter[1] ^ key[0],
            product1 as u32,
            (product0 >> 32) as u32 ^ counter[3] ^ key[1],
            product0 as u32,
        ];
        if round != 9 {
            key[0] = key[0].wrapping_add(PHILOX_W0);
            key[1] = key[1].wrapping_add(PHILOX_W1);
        }
    }
    counter
}

/// Every stable identity that may select an authoritative random stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RandomDomain {
    /// Vegetation-map or authored-domain identity.
    pub map: u128,
    /// Stable graph-node identity.
    pub node_guid: u128,
    /// Node semantic revision, changed only when the node's meaning changes.
    pub node_semantic_revision: u32,
    /// Full-width named seed namespace for this stochastic decision.
    pub seed_namespace: u128,
    /// Canonical owner cell.
    pub cell: WorldCellKey,
    /// Candidate identity within the node and cell.
    pub candidate: u64,
    /// Ancestor candidate identity used by hierarchical refinement.
    pub ancestor: u64,
    /// Species identity.
    pub species: u128,
    /// Named random channel within the node.
    pub channel: u32,
}

impl RandomDomain {
    fn canonical_words(self) -> Vec<u32> {
        let mut words = Vec::with_capacity(25);
        append_u128(&mut words, self.map);
        append_u128(&mut words, self.node_guid);
        words.push(self.node_semantic_revision);
        append_u128(&mut words, self.seed_namespace);
        words.push(u32::from(self.cell.level()));
        for coordinate in self.cell.coordinates() {
            append_u64(&mut words, zigzag_i64(coordinate));
        }
        append_u64(&mut words, self.candidate);
        append_u64(&mut words, self.ancestor);
        append_u128(&mut words, self.species);
        words.push(self.channel);
        words
    }
}

/// A domain-separated Philox stream. Samples are random-access and independent of dispatch order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RandomStream {
    counter: [u32; 4],
    key: [u32; 2],
}

impl RandomStream {
    /// Folds the complete canonical vocabulary into a pinned base counter and key.
    #[must_use]
    pub fn new(domain: RandomDomain) -> Self {
        let mut counter = DOMAIN_COUNTER;
        let mut key = DOMAIN_KEY;
        for (index, word) in domain.canonical_words().into_iter().enumerate() {
            let lane = index & 3;
            let neighbour = (lane + 1) & 3;
            counter[lane] ^= word;
            counter[neighbour] =
                counter[neighbour].wrapping_add(word.rotate_left(((index * 7 + 5) & 31) as u32));
            key[index & 1] =
                key[index & 1].wrapping_add(word ^ (index as u32).wrapping_mul(PHILOX_W0));
            counter = philox4x32_10(counter, key);
        }
        Self { counter, key }
    }

    /// Four random lanes for a random-access sample index.
    #[must_use]
    pub fn sample(self, sample_index: u64) -> [u32; 4] {
        let low = sample_index as u32;
        let high = (sample_index >> 32) as u32;
        let (counter0, carry) = self.counter[0].overflowing_add(low);
        let counter1 = self.counter[1]
            .wrapping_add(high)
            .wrapping_add(u32::from(carry));
        philox4x32_10(
            [counter0, counter1, self.counter[2], self.counter[3]],
            self.key,
        )
    }

    /// One lane from a random-access sample.
    #[must_use]
    pub fn lane(self, sample_index: u64, lane: usize) -> u32 {
        self.sample(sample_index)[lane & 3]
    }

    /// A canonical unit interval from the high 16 bits of one lane.
    #[must_use]
    pub fn unit(self, sample_index: u64, lane: usize) -> crate::UnitInterval {
        crate::UnitInterval::from_bits((self.lane(sample_index, lane) >> 16) as u16)
    }

    /// Accepts a probability against the full 32-bit random lane.
    #[must_use]
    pub fn chance(self, sample_index: u64, lane: usize, probability: crate::UnitInterval) -> bool {
        chance_from_draw(self.lane(sample_index, lane), probability)
    }
}

const fn chance_from_draw(draw: u32, probability: crate::UnitInterval) -> bool {
    draw as u64 * (u16::MAX as u64) < probability.bits() as u64 * (u32::MAX as u64 + 1)
}

fn append_u64(words: &mut Vec<u32>, value: u64) {
    words.push(value as u32);
    words.push((value >> 32) as u32);
}

fn append_u128(words: &mut Vec<u32>, value: u128) {
    append_u64(words, value as u64);
    append_u64(words, (value >> 64) as u64);
}

const fn zigzag_i64(value: i64) -> u64 {
    ((value as u64) << 1) ^ ((value >> 63) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain() -> RandomDomain {
        RandomDomain {
            map: 0x0123_4567_89AB_CDEF_0011_2233_4455_6677,
            node_guid: 0x8877_6655_4433_2211_FEDC_BA98_7654_3210,
            node_semantic_revision: 9,
            seed_namespace: 0xCAFE_BABE_1020_3040_5060_7080_90A0_B0C0,
            cell: WorldCellKey::new(-5, 7, -11, 3).unwrap(),
            candidate: 123_456,
            ancestor: 789,
            species: 0xDEAD_BEEF_CAFE_BABE_1122_3344_5566_7788,
            channel: 4,
        }
    }

    #[test]
    fn published_zero_vector_matches_random123() {
        assert_eq!(philox4x32_10([0; 4], [0; 2]), PHILOX4X32_ZERO_VECTOR);
    }

    #[test]
    fn domain_golden_is_pinned() {
        let stream = RandomStream::new(domain());
        assert_eq!(
            stream.sample(0),
            [0xB3FD_6963, 0x85E7_79D7, 0x43A8_55FE, 0xAC3A_616F]
        );
        assert_eq!(
            stream.sample(u64::MAX),
            [0x5D40_A1AD, 0x8702_699B, 0x7399_9997, 0x2C2B_B64A]
        );
    }

    #[test]
    fn chance_endpoints_are_exact_for_every_draw() {
        for draw in [0, 1, u32::MAX - 1, u32::MAX] {
            assert!(!chance_from_draw(draw, crate::UnitInterval::ZERO));
            assert!(chance_from_draw(draw, crate::UnitInterval::ONE));
        }
    }

    #[test]
    fn chance_uses_the_full_u32_draw() {
        let probability = crate::UnitInterval::from_bits(32_768);
        assert!(chance_from_draw(0x8000_0000, probability));
        assert!(!chance_from_draw(0x8001_0002, probability));
    }

    #[test]
    fn stream_chance_is_pinned_to_the_domain_golden() {
        let stream = RandomStream::new(domain());
        assert!(!stream.chance(0, 0, crate::UnitInterval::from_bits(46_076)));
        assert!(stream.chance(0, 0, crate::UnitInterval::from_bits(46_077)));
    }

    #[test]
    fn each_vocabulary_field_separates_the_stream() {
        let base = domain();
        let expected = RandomStream::new(base).sample(17);
        let variants = [
            RandomDomain {
                map: base.map + 1,
                ..base
            },
            RandomDomain {
                node_guid: base.node_guid + 1,
                ..base
            },
            RandomDomain {
                node_semantic_revision: base.node_semantic_revision + 1,
                ..base
            },
            RandomDomain {
                seed_namespace: base.seed_namespace + 1,
                ..base
            },
            RandomDomain {
                cell: base.cell.neighbour([1, 0, 0]).unwrap(),
                ..base
            },
            RandomDomain {
                candidate: base.candidate + 1,
                ..base
            },
            RandomDomain {
                ancestor: base.ancestor + 1,
                ..base
            },
            RandomDomain {
                species: base.species + 1,
                ..base
            },
            RandomDomain {
                channel: base.channel + 1,
                ..base
            },
        ];
        for variant in variants {
            assert_ne!(RandomStream::new(variant).sample(17), expected);
        }
    }
}
