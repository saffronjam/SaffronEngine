+++
title = 'Vegetation navigation contributions'
weight = 11
+++

# Vegetation navigation contributions

A forest changes where agents can walk, but vegetation owns no navigation system. This seam
publishes what vegetation *contributes* — cell-addressed obstacles and traversal-cost fields, plus
the world regions that changed — and a navigation consumer decides what to build from it. There is
no foliage-private navmesh, no tile builder, and no pathfinding here.

## Four declarations

Every plant contributes exactly one thing, derived from its interaction policy and its family's
authored navigation proxies:

| Plant | Contribution |
|---|---|
| `Decorative`, or a family with no navigation proxy | nothing at all |
| `Interactive` | traversal-cost field: passable at a cost multiplier |
| `Structural`, `Harvestable` | static obstacle |
| Either of those while [promoted](../plant-promotion/) | dynamic obstacle |

The promoted case is the interesting one. A promoted plant is moving under physics, so a static
tile rebuild would be stale before it finished; the consumer gets a dynamic obstacle until the
plant settles and demotes back to bulk. The same suppression revision that retires the plant's bulk
render instance and collision batch also re-derives its navigation contribution, so all three
views agree at every synchronization point.

Each published contribution carries the plant identity, its conservative world bounds, the
world-space footprint polygon as X/Z metre pairs, the obstacle height, and the cost multiplier —
where one is neutral.

## Dirty regions

Rebuild cost belongs to the consumer, so the seam tells it exactly what changed rather than making
it diff. A cell whose generation or promoted-plant set moves marks the bounds of every contribution
it retired and every one it published. Overlapping regions coalesce into one box as they arrive,
which keeps a busy frame from producing hundreds of adjacent rebuild requests.

Draining transfers ownership: `vegetation-nav-contributions` with `drainDirty` clears the regions,
so exactly one consumer rebuilds each. Reading without it peeks.

```sh
sa vegetation-nav-contributions
#   cell      1,     0,    -2 L0  2 contribution(s), 1 obstacle(s)
#   dirty=1  contributions=2  obstacles=1 (dynamic 0)  drained=no
```

The `vegetationNavigation` debug overlay draws the same publication in the viewport: each footprint
as a closed loop with an upright per vertex carrying its obstacle height, red for a static obstacle,
amber for a dynamic one, blue for a cost field, and white boxes for the dirty regions.
Reading the overlay against the plants on screen answers whether the seam agrees with the world.

## In the code

| What | File | Symbols |
|---|---|---|
| Publication and dirty tracking | `runtime/src/vegetation_navigation.rs` | `VegetationNavigationSeam`, `derive_contributions`, `take_dirty_regions` |
| Contribution vocabulary | `runtime/src/vegetation_navigation.rs` | `NavigationContribution`, `NavigationContributionKind` |
| Cooked per-plant rows | `vegetation/src/cell_facet.rs` | `VegetationNavigationContribution` |
| Authored proxies | `vegetation/src/asset.rs` | `PlantNavigationProxy` |
| Synchronization point | `runtime/src/session.rs` | `RuntimeSession::synchronize_vegetation` |

## Related

- [Vegetation state](../vegetation-state/) — facet residency and the typed transitions that move contributions
- [Plant promotion](../plant-promotion/) — why a promoted plant contributes a dynamic obstacle
- [Vegetation collision residency](../../physics/vegetation-collision/) — the parallel physics facet
