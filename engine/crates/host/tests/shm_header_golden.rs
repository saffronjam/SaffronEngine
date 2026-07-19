//! The shm-ABI header golden snapshot: the *static* half of the shm-ABI gate, paired with the
//! live byte-exact reader-oracle gate in `shm_abi_gate.rs`.
//!
//! The frame transport's 32-byte header is `[magic, width, height, seq, ringSlots,
//! slotCapacity, generationLo, generationHi]` (eight native-endian `u32` words), written at
//! segment creation with width/height/seq = 0 and the capacity floored at 4K RGBA. A change to any of those
//! words — a magic, a ring depth, a header size — is an ABI break the editor reader cannot
//! tolerate, and it never throws. The detector is a golden header layout in
//! `fixtures/golden/gen/`.
//!
//! This test rebuilds the header from the Rust constants, renders the same
//! `word N <field> <value>` / `hexdump:` text the generator emits, matches it byte-for-byte
//! against `shm_header.layout`, and additionally asserts the *live* `ViewportShmPublisher`
//! segment carries those exact header bytes at startup — the static contract and the real
//! producer agreeing on the same bytes.

use saffron_host::{ShmView, ShmViewConfig, ViewportShmPublisher};
use saffron_rendering::{MIN_SHM_SLOT_CAPACITY, SHM_HEADER_BYTES, SHM_MAGIC, SHM_RING_SLOTS};
use saffron_test_support::unique_shm_name;

const GOLDEN_GENERATION: u64 = 0x0123_4567_89ab_cdef;

/// The 32-byte header words at segment creation, exactly as `recreate_segment` writes them:
/// magic, then width/height/seq = 0 (no frame yet), ring depth, capacity, and generation.
fn startup_header_words() -> [u32; 8] {
    [
        SHM_MAGIC,
        0,
        0,
        0,
        SHM_RING_SLOTS,
        MIN_SHM_SLOT_CAPACITY as u32,
        GOLDEN_GENERATION as u32,
        (GOLDEN_GENERATION >> 32) as u32,
    ]
}

fn header_bytes(words: &[u32; 8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SHM_HEADER_BYTES);
    for word in words {
        bytes.extend_from_slice(&word.to_ne_bytes());
    }
    bytes
}

/// The generator's `hexdumpBytes` shape: space-separated hex bytes, 16 per row, with a
/// trailing newline.
fn hexdump(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (i, byte) in bytes.iter().enumerate() {
        if i != 0 {
            if i % 16 == 0 {
                out.push('\n');
            } else {
                out.push(' ');
            }
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out.push('\n');
    out
}

#[test]
fn shm_header_layout_matches_cpp_golden() {
    assert_eq!(SHM_HEADER_BYTES, 32, "the header is eight u32 words");
    let words = startup_header_words();
    let bytes = header_bytes(&words);

    let mut map = String::new();
    map.push_str("shm header SFV3 32 bytes, 8 u32 words native-endian\n");
    map.push_str(&format!("word 0 magic 0x{:08x}\n", words[0]));
    map.push_str(&format!("word 1 width {}\n", words[1]));
    map.push_str(&format!("word 2 height {}\n", words[2]));
    map.push_str(&format!("word 3 seq {}\n", words[3]));
    map.push_str(&format!("word 4 ringSlots {}\n", words[4]));
    map.push_str(&format!("word 5 slotCapacity {}\n", words[5]));
    map.push_str(&format!("word 6 generationLo 0x{:08x}\n", words[6]));
    map.push_str(&format!("word 7 generationHi 0x{:08x}\n", words[7]));
    map.push_str("hexdump:\n");
    map.push_str(&hexdump(&bytes));

    saffron_test_support::assert_bytes_match_golden("shm_header.layout", map.as_bytes());
}

/// The live producer's segment header must carry the same field semantics the golden pins.
/// The generation value itself is minted per mapping; every other startup word is fixed.
#[test]
fn live_publisher_header_matches_the_golden_abi_words() {
    let mut publisher = ViewportShmPublisher::new();
    publisher
        .enable(ShmViewConfig {
            view: ShmView::Scene,
            name: unique_shm_name(),
        })
        .expect("enable scene segment");

    let scene = publisher.view_mut(ShmView::Scene).expect("scene enabled");
    let seg = scene.segment_bytes().expect("scene mapped");

    let word = |i: usize| u32::from_ne_bytes(seg[i * 4..i * 4 + 4].try_into().unwrap());
    assert_eq!(word(0), SHM_MAGIC, "magic word matches the golden ABI");
    assert_eq!(word(1), 0, "no frame yet: width 0 at startup");
    assert_eq!(word(2), 0, "no frame yet: height 0 at startup");
    assert_eq!(word(3), 0, "seq 0 at startup");
    assert_eq!(word(4), SHM_RING_SLOTS, "ring depth matches the golden");
    assert_eq!(
        word(5),
        MIN_SHM_SLOT_CAPACITY as u32,
        "capacity floored at 4K RGBA, matching the golden"
    );
    let generation = u64::from(word(6)) | (u64::from(word(7)) << 32);
    assert_eq!(generation, scene.generation());
    assert_ne!(generation, 0);
}
