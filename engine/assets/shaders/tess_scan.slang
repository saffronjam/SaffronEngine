// Predict + scan (Phase 3b), per-instance dispatch: one compute thread per base triangle of one
// displaced instance. Reads the triangle's three per-edge factors (Phase-3a output, indexed by the
// Phase-2 per-triangle edge references), derives the integer dice level, computes the EXACT output
// vertex/index counts of the Phase-4 dice, and prefix-sums them (atomic carry) to packed write offsets
// inside the instance's worst-case-reserved slice.
//
// The dice contract (Phase 3 defines it; Phase 4 must emit exactly this):
//   m          = clamp(max(f0, f1, f2), 1, factorCap)       — driving factor (the coarsest edge)
//   (lv, leaf) = dicePlan(m)                                 — SPLIT pass: 4^lv barycentric subpatches, each
//                                                             diced at leaf = ceil(m / 2^lv), leaf <= 11 so
//                                                             every leaf is a valid CLAS-sized cluster
//   verts      = 4^lv * (leaf + 1) * (leaf + 2) / 2          — barycentric grid vertices, all subpatches
//   tris       = 4^lv * leaf * leaf                          — grid triangles  (indices = tris * 3)
// A triangle within the dice cap needs no split (lv = 0, one leaf at ceil(m)). Sibling subpatches share
// their interior split edges bit-identically (parent-relative dyadic midpoints); the parent's OUTER edges
// stay watertight because Phase 4 snaps every boundary micro-vertex onto that edge's SHARED per-edge factor
// (both incident base triangles read the same slot regardless of either side's split depth). The count is a
// closed form of dicePlan — Phase 3's prediction and Phase 4's emission cannot diverge (both call dicePlan).

// TriEdges: 16 B — e[3] (uint) @0, pad @12.
[[vk::binding(0, 0)]] ByteAddressBuffer triEdges;            // TriEdges[] (16 B stride)
[[vk::binding(1, 0)]] StructuredBuffer<float> factors;      // per-edge factors (instance slice)
// Per-triangle dice record for Phase 4: (vertOffset, indexOffset, level, pad), at triBase + t.
[[vk::binding(2, 0)]] RWStructuredBuffer<uint4> perTri;
// Per-instance running totals [vertCount, indexCount], two uints per instance — atomically accumulated.
[[vk::binding(3, 0)]] RWStructuredBuffer<uint> instanceCounters;
// Global micro-geometry totals: [0]=micro-vert total, [1]=micro-tri total (emit-dispatch + RT sizing).
[[vk::binding(4, 0)]] RWStructuredBuffer<uint> globalTotals;

struct Push
{
    uint triCount;    // base triangles in this instance's mesh
    uint triBase;     // this instance's base offset into perTri (and its 2-uint counter slot = 2*instanceRow)
    uint counterBase; // this instance's base index into instanceCounters (= 2 * instance row)
    float factorCap;  // hard clamp on the dice level
    uint pad0;
    uint pad1;
    uint pad2;
    uint pad3;
};
[[vk::push_constant]] Push push;

// The largest integer dice level whose diced leaf fits the CLAS caps (128 tris / 256 verts): L = 11.
// Mirrors `TESS_MAX_DICE_FACTOR` (tessellation.rs).
static const float TESS_MAX_DICE_FACTOR = 11.0;
// Numerical-safety clamp on split depth so `1 << (2 * levels)` never overflows a uint (mirrors
// `TESS_MAX_SPLIT_LEVELS`); the budget keeps the cap far below the depth that would reach it.
static const uint TESS_MAX_SPLIT_LEVELS = 15u;

// The GPU split-pass dice plan for a driving factor `m`: how many `4^levels` barycentric subpatches to
// enumerate and the integer leaf dice level each is diced at. Mirrors `dice_plan` / `split_recursion`
// (tessellation.rs) EXACTLY so the CPU reservation and this scan's counts cannot diverge from Phase 4's
// emission. `ceil` (not round) on the leaf so each edge always has >= ceil(f_e)+1 grid points, enough to
// represent the geomorph's floor->ceil boundary segments without running short.
void dicePlan(float m, out uint levels, out uint leafLevel)
{
    if (m <= TESS_MAX_DICE_FACTOR)
    {
        levels = 0u;
        leafLevel = (uint)clamp(ceil(m), 1.0, TESS_MAX_DICE_FACTOR);
        return;
    }
    levels = min((uint)max(ceil(log2(m / TESS_MAX_DICE_FACTOR)), 0.0), TESS_MAX_SPLIT_LEVELS);
    const float leaf = m / (float)(1u << levels);
    leafLevel = (uint)clamp(ceil(leaf), 1.0, TESS_MAX_DICE_FACTOR);
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    const uint t = tid.x;
    if (t >= push.triCount)
    {
        return;
    }

    const uint eBase = t * 16u;
    const uint e0 = triEdges.Load<uint>(eBase);
    const uint e1 = triEdges.Load<uint>(eBase + 4u);
    const uint e2 = triEdges.Load<uint>(eBase + 8u);
    const float f0 = factors[e0];
    const float f1 = factors[e1];
    const float f2 = factors[e2];

    // Split pass: resolve the driving factor into subpatches + a CLAS-sized leaf dice level. The count is
    // the closed form over all `4^levels` subpatches, each a `leaf`-grid; Phase 4 emits exactly this.
    const float m = clamp(max(f0, max(f1, f2)), 1.0, push.factorCap);
    uint levels;
    uint leaf;
    dicePlan(m, levels, leaf);
    const uint subpatches = 1u << (2u * levels);
    const uint verts = subpatches * (leaf + 1u) * (leaf + 2u) / 2u;
    const uint tris = subpatches * leaf * leaf;
    const uint indices = tris * 3u;

    // Atomic-carry prefix sum into this instance's running totals: a valid contiguous packing inside
    // the instance's reserved slice (order is non-deterministic but disjoint and gapless).
    uint vertOffset;
    uint indexOffset;
    InterlockedAdd(instanceCounters[push.counterBase], verts, vertOffset);
    InterlockedAdd(instanceCounters[push.counterBase + 1u], indices, indexOffset);

    // Global micro-geometry totals for the emit-dispatch size (Phase 4) + RT worst-case (Phase 7).
    InterlockedAdd(globalTotals[0], verts);
    InterlockedAdd(globalTotals[1], tris);

    // Phase-4 dice record: (vertOffset, indexOffset, leafLevel, splitLevels). The emit enumerates
    // `4^splitLevels` subpatches, each diced at `leafLevel`, packed contiguously from `vertOffset`.
    perTri[push.triBase + t] = uint4(vertOffset, indexOffset, leaf, levels);
}
