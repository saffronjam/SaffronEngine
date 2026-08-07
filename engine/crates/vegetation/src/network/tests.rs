use saffron_core::Uuid;
use saffron_spatial::{ResidencyFacet, ResidencyMask, WorldCellKey};

use crate::{
    BaseManifestOffer, CellInterestKey, CellInterestSet, CheckpointReconciliation, ContentHash,
    CookPlatformProfile, CookVersionSet, Error, ManifestHandshakeRejection, PeerBaseIdentity,
    VegetationBaseManifest, VegetationCheckpoint, VegetationState,
};

use super::VEGETATION_NETWORK_PROTOCOL_VERSION;

fn platform() -> CookPlatformProfile {
    CookPlatformProfile {
        target: "x86_64-unknown-linux-gnu".to_owned(),
        content_profile: "portable-vulkan".to_owned(),
        toolchain: "rust-1.96".to_owned(),
        features: vec!["canonical-fixed".to_owned()],
    }
}

fn manifest() -> VegetationBaseManifest {
    VegetationBaseManifest::current(
        Uuid(1),
        Uuid(2),
        ContentHash::new([3; 32]),
        CookVersionSet::current(),
        platform(),
        ContentHash::new([4; 32]),
    )
}

fn interest(cells: &[(WorldCellKey, ResidencyMask)]) -> CellInterestSet {
    let mut set = CellInterestSet::new();
    for (cell, mask) in cells {
        set.declare(*cell, *mask).unwrap();
    }
    set
}

#[test]
fn interest_declaration_round_trips_and_rejects_an_empty_mask() {
    let mut set = interest(&[
        (
            WorldCellKey::base(4, -1, 2),
            ResidencyMask::one(ResidencyFacet::Render).with(ResidencyFacet::Simulation),
        ),
        (
            WorldCellKey::base(-9, 0, 0),
            ResidencyMask::one(ResidencyFacet::Physics),
        ),
    ]);
    let bytes = set.canonical_bytes().unwrap();
    assert_eq!(CellInterestSet::from_canonical_bytes(&bytes).unwrap(), set);

    assert!(matches!(
        set.declare(WorldCellKey::base(1, 1, 1), ResidencyMask::NONE),
        Err(Error::Network(_))
    ));
    assert!(set.withdraw(WorldCellKey::base(-9, 0, 0)));
    assert!(!set.withdraw(WorldCellKey::base(-9, 0, 0)));
    assert_ne!(set.canonical_bytes().unwrap(), bytes);
}

#[test]
fn a_declaration_out_of_canonical_cell_order_is_rejected() {
    let ordered = interest(&[
        (
            WorldCellKey::base(-1, 0, 0),
            ResidencyMask::one(ResidencyFacet::Render),
        ),
        (
            WorldCellKey::base(1, 0, 0),
            ResidencyMask::one(ResidencyFacet::Render),
        ),
    ]);
    let bytes = ordered.canonical_bytes().unwrap();
    // Two 26-byte entries close the encoding — three coordinates, a level, a facet mask. Swapping
    // them is the only change, so a decoder that sorted rather than verified would accept this.
    let entry = bytes.len() - 52;
    let mut swapped = bytes.clone();
    swapped[entry..entry + 26].copy_from_slice(&bytes[entry + 26..]);
    swapped[entry + 26..].copy_from_slice(&bytes[entry..entry + 26]);
    assert!(CellInterestSet::from_canonical_bytes(&swapped).is_err());
}

#[test]
fn interest_keys_enumerate_in_canonical_cell_then_facet_order() {
    let set = interest(&[
        (
            WorldCellKey::base(2, 0, 0),
            ResidencyMask::one(ResidencyFacet::Render),
        ),
        (
            WorldCellKey::base(-3, 0, 0),
            ResidencyMask::one(ResidencyFacet::Simulation).with(ResidencyFacet::Render),
        ),
    ]);
    let keys: Vec<_> = set.keys().collect();
    assert_eq!(
        keys,
        vec![
            CellInterestKey::new(WorldCellKey::base(-3, 0, 0), ResidencyFacet::Render),
            CellInterestKey::new(WorldCellKey::base(-3, 0, 0), ResidencyFacet::Simulation),
            CellInterestKey::new(WorldCellKey::base(2, 0, 0), ResidencyFacet::Render),
        ]
    );
    assert!(set.contains(keys[0]));
    assert!(!set.contains(CellInterestKey::new(
        WorldCellKey::base(2, 0, 0),
        ResidencyFacet::Physics
    )));

    // The routing word is a name, so two distinct keys must not share one.
    let words: std::collections::BTreeSet<_> = keys.iter().map(|key| key.routing_word()).collect();
    assert_eq!(words.len(), keys.len());
}

#[test]
fn a_handshake_names_the_first_dimension_that_differs() {
    let authority = manifest();
    let offer = BaseManifestOffer::for_manifest(&authority).unwrap();
    let peer = PeerBaseIdentity::for_manifest(&authority).unwrap();
    assert_eq!(peer.accept(offer).unwrap(), offer.binding);

    let mut different_world = manifest();
    different_world.world = Uuid(99);
    let stranger = PeerBaseIdentity::for_manifest(&different_world).unwrap();
    assert_eq!(
        stranger.accept(offer),
        Err(ManifestHandshakeRejection::ManifestIdentity)
    );

    let mut different_graph = offer;
    different_graph.binding.cook_graph_identity = ContentHash::new([9; 32]);
    assert_eq!(
        peer.accept(different_graph),
        Err(ManifestHandshakeRejection::CookGraphIdentity)
    );

    let mut different_versions = offer;
    different_versions.binding.versions.evaluator += 1;
    assert!(matches!(
        peer.accept(different_versions),
        Err(ManifestHandshakeRejection::Versions { .. })
    ));

    let mut different_seeds = offer;
    different_seeds.binding.seed_namespaces_identity = ContentHash::new([8; 32]);
    assert_eq!(
        peer.accept(different_seeds),
        Err(ManifestHandshakeRejection::SeedNamespaces)
    );

    let mut different_schema = offer;
    different_schema.point_schema_identity = ContentHash::new([7; 32]);
    assert_eq!(
        peer.accept(different_schema),
        Err(ManifestHandshakeRejection::PointSchema)
    );

    let mut different_protocol = offer;
    different_protocol.protocol_version = VEGETATION_NETWORK_PROTOCOL_VERSION + 1;
    assert!(matches!(
        peer.accept(different_protocol),
        Err(ManifestHandshakeRejection::ProtocolVersion { .. })
    ));
}

/// A two-cell state whose ecology summarises both, so a scoping rule that dropped nothing is
/// visible in the result rather than absorbed by a world that only ever held one cell.
fn two_cell_state(
    identity: [u8; 32],
    inside: WorldCellKey,
    outside: WorldCellKey,
) -> VegetationState {
    let mut state = VegetationState::new(identity);
    for (index, cell) in [inside, outside].into_iter().enumerate() {
        let ordinal = u128::try_from(index).unwrap() + 1;
        crate::reduce_mutations(
            &mut state,
            identity,
            &[crate::VegetationMutationRecord {
                header: crate::MutationHeader {
                    cell,
                    transaction: ordinal,
                    authority: 1,
                    logical_tick: 1,
                    idempotency_key: ordinal,
                    base_revision: None,
                },
                mutation: crate::VegetationMutation::Tombstone {
                    plant: crate::PlantId::explicit([u8::try_from(index).unwrap() + 1; 16])
                        .unwrap(),
                },
            }],
        )
        .unwrap();
    }
    let summaries = [inside, outside]
        .into_iter()
        .map(|cell| (cell, crate::EcologyCellSummary::default()))
        .collect();
    *state.ecology_mut() = crate::EcologyState::from_parts(
        crate::ECOLOGY_SIMULATION_VERSION,
        crate::EcologyClock::at(0),
        summaries,
    )
    .unwrap();
    state
}

#[test]
fn scoping_drops_an_undeclared_cell_and_a_summary_no_facet_asked_for() {
    let identity = manifest().identity().unwrap().bytes();
    let inside = WorldCellKey::base(0, 0, 0);
    let outside = WorldCellKey::base(7, 0, 0);
    let state = two_cell_state(identity, inside, outside);
    assert_eq!(state.cells().len(), 2);
    assert_eq!(state.ecology().summaries().len(), 2);

    let drawing = state
        .scope_to_interest(&interest(&[(
            inside,
            ResidencyMask::one(ResidencyFacet::Render),
        )]))
        .unwrap();
    assert_eq!(
        drawing.cells().keys().copied().collect::<Vec<_>>(),
        vec![inside],
        "the undeclared cell's delta must not cross"
    );
    assert!(
        drawing.ecology().summaries().is_empty(),
        "a peer that only draws a cell never advances its biology, so no summary crosses"
    );

    let simulating = state
        .scope_to_interest(&interest(&[(
            inside,
            ResidencyMask::one(ResidencyFacet::Simulation),
        )]))
        .unwrap();
    assert_eq!(
        simulating
            .ecology()
            .summaries()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![inside],
        "the declared cell's summary crosses under Simulation, the undeclared one never does"
    );

    // Scope is what the fingerprint is taken over: two peers naming the same interest but holding
    // different content for it disagree, which is the only way Diverged can be reported.
    let narrow = interest(&[(inside, ResidencyMask::one(ResidencyFacet::Render))]);
    let authority = VegetationCheckpoint::of(&state, &narrow, 4).unwrap();
    let stale = VegetationCheckpoint::of(&VegetationState::new(identity), &narrow, 4).unwrap();
    assert_eq!(
        stale.reconcile(authority),
        CheckpointReconciliation::Diverged
    );
    assert!(stale.reconcile(authority).requires_snapshot());
}

#[test]
fn a_checkpoint_over_the_same_scope_agrees_and_over_a_different_one_does_not() {
    let identity = manifest().identity().unwrap().bytes();
    let state = VegetationState::new(identity);
    let render = ResidencyMask::one(ResidencyFacet::Render);
    let narrow = interest(&[(WorldCellKey::base(0, 0, 0), render)]);
    let wide = interest(&[
        (WorldCellKey::base(0, 0, 0), render),
        (WorldCellKey::base(1, 0, 0), render),
    ]);

    let authority = VegetationCheckpoint::of(&state, &narrow, 12).unwrap();
    let peer = VegetationCheckpoint::of(&state, &narrow, 12).unwrap();
    assert_eq!(peer.reconcile(authority), CheckpointReconciliation::InSync);

    let behind = VegetationCheckpoint::of(&state, &narrow, 9).unwrap();
    assert_eq!(
        behind.reconcile(authority),
        CheckpointReconciliation::Behind { by: 3 }
    );
    let ahead = VegetationCheckpoint::of(&state, &narrow, 15).unwrap();
    assert_eq!(
        ahead.reconcile(authority),
        CheckpointReconciliation::Ahead { by: 3 }
    );
    assert!(ahead.reconcile(authority).requires_snapshot());

    let other_scope = VegetationCheckpoint::of(&state, &wide, 12).unwrap();
    assert_eq!(
        other_scope.reconcile(authority),
        CheckpointReconciliation::InterestMismatch
    );

    let foreign = VegetationCheckpoint::of(&VegetationState::new([0xEE; 32]), &narrow, 12).unwrap();
    assert_eq!(
        foreign.reconcile(authority),
        CheckpointReconciliation::ManifestMismatch
    );
}
