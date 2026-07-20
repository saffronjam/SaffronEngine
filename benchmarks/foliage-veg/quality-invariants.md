# Foliage and vegetation quality invariants

These invariants constrain every performance optimization and representation transition. A budget
controls scheduling, memory residency, or representation choice; it never authorizes dropped
authored plants, silent density reduction, unstable identity, or a lower-quality persistent result.

## Image continuity

- Silhouette and alpha coverage have no transition-attributable step when a plant changes cluster,
  hierarchy cut, aggregate, or residency representation. A moving-camera A/B test compares the
  transition run with a run pinned to either neighbouring representation and rejects a new one-frame
  coverage edge.
- Transmission and subsurface response use the same canonical coverage, thickness, material, and
  light-facing data in every representation. A transition cannot introduce an energy step beyond the
  temporal derivative measured in both pinned references.
- Depth, motion, direct shadow, ray visibility, and GI use the same accepted instance identity and
  deformation state as the color pass. A plant cannot disappear from one consumer while remaining in
  another, and a transition cannot create a shadow or irradiance step absent from both pinned runs.
- Wind, interaction, and phenology preserve phase and state across streaming, origin rebasing, and
  representation changes. Re-entry resumes canonical state rather than restarting from the camera.

## Authoritative continuity

- Worker count, job order, GPU dispatch shape, source order, cancellation timing, and origin rebasing
  cannot change accepted macro identities, fixed numeric decisions, persisted state, or cooked bytes.
- A provider edit either reprojects a manual attachment to its declared primitive identity or marks it
  orphaned. It never chooses another primitive because that point happens to be nearby.
- Overflow and unavailable data are typed failures or explicit not-ready states. They cannot clamp,
  wrap, substitute bind pose, reuse a stale generation, or reduce population density.

## Budget evidence

Every numeric performance ceiling comes from the repeatable Anima fixture on the named hardware and
driver in the adjacent JSON record. The Phase 1 record establishes CPU scene gather, GPU and CPU frame
time, draw and shadow submission, exact instance traffic, RT participation, and retained mesh-query
memory before the renderer cutover. NVIDIA, AMD, and MoltenVK keep separate records; a result from one
class is not relabelled as another class's acceptance threshold.
