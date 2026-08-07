+++
title = 'Vegetation network sessions'
weight = 14
+++

# Vegetation network sessions

Two machines simulating the same vegetation world have to agree on more than the messages between
them. They have to agree on the world itself — the cooked base, the numeric contract, the seed
namespaces — because a plant's identity and position are derived, not transmitted. A peer that
disagrees about any of that reduces the same mutation into a different world and drifts silently.

So the session contract is four values, and none of them names a transport. A transport carries
them; it does not define them.

## Binding to one base

An authority offers exactly what it is simulating: the wire contract version, the state binding
(immutable manifest identity, canonical cook graph, deterministic version set, seed-namespace
identity), and the macro-point schema identity. A peer compares that against the manifest it
loaded and either accepts it whole or names the first dimension that differs.

There is no negotiation and no partial acceptance. A rejection is a typed reason a user can act on
— "peer compiled a different canonical cook graph" — rather than a version number to coerce.

## Declaring interest

A peer does not subscribe to a world; it subscribes to cells, and within a cell to facets. One
declaration is a facet mask per cell in canonical cell order, and its exact canonical encoding
hashes to a scope identity both sides carry.

The facet matters as much as the cell. A peer that only *draws* a cell reconstructs appearance from
the immutable base plus the persistent deltas, and never advances biology; shipping it ecology
boundary summaries would hand it state it cannot maintain. So an ecology summary crosses only under
the `Simulation` facet, while a cell's plant deltas cross under any facet at all.

Applied-transaction signatures never cross. A signature covers every cell its transaction touched,
so a peer holding a subset would recompute a different one and reject a legitimate replay.
Duplicate suppression on the wire is the transport sequence, not the idempotency key.

Nothing reconstructible crosses either. Micro grass fields, the wind field, and per-plant bend are
all derived from the immutable base and the transmitted macro state, so each peer rebuilds them
locally and identically.

## Sequencing and late join

Operations travel in a sequenced envelope: a monotonic transport sequence, the manifest identity
both sides understand, the idempotent operations, and optionally an authoritative snapshot. A
sequence at or below the last accepted one is a retransmission and never reaches the reducer.

A late joiner takes the *same* path a corrected peer takes. The authority consumes a stream
position, scopes its state to the peer's declaration, and puts that snapshot inside an ordinary
envelope. There is one receive path, so a bug in seating is a bug in correction and shows up twice.

A seated peer also refuses an operation for a cell it declared no interest in, rather than widening
its scope silently to accept it.

## Noticing divergence

Both sides periodically fingerprint the same scope: the sequence, the completed ecology tick, the
manifest identity, the scope identity, and a digest of the scoped persistent state. Comparing two
fingerprints answers a question, not a boolean — in sync, behind by *n*, ahead by *n*, scoped
differently, bound to a different world, or diverged at the same sequence.

Only the last three call for an authoritative snapshot. Being behind is ordinary and operations
fix it.

## Driving it

```sh
sa vegetation-network-interest '{"cells":[{"cell":{"coordinates":["0","0","0"],"level":0},
                                          "facets":["render","simulation"]}]}'
sa vegetation-network-checkpoint
```

The first declares the seated scope (an empty list leaves the session); the second reports the
scope and its fingerprint at the highest agreed sequence.

## Where it lives

| What | File | Symbols |
|---|---|---|
| Base handshake | `vegetation/src/network/handshake.rs` | `BaseManifestOffer`, `PeerBaseIdentity`, `ManifestHandshakeRejection` |
| Interest and scoping | `vegetation/src/network/interest.rs` | `CellInterestKey`, `CellInterestSet`, `VegetationState::scope_to_interest` |
| Checkpoints | `vegetation/src/network/checkpoint.rs` | `VegetationCheckpoint`, `CheckpointReconciliation` |
| Late join | `vegetation/src/network/session.rs` | `LateJoinRequest`, `LateJoinGrant` |
| Sequenced envelope | `vegetation/src/mutation/envelope.rs` | `NetworkMutationEnvelope` |
| World seam | `vegetation/src/runtime_world/network.rs` | `issue_late_join`, `accept_late_join`, `network_checkpoint` |
| Control surface | `control/src/commands_vegetation_runtime/network.rs` | `vegetation-network-interest`, `vegetation-network-checkpoint` |
