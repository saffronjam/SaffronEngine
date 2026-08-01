//! What a network peer declares it needs, per cell and per facet.

use std::collections::BTreeMap;

use saffron_spatial::{ResidencyFacet, ResidencyMask, WorldCellKey};

use crate::binary::BinaryWriter;
use crate::{ContentHash, Error, Result, VegetationState};

const INTEREST_FORMAT: &str = "vegetation cell interest";

/// One exact cell/facet pair a peer is subscribed to.
///
/// Exact rather than a digest: the authority routes and scopes on this value, and a truncated
/// name that collided would ship one peer another peer's cell. [`CellInterestKey::routing_word`]
/// is the fixed-width form for a transport whose subscription table needs one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellInterestKey {
    pub cell: WorldCellKey,
    pub facet: ResidencyFacet,
}

impl CellInterestKey {
    #[must_use]
    pub const fn new(cell: WorldCellKey, facet: ResidencyFacet) -> Self {
        Self { cell, facet }
    }

    /// The canonical 26-byte encoding: three big-endian coordinates, the level, the facet.
    #[must_use]
    pub fn canonical_bytes(self) -> [u8; 26] {
        let mut bytes = [0_u8; 26];
        for (axis, coordinate) in self.cell.coordinates().into_iter().enumerate() {
            bytes[axis * 8..axis * 8 + 8].copy_from_slice(&coordinate.to_be_bytes());
        }
        bytes[24] = self.cell.level();
        bytes[25] = self.facet as u8;
        bytes
    }

    /// A fixed-width subscription name for a transport that indexes by machine word.
    ///
    /// Truncation of a domain-separated digest, so it is a routing hint and never an identity:
    /// resolve a received word back through the declared set rather than trusting it.
    #[must_use]
    pub fn routing_word(self) -> u128 {
        let mut preimage = b"saffron-anima/vegetation-network/interest-key/v1".to_vec();
        preimage.extend_from_slice(&self.canonical_bytes());
        let digest = ContentHash::of(&preimage).bytes();
        let mut word = [0_u8; 16];
        word.copy_from_slice(&digest[..16]);
        u128::from_be_bytes(word)
    }
}

/// The complete declared interest of one peer: a facet mask per cell, in canonical cell order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellInterestSet {
    entries: BTreeMap<WorldCellKey, ResidencyMask>,
}

impl CellInterestSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares interest in one cell, replacing any previous mask for it.
    ///
    /// # Errors
    ///
    /// [`Error::Network`] when the mask is empty — withdrawing is [`Self::withdraw`], and an
    /// empty entry would otherwise encode two ways.
    pub fn declare(&mut self, cell: WorldCellKey, facets: ResidencyMask) -> Result<()> {
        if facets == ResidencyMask::NONE {
            return Err(Error::Network(format!(
                "cell {cell} was declared with no facets; withdraw it instead"
            )));
        }
        self.entries.insert(cell, facets);
        Ok(())
    }

    /// Drops one cell from the declaration, reporting whether it was present.
    pub fn withdraw(&mut self, cell: WorldCellKey) -> bool {
        self.entries.remove(&cell).is_some()
    }

    #[must_use]
    pub fn facets(&self, cell: WorldCellKey) -> ResidencyMask {
        self.entries
            .get(&cell)
            .copied()
            .unwrap_or(ResidencyMask::NONE)
    }

    #[must_use]
    pub fn contains(&self, key: CellInterestKey) -> bool {
        self.facets(key.cell).contains(key.facet)
    }

    #[must_use]
    pub fn entries(&self) -> &BTreeMap<WorldCellKey, ResidencyMask> {
        &self.entries
    }

    /// Every declared cell/facet pair, in canonical cell-then-facet order.
    pub fn keys(&self) -> impl Iterator<Item = CellInterestKey> + '_ {
        self.entries.iter().flat_map(|(cell, mask)| {
            mask.iter()
                .map(move |facet| CellInterestKey::new(*cell, facet))
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The canonical encoding both peers hash to agree on scope.
    ///
    /// # Errors
    ///
    /// [`Error::NumericOverflow`] when the declaration exceeds the encodable length.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let mut writer = BinaryWriter::new();
        writer.bytes(b"saffron-anima/vegetation-network/interest-set/v1\0");
        writer.length(self.entries.len())?;
        for (cell, mask) in &self.entries {
            writer.cell(*cell);
            writer.u8(mask.bits());
        }
        Ok(writer.finish())
    }

    /// Exact scope identity, carried in a handshake and in every checkpoint.
    ///
    /// # Errors
    ///
    /// [`Error::NumericOverflow`] when the declaration exceeds the encodable length.
    pub fn identity(&self) -> Result<ContentHash> {
        Ok(ContentHash::of(&self.canonical_bytes()?))
    }

    /// Decodes a canonical declaration.
    ///
    /// # Errors
    ///
    /// [`Error::ArtifactFormat`] when the bytes are not a canonical declaration: an unknown
    /// prefix, an empty or duplicated mask, or cells out of canonical order.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let mut reader = crate::binary::BinaryReader::new(bytes, INTEREST_FORMAT);
        reader.expect(
            b"saffron-anima/vegetation-network/interest-set/v1\0",
            "prefix",
        )?;
        let count = reader.count(9)?;
        let mut entries: BTreeMap<WorldCellKey, ResidencyMask> = BTreeMap::new();
        let mut previous: Option<WorldCellKey> = None;
        for _ in 0..count {
            let cell = reader.cell()?;
            let bits = reader.u8()?;
            if bits == 0 || bits & !ResidencyMask::ALL.bits() != 0 {
                return Err(Error::ArtifactFormat {
                    format: INTEREST_FORMAT,
                    field: "entries.facets".to_owned(),
                });
            }
            if previous.is_some_and(|last| last >= cell) {
                return Err(Error::ArtifactFormat {
                    format: INTEREST_FORMAT,
                    field: "entries.cell".to_owned(),
                });
            }
            previous = Some(cell);
            let mut mask = ResidencyMask::NONE;
            for facet in ResidencyFacet::ALL {
                if bits & (1 << facet as u8) != 0 {
                    mask = mask.with(facet);
                }
            }
            entries.insert(cell, mask);
        }
        reader.complete()?;
        Ok(Self { entries })
    }
}

impl VegetationState {
    /// The part of this state one declared interest covers.
    ///
    /// Three rules, and each is a contract a peer relies on:
    ///
    /// - A cell's persistent delta crosses when the interest names that cell with any facet.
    /// - An ecology boundary summary crosses only under [`ResidencyFacet::Simulation`]; a peer
    ///   that only draws a cell reconstructs appearance from the immutable base and never
    ///   advances biology, so shipping it summaries would give it state it cannot maintain.
    /// - Applied-transaction signatures never cross. A signature covers every cell the
    ///   transaction touched, so a scoped peer recomputing one would disagree with the authority
    ///   and reject a legitimate replay; sequenced envelopes suppress duplicates instead.
    ///
    /// Nothing reconstructible crosses either: micro fields, wind, and bend are derived from the
    /// immutable base plus the transmitted macro state, so a receiver rebuilds them locally.
    ///
    /// # Errors
    ///
    /// [`Error::Mutation`] when the retained ecology summaries do not reassemble under this
    /// build's rule set.
    pub fn scope_to_interest(&self, interest: &CellInterestSet) -> Result<Self> {
        let cells = self
            .cells()
            .iter()
            .filter(|(cell, _)| interest.facets(**cell) != ResidencyMask::NONE)
            .map(|(cell, state)| (*cell, state.clone()))
            .collect();
        let summaries = self
            .ecology()
            .summaries()
            .iter()
            .filter(|(cell, _)| interest.facets(**cell).contains(ResidencyFacet::Simulation))
            .map(|(cell, summary)| (*cell, summary.clone()))
            .collect();
        let ecology = crate::EcologyState::from_parts(
            self.ecology().version(),
            self.ecology().clock(),
            summaries,
        )?;
        Ok(Self::from_canonical_parts(
            self.manifest_identity(),
            cells,
            BTreeMap::new(),
            ecology,
        ))
    }
}
