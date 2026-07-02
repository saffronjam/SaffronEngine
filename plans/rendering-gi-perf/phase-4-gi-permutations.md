# Phase 4 — GI shader permutations

**Status:** NOT STARTED

Every GI/indirect feature in `lighting.slang` is a **uniform runtime `if`** (screenFlags / extraFlags
/ counts …) inside one monolithic PSO. Worst-case VGPR (all paths live) caps occupancy for *every*
pixel, even when a feature is off. After Phase 2 removes diffuse GI from the fragment, the remaining
specular/branch set is smaller — convert the toggles to **Slang specialization constants** and bake a
small set of **GI-quality tiers** (not 2^N permutations).

## Approach
- Identify the feature flags still branched in the fragment post-Phase-2 (SSR, RT reflections,
  contact shadows, reflection-probe specular, DDGI-specular if any).
- Convert them to specialization constants; the übershader/PSO cache already keys unlit/wireframe —
  extend the key with a **tier enum** (e.g. Low / Medium / High GI), each a fixed feature set.
- Dead-strip unreached paths per tier so the compiler drops their VGPRs → higher occupancy.
- Warm the cache at startup / on tier change to avoid first-use compile hitches.

## Expected effect
`scene-opaque` common case lower peak VGPR → higher occupancy; stacks with Phases 1–2. Bucket by
tier to avoid permutation explosion.

## Risks / watch
- Permutation count — keep it a handful of tiers, not per-flag combinatorics.
- First-use PSO compile hitch — reuse the existing cache + warm-up.

## Verification
- Pipeline-statistics register count drop per tier; profiler occupancy/ms per active tier.
- Every tier renders correctly (A/B each). `just engine` + lint clean.
