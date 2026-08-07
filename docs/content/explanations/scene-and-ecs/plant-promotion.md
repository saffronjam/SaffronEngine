+++
title = 'Plant promotion'
weight = 10
+++

# Plant promotion

Gameplay needs a felled tree to fall, break, and carry a script — behaviour that wants a real
entity and a real rigid body. Promotion gives a sparse subset of macro plants exactly that: a
transient scene entity that renders, collides, and simulates on the plant's behalf, while the
macro SoA row stays the authority for what the plant *is*.

## One owner at a time

A promoted plant must never appear twice. Promotion suppresses the plant's bulk render instance
and its batched collision proxy in the same synchronization pass that creates the entity;
demotion restores them in the pass that destroys it. The suppression lives on `VegetationWorld`
keyed by `PlantId`, not inside a published cell generation, so it survives cell unload,
republication, and reload without bookkeeping. Render and collision adapters cache each cell's
`(generation, bulk_revision)` pair and re-derive the cell when either moves.

```text
Bulk → Promoting → Promoted(entity) → Demoting → Bulk
```

Requests arrive at any time and commit only at the fixed synchronization point inside
`RuntimeSession::synchronize_vegetation`, ahead of collision residency, so no frame observes a
half-applied transition. A demotion request on a promotion that has not committed cancels it
outright; a promotion request during `Demoting` keeps the live entity.

## What the entity view carries

The view renders identically to the bulk instance it replaces: the family's compiled mesh, the
`PlantVariant` that selects the same authored `(variation, phenotype)` assembly combination, and
a `MaterialSet` built from the family's material slots. Collision comes from the family's
largest analytic proxy as one solid dynamic body, so the entity owns collision outright rather
than sharing it.

`PlantOrigin` binds the view to its plant and the cell generation it was promoted from. It is
immutable: the scene rejects replacing it, borrowing it mutably, or removing it, so a view can
never be re-pointed at another plant.

`PlantVitals` carries the plant's live biology (lifecycle, health, moisture, fuel, age) and is
mutable, because damage and growth happen to the view. Both components are runtime-only and never
serialized: a promoted entity is a view, not a second plant database.

`vegetation-plant-vitals` is how gameplay and tooling reach it — read it with the plant alone, or
pass any of `lifecycle`, `health`, `moisture`, `fuel`, `ecologyTick` to replace that field on the
standing view. Health, moisture, and fuel cross as the reducer's unit-interval bits, so a value
written here is exactly the value the write-back records.

```sh
sa vegetation-plant-vitals 40aabbccddeeff00112233445566778899 --health 20000
# plant=40aa…  entity=7  lifecycle=mature  health=20000  moisture=9000  fuel=4000  tick=12
```

## State returns through the reducer

Demotion writes the entity's live world state back as a `PromotionOriginState` mutation: exact
position, quantized orientation and scale, plus linear velocity in metres per fixed tick and
angular velocity in turns per fixed tick, read from the live body.

Momentum travels both ways. The velocity a write-back recorded rides the plant's persistent delta
into every generation the cell publishes afterwards, so the snapshot a later promotion builds its
view from carries it, and the fresh body is given it back the moment it exists. A tree knocked over
and then demoted while it was still moving keeps moving when it is promoted again, rather than
restarting from rest at the pose it happened to be caught in. The two conversions are one round
trip: what `origin_state` reads off a live body is exactly what `restore_momentum` puts back, to
within one Q15.16 quantum.

Biology rides the same transaction, but only what moved. The view is stamped with the row's vitals
at promotion, and the demotion compares against that stamp: a field the view changed becomes a
`StateOverride`, an age it advanced becomes a `LifecycleTransition`, and a view nobody touched
writes back no biology at all. The macro row goes on ageing under a standing view, so returning the
whole snapshot would overwrite whatever the row accrued meanwhile — the difference is the only part
the view can honestly claim. A change smaller than one quantum of the reducer's unit vocabulary is
not a change.

The payload records into the owner cell of its *final* position, so a tree that fell across a cell
boundary writes back in the cell it now occupies.

Saving is a barrier, not a wait. `save-project` and `vegetation-state-export` flush every
promoted plant's live state through the reducer first, as one transaction, and the entity keeps
living — a falling trunk is captured mid-fall with its velocity rather than settled. The
transaction id and every operation key derive from a content hash of the records, so an
unchanged flush is an exact idempotent replay while a changed one is a fresh transaction.

A rebind to a different cooked generation drops every view instead of writing back. A view
describes a plant of the generation it was promoted from, and that generation is not the bound
one any more, so its state has nowhere truthful to land. Every plant then has exactly one owner
again — the entity, its body, and the bulk suppression it stood for all go at once — while the
persistent deltas the world already carried rebase onto the incoming generation.

## Simulation ownership changes hands explicitly

Promoting a plant claims its *simulation ownership* on `VegetationWorld`, beside the bulk
suppression. Every committed mutation that says where a plant is, or whether it exists at all —
a transform override, a promotion write-back, a whole-delta restore, a tombstone, a removal, a
regrow — records the authority that committed it as the plant's new owner. Biology does not:
damage, harvest, wetting, and ordinary lifecycle steps leave the claim where it is, so a script
damaging a promoted plant does not take the plant away from the view simulating it.

At each synchronization point the promotion authority checks the claim before it commits any
transition. A plant whose owner is now somebody else loses its view immediately and without a
write-back — the view's state describes a plant this session no longer speaks for, and recording
it would overwrite the new owner's answer. The plant gets its bulk representation back in the same
pass, so the handover never leaves two owners or none. This is what makes an editor undo, a
recook's delta restore, and a removal safe to issue while a plant is standing as an entity.

A wholesale replacement of persistent state — loading a save, importing a snapshot, joining a
network stream — moves `VegetationWorld::authority_epoch` instead of naming plants one by one.
Every live view yields on the next synchronization point, because the world they were observing
was replaced by an answer they had no part in. `vegetation-runtime-status` reports how many views
have been released this way as `promotion.releasedTotal`.

A promoted view is also kept out of the surface set a cook reads. It is a derived representation of
a plant the cook itself placed, and it falls under physics; admitting it would let a cook take its
own output as ground and would move the surface inputs every frame a view is standing, so no cook
taken during a promotion could validate its inputs at commit.

## Felling separates the product from the plant

Felling is not a demotion. The rooted plant stays under its own identity and becomes a stump
through a `LifecycleTransition`; the above-ground mass becomes a separate product entity — same
mesh, same materials, its own dynamic body — that falls and can be carried away.

The product deliberately carries no `PlantOrigin` and no `PlantVitals`. A log is not the tree it
came from, so no query, contact, or save can resolve it as one, and the stump keeps everything that
identifies the plant. A plant that was promoted demotes first, writing its state back, before the
felling commits.

```sh
sa vegetation-fell 40aabbccddeeff00112233445566778899
```

## Driving it

```sh
sa vegetation-promote 40aabbccddeeff00112233445566778899
# plant=40aabbccddeeff00112233445566778899  state=promoting

sa vegetation-demote 40aabbccddeeff00112233445566778899
# plant=40aabbccddeeff00112233445566778899  state=demoting  entity=7
```

`vegetation-runtime-status` reports the counters (`promoted`, `promoting`, `demoting`, plus
lifetime totals) and `vegetation-runtime-inspect` reports one plant's promotion state beside its
resident row and persistent delta.

## In the code

| What | File | Symbols |
|---|---|---|
| State machine and transitions | `runtime/src/vegetation_promotion.rs` | `VegetationPromotion`, `PlantPromotionState`, `flush_state` |
| Felling and products | `runtime/src/vegetation_promotion.rs` | `request_felling`, `commit_felling`, `spawn_product` |
| Entity view construction | `runtime/src/vegetation_promotion.rs` | `spawn_view`, `primary_collider`, `origin_state`, `restore_momentum` |
| Ownership handover | `runtime/src/vegetation_promotion.rs` | `reconcile_ownership`, `release_view` |
| Bulk suppression and ownership authority | `vegetation/src/runtime_world/state.rs` | `promote_plant`, `demote_plant`, `cell_bulk_revision`, `plant_simulation_authority`, `authority_epoch` |
| Momentum in the published generation | `vegetation/src/runtime_world/generation.rs` | `CellPersistentOverlay`, `VegetationPlantSnapshot::linear_velocity` |
| View components | `scene/src/component.rs` | `PlantOrigin`, `PlantVitals` |
| Live biology read/write | `runtime/src/vegetation_promotion.rs` | `vitals`, `set_vitals`, `vitals_mutations` |
| Mid-play body creation | `physics/src/world.rs` | `World::add_entity_body`, `World::remove_entity_bodies` |
| Write-back payload | `vegetation/src/mutation.rs` | `PromotionOriginState`, `VegetationMutation` |

## Related

- [Vegetation state](../vegetation-state/) — cell generations, facet residency, and the reducer
- [Vegetation collision residency](../../physics/vegetation-collision/) — the batched bulk proxies promotion suppresses
- [Plant rendering](../../geometry-and-assets/plant-rendering/) — the assembly mesh and combinations a view reuses
