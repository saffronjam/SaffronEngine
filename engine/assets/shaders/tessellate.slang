// The amplifying displacement kernel. One workgroup per base triangle (dispatched per instance,
// `tri_count` workgroups): dices the triangle into a barycentric micro-grid at the scan's dice level
// `L`, evaluates a Phong-smoothed base position at each sample, displaces it (scalar along the normal
// or tangent-space vector), re-derives a normal + tangent from the full displaced-surface Jacobian,
// welds shared-edge micro-vertices onto the edge's shared per-edge factor, and writes the
// micro-vertices + a generated index stream into the worst-case-reserved transient VB/IB at the
// offsets the scan predicted.
//
// The temporal prev-stream: each micro-vertex's PREVIOUS-frame position also goes to
// `outPrevVertices`, evaluated on the SAME cur-`L` grid but with the previous frame's per-edge factors
// (`prevFactors`) driving the boundary snap + interior geomorph. With static height the only
// cross-frame change is the camera-driven factor, so the cur→prev delta is exactly the geomorph slide
// the motion prepass needs for watertight TAA reprojection. The prev boundary snap reuses the shared
// `prevFactors`, so the previous silhouette welds as watertight as the current one.
//
// The dice contract `tess_scan.slang` defines and this kernel must emit exactly: the scan resolves the
// driving factor into `4^levels` barycentric SUBPATCHES (the SPLIT pass, which lifts the manual-cap
// ceiling for extreme near-field triangles: `perTri.z = leafLevel`, `perTri.w = levels`), each diced at
// `leafLevel <= TESS_MAX_DICE_FACTOR` into `(L+1)(L+2)/2` verts + `L²` tris. This kernel enumerates the
// subpatches (`subpatchCorners`), dices each leaf in the parent frame, and packs them contiguously so
// each leaf is a CLAS-sized cluster. Watertight by construction: (a) sibling subpatches share their
// interior split edges — the parent-relative corners are exact dyadic midpoints, identical on both, so
// the plain leaf grid places matching boundary verts; (b) the parent's OUTER edges weld with the
// neighbouring base triangle because every boundary micro-vertex is snapped onto that edge's SHARED
// per-edge factor's segment grid (`snapEdgeParam`, geomorphed floor→ceil), a pure function of the two
// endpoints regardless of either side's split depth — extra grid points collapse onto shared segment
// points as degenerate slivers, never a gap.
//
// Output is mesh-LOCAL: every graphics pass applies `inst.model` and the RT BLAS places the instance at
// its node matrix. `sa::Vertex` is 48 B (position@0, normal@12, uv0@24, tangent@32), read and written
// through ByteAddressBuffer so std430 packing matches the engine `#[repr(C)]` layout.

// Bindless albedo/height array (shared set 0, matches the übershader), sampled by index.
[[vk::binding(0, 0)]] Sampler2D albedoTextures[1024];

// The emit buffers.
[[vk::binding(0, 1)]] ByteAddressBuffer baseVertices;   // base stream in (48 B): static VB or skinned ring
[[vk::binding(1, 1)]] ByteAddressBuffer baseIndices;    // base index stream (u32) — the triangle corners
[[vk::binding(2, 1)]] StructuredBuffer<uint4> perTri;   // (vertOffset, indexOffset, L, pad) per base triangle
[[vk::binding(3, 1)]] ByteAddressBuffer triEdges;       // TriEdges[] (16 B): e[3] unique-edge ids per triangle
[[vk::binding(4, 1)]] StructuredBuffer<float> factors;  // per-edge fractional factors (instance slice, current frame)
[[vk::binding(5, 1)]] RWByteAddressBuffer outVertices;  // amplified micro-vertices (48 B stride)
[[vk::binding(6, 1)]] RWByteAddressBuffer outIndices;   // generated index stream (u32)
// The SAME per-edge factor slice as `factors` but from the PREVIOUS frame (double-buffered on the CPU;
// identical to `factors` on frame 0 or a layout change, giving zero geomorph delta).
[[vk::binding(7, 1)]] StructuredBuffer<float> prevFactors; // per-edge factors, previous frame (instance slice)
// The previous-frame micro-vertex positions: each slot placed with `prevFactors` on the SAME cur-`L` grid,
// so it maps 1:1 onto `outVertices`/`outIndices`. The motion prepass binds this as its prev-position stream,
// reading only the position (offset 0); the normal/uv/tangent mirror the current vertex to fill the stride.
[[vk::binding(8, 1)]] RWByteAddressBuffer outPrevVertices;

static const uint VertexStride = 48u;
static const uint NormalOffset = 12u;
static const uint UvOffset = 24u;
static const uint TangentOffset = 32u;
static const uint EdgeStride = 16u;

struct Push
{
    uint triBase;      // this instance's base offset into perTri / triEdges
    uint vertexBase;   // this instance's reserved vertex slice base (the draw's vertexOffset)
    uint indexBase;    // this instance's reserved index slice base (the draw's firstIndex)
    uint heightIndex;  // bindless index of the height map
    float heightScale; // LOCAL-space displacement amplitude — scales with the instance like its geometry
    float factorCap;   // hard dice cap (matches the scan's)
    uint vectorIndex;  // bindless index of the vector-displacement map (0 = scalar)
    uint pad3;
    float4 uvTransform; // tiling.xy, offset.xy
    uint factorBase;   // this instance's base offset into factors
    uint pad0;
    uint pad1;
    uint pad2;
};
[[vk::push_constant]] Push push;

struct Corner
{
    float3 position;
    float3 normal;
    float2 uv;
    float4 tangent;
};

Corner loadCorner(uint vertexIndex)
{
    const uint b = vertexIndex * VertexStride;
    Corner c;
    c.position = baseVertices.Load<float3>(b);
    c.normal = normalize(baseVertices.Load<float3>(b + NormalOffset));
    c.uv = baseVertices.Load<float2>(b + UvOffset);
    c.tangent = baseVertices.Load<float4>(b + TangentOffset);
    return c;
}

// Mip-aware height sampling: a LOD matched to the micro-edge texel density, so a coarse micro-grid
// prefilters the height field instead of point-sampling full resolution below Nyquist (the aliasing
// that reads as "spiky"). `lod` is computed once per base triangle in `computeMain`.
float sampleHeight(float2 uv, float lod)
{
    return albedoTextures[NonUniformResourceIndex(push.heightIndex)].SampleLevel(uv, lod).r;
}

float3 sampleVector(float2 uv, float lod)
{
    return albedoTextures[NonUniformResourceIndex(push.vectorIndex)].SampleLevel(uv, lod).xyz * 2.0 - 1.0;
}

// The height-map LOD for a base triangle diced to level `L`: the coarser the micro-grid relative to the
// (tiled) texel density, the higher the mip, so each micro-vertex reads a prefiltered height rather than
// aliasing the full-resolution field. Uses the widest base-UV edge as the representative span.
float heightSampleLod(Corner c0, Corner c1, Corner c2, uint L)
{
    uint tw, th;
    albedoTextures[NonUniformResourceIndex(push.heightIndex)].GetDimensions(tw, th);
    const float2 tiling = push.uvTransform.xy;
    const float uvEdge = max(length((c1.uv - c0.uv) * tiling),
                             max(length((c2.uv - c1.uv) * tiling),
                                 length((c0.uv - c2.uv) * tiling)));
    const float texelsPerMicroEdge = uvEdge * (float)tw / (float)max(L, 1u);
    return max(0.0, log2(max(texelsPerMicroEdge, 1.0)));
}

// Project `x` onto the tangent plane of corner `i` (Phong tessellation, Boubekeur & Alexa).
float3 projectToPlane(float3 x, float3 p, float3 n)
{
    return x - n * dot(x - p, n);
}

// The Phong-smoothed base position at barycentric weights (w0, w1, w2) — a function that, on a shared
// edge (one weight zero), depends only on that edge's two endpoints + normals, so both incident
// triangles evaluate a bit-identical edge curve (watertight base).
float3 phongPosition(Corner c0, Corner c1, Corner c2, float3 w)
{
    const float3 flat = w.x * c0.position + w.y * c1.position + w.z * c2.position;
    const float3 p0 = projectToPlane(flat, c0.position, c0.normal);
    const float3 p1 = projectToPlane(flat, c1.position, c1.normal);
    const float3 p2 = projectToPlane(flat, c2.position, c2.normal);
    return w.x * p0 + w.y * p1 + w.z * p2;
}

// The displaced local-space position at barycentric weights. Scalar-along-normal or tangent-space
// vector. The amplitude is LOCAL-space: the displaced surface rides the instance's model matrix like
// every other vertex, so gizmo-scaling an object scales its relief proportionally (the Blender/UE
// convention — bumps keep their proportion to the object, never shred a shrunk mesh or flatten a grown
// one).
float3 displacedPosition(Corner c0, Corner c1, Corner c2, float3 w, float lod)
{
    const float3 base = phongPosition(c0, c1, c2, w);
    const float3 n = normalize(w.x * c0.normal + w.y * c1.normal + w.z * c2.normal);
    const float2 uv0 = w.x * c0.uv + w.y * c1.uv + w.z * c2.uv;
    const float2 uv = uv0 * push.uvTransform.xy + push.uvTransform.zw;
    const float localScale = push.heightScale;

    if (push.vectorIndex != 0u)
    {
        const float4 tan4 = w.x * c0.tangent + w.y * c1.tangent + w.z * c2.tangent;
        const float3 t = normalize(tan4.xyz - n * dot(n, tan4.xyz));
        const float3 b = cross(n, t) * tan4.w;
        const float3 v = sampleVector(uv, lod) * localScale;
        return base + t * v.x + b * v.y + n * v.z;
    }
    const float h = sampleHeight(uv, lod);
    return base + n * (h * localScale);
}

// The interpolated base UV at barycentric weights (stored raw on the micro-vertex; the material's
// uvTransform is applied only for sampling).
float2 baseUv(Corner c0, Corner c1, Corner c2, float3 w)
{
    return w.x * c0.uv + w.y * c1.uv + w.z * c2.uv;
}

// The tangent handedness at barycentric weights (from the base corners; preserved so mirrored UVs do
// not invert).
float tangentHandedness(Corner c0, Corner c1, Corner c2, float3 w)
{
    const float hw = w.x * c0.tangent.w + w.y * c1.tangent.w + w.z * c2.tangent.w;
    return hw >= 0.0 ? 1.0 : -1.0;
}

// Hermite smoothstep on [0,1] — the shared geomorph ramp (matches `smoothstep01` on the CPU).
float smoothstep01(float t)
{
    const float c = clamp(t, 0.0, 1.0);
    return c * c * (3.0 - 2.0 * c);
}

// Geomorph a boundary parameter `p in [0,1]` along an edge onto its shared per-edge segment grid.
// Snapping to a single integer segment count pops when the factor crosses an integer, so blend
// between the floor and ceil segment counts by a smoothstep of the fractional factor:
// as `factor` sweeps N → N+1, the boundary point slides continuously from the N-segment placement to the
// (N+1)-segment one. Both incident triangles read the SAME shared per-edge `factor`, so they compute the
// bit-identical blended position — watertight through the transition, not just at rest.
float snapEdgeParam(float p, float factor)
{
    const float f = clamp(factor, 1.0, push.factorCap);
    const float segsLo = max(floor(f), 1.0);
    const float segsHi = min(segsLo + 1.0, push.factorCap);
    const float pLo = round(p * segsLo) / segsLo;
    const float pHi = round(p * segsHi) / segsHi;
    return lerp(pLo, pHi, smoothstep01(f - segsLo));
}

// Barycentric weights over the base triangle of the integer grid vertex `(gi, gj)` on a grid of level
// `levelF` (the coarse `L-1` grid for the interior geomorph). `w = (1-a-b, a, b)`.
float3 gridBary(float gi, float gj, float levelF)
{
    const float a = gi / levelF;
    const float b = gj / levelF;
    return float3(1.0 - a - b, a, b);
}

// The PARENT-relative barycentric corners of subpatch `s` in the SPLIT pass. A base triangle whose
// driving factor exceeds `TESS_MAX_DICE_FACTOR` is subdivided `levels` times (4 sub-triangles via edge
// midpoints per level) into `4^levels` subpatches; this returns subpatch `s`'s three corners as
// barycentric coords WITHIN the parent (parent identity = corners (1,0,0),(0,1,0),(0,0,1)). Each level's
// most-significant base-4 digit of `s` selects a child. Every corner is an exact dyadic rational (repeated
// `0.5*(x+y)` of exact values), and `0.5*(x+y)` is commutative, so two subpatches that share a corner or a
// split edge compute bit-identical corner coords — the structural guarantee that sibling subpatches place
// identical boundary micro-vertices along their shared interior split edges (WATERTIGHT by construction).
void subpatchCorners(uint s, uint levels, out float3 SA, out float3 SB, out float3 SC)
{
    SA = float3(1.0, 0.0, 0.0);
    SB = float3(0.0, 1.0, 0.0);
    SC = float3(0.0, 0.0, 1.0);
    for (uint k = 0u; k < levels; ++k)
    {
        const uint d = (s >> (2u * (levels - 1u - k))) & 3u; // most-significant digit first
        const float3 mAB = 0.5 * (SA + SB);
        const float3 mBC = 0.5 * (SB + SC);
        const float3 mCA = 0.5 * (SC + SA);
        if (d == 0u) { SB = mAB; SC = mCA; }        // corner A child (A, mAB, mCA)
        else if (d == 1u) { SA = mAB; SC = mBC; }   // corner B child (mAB, B, mBC)
        else if (d == 2u) { SA = mCA; SB = mBC; }   // corner C child (mCA, mBC, C)
        else { SA = mAB; SB = mBC; SC = mCA; }      // centre child (mAB, mBC, mCA)
    }
}

// Map a subpatch-LOCAL barycentric weight (la = corner-1, lb = corner-2) to the PARENT barycentric frame
// through the subpatch's corners — the frame every `displacedPosition` / Phong sample is evaluated in, so
// the emitted surface is identical to what an un-split dice of the same grid would place there.
float3 localToParent(float la, float lb, float3 SA, float3 SB, float3 SC)
{
    return (1.0 - la - lb) * SA + la * SB + lb * SC;
}

// The COARSE-PARENT displaced position for a subpatch-local point `(la, lb)` (the interior geomorph):
// the position the previous integer LEAF level `coarseLevel = leafLevel-1` places at the same point — the
// linear interpolation across the `coarseLevel` micro-triangle (in SUBPATCH-local coords) that contains it,
// its three local corners mapped to the parent frame and displaced (sampled at `coarseLod`, the prefilter
// the coarse dice would use), then barycentric-blended. For the un-split path the subpatch corners are the
// parent identity, so this reduces to the plain parent coarse grid. Valid for any interior sample (and its
// FD neighbours): an interior point has local `1-la-lb > 0`, so `I + J < coarseLevel`, keeping every
// referenced corner (incl. the down-triangle apex) inside the local grid.
float3 coarseParentPositionLocal(Corner c0, Corner c1, Corner c2, float la, float lb, float3 SA, float3 SB, float3 SC, float coarseLod, uint coarseLevel)
{
    const float levelF = (float)coarseLevel;
    const float I = la * levelF; // corner-1 axis, coarse local-grid coords
    const float J = lb * levelF; // corner-2 axis, coarse local-grid coords
    const float i0 = floor(I);
    const float j0 = floor(J);
    const float fi = I - i0;
    const float fj = J - j0;

    float3 lA; // local corner barycentrics (1-a-b, a, b) on the coarse grid
    float3 lB;
    float3 lC;
    float wA;
    float wB;
    float wC;
    if (fi + fj <= 1.0)
    {
        // Up-pointing micro-triangle: corners (i0,j0), (i0+1,j0), (i0,j0+1).
        lA = gridBary(i0, j0, levelF);
        lB = gridBary(i0 + 1.0, j0, levelF);
        lC = gridBary(i0, j0 + 1.0, levelF);
        wA = 1.0 - fi - fj;
        wB = fi;
        wC = fj;
    }
    else
    {
        // Down-pointing micro-triangle: corners (i0+1,j0), (i0,j0+1), (i0+1,j0+1).
        lA = gridBary(i0 + 1.0, j0, levelF);
        lB = gridBary(i0, j0 + 1.0, levelF);
        lC = gridBary(i0 + 1.0, j0 + 1.0, levelF);
        wA = 1.0 - fj;
        wB = 1.0 - fi;
        wC = fi + fj - 1.0;
    }
    // Map each coarse local corner to the parent frame before displacing.
    const float3 pA = displacedPosition(c0, c1, c2, localToParent(lA.y, lA.z, SA, SB, SC), coarseLod);
    const float3 pB = displacedPosition(c0, c1, c2, localToParent(lB.y, lB.z, SA, SB, SC), coarseLod);
    const float3 pC = displacedPosition(c0, c1, c2, localToParent(lC.y, lC.z, SA, SB, SC), coarseLod);
    return wA * pA + wB * pB + wC * pC;
}

// The INTERIOR-geomorph position for a subpatch-local point: blend the coarse-parent (`leafLevel-1`)
// approximation toward the fine (`leafLevel`) displaced surface by `blend = smoothstep01(frac)` (`frac` =
// fractional part of the LEAF factor). `blend >= 1` — boundary verts (see the call site), or a factor
// pinned at an integer / the cap — short-circuits to the pure fine position, so a boundary weld is
// byte-identical to the non-geomorphed path (no shared-edge crack) and a saturated leaf keeps full detail.
// A vertex on a subpatch's interior split edge is shared with a sibling that dices the same coarse/fine
// grids over bit-identical shared corners, so both siblings blend to the same position — watertight.
//
// Continuity within a leaf-factor band mirrors the un-split geomorph: smooth as `frac` sweeps `0 -> 1`,
// continuous-not-bit-exact across a band boundary (successive integer leaf grids are distinct, not nested).
float3 geomorphLocal(Corner c0, Corner c1, Corner c2, float la, float lb, float3 SA, float3 SB, float3 SC, float fineLod, float coarseLod, uint coarseLevel, float blend)
{
    const float3 fine = displacedPosition(c0, c1, c2, localToParent(la, lb, SA, SB, SC), fineLod);
    if (blend >= 1.0)
    {
        return fine;
    }
    const float3 coarse = coarseParentPositionLocal(c0, c1, c2, la, lb, SA, SB, SC, coarseLod, coarseLevel);
    return lerp(coarse, fine, blend);
}

// Dice ONE subpatch (a leaf of the split, or the whole base triangle when un-split) into its barycentric
// micro-grid at leaf level `L`, evaluated in the parent frame through the subpatch corners `SA/SB/SC`.
// `emitLeafVertex` places ONE micro-vertex and `emitLeafTriangle` writes ONE micro-triangle;
// `computeMain`'s 64 lanes stride the flattened (subpatch × local) index space, so a heavily split
// triangle never serializes onto a single thread — that device-loses at high caps.
// WATERTIGHT: a micro-vertex on a PARENT outer edge (detected exactly via integer weights + the dyadic
// subpatch corners) is snapped onto that edge's SHARED per-edge factor's segment grid AND samples the
// height at that edge's SHARED LOD (`edgeLodFor` — a pure function of the two shared endpoints + the
// shared factor; a base-mesh corner samples at LOD 0) — identical from the neighbouring base triangle
// regardless of split depth or its differing per-triangle interior LOD. A micro-vertex on an interior
// split edge is placed by the plain leaf grid over bit-identical shared corners — identical from the
// sibling subpatch.

// The shared LOD for height samples ON an edge: texel span of the edge over its (continuous) segment
// count. A pure function of (uvA, uvB, factor) — both incident triangles agree bit-for-bit on a non-seam
// edge, so the boundary weld samples the same mip from both sides (the per-triangle interior LOD may not).
float edgeLodFor(float2 uvA, float2 uvB, float f)
{
    uint tw;
    uint th;
    albedoTextures[NonUniformResourceIndex(push.heightIndex)].GetDimensions(tw, th);
    const float spanTex = length((uvB - uvA) * push.uvTransform.xy) * (float)tw;
    const float segs = clamp(f, 1.0, push.factorCap);
    return max(0.0, log2(max(spanTex / segs, 1.0)));
}

// Invert a leaf-local linear vertex index into its grid row `i` + column `j` (row `i` holds `L+1-i`
// vertices starting at `i*(L+1) - i*(i-1)/2`). `L <= TESS_MAX_DICE_FACTOR` keeps the loop tiny.
void vertexRowCol(uint v, uint L, out uint i, out uint j)
{
    i = 0u;
    uint rowStart = 0u;
    for (uint r = 0u; r <= L; ++r)
    {
        const uint next = rowStart + (L + 1u - r);
        if (v < next)
        {
            i = r;
            break;
        }
        rowStart = next;
    }
    j = v - (i * (L + 1u) - i * (i - 1u) / 2u);
}

// Place ONE micro-vertex of one subpatch (leaf grid position (i, j) at level `L`): boundary snap +
// geomorph + displacement + FD normal/tangent, current AND previous streams. Mirrors the serial kernel
// exactly — same math, same write slots — only the iteration moved to the caller.
void emitLeafVertex(
    Corner c0, Corner c1, Corner c2,
    float3 SA, float3 SB, float3 SC,
    uint L, uint subVertBase,
    float f01, float f12, float f20,
    float pf01, float pf12, float pf20,
    float heightLod, float coarseLod, uint coarseLevel,
    float edgeLod01, float edgeLod12, float edgeLod20,
    float interiorBlend, float interiorBlendPrev,
    uint i, uint j)
{
    const float invL = 1.0 / (float)L;
    const float eps = invL * 0.5;
    const uint rowStart = i * (L + 1u) - i * (i - 1u) / 2u;
    const float la = (float)i * invL; // local corner-1 weight
    const float lb = (float)j * invL; // local corner-2 weight

    // Parent-frame weight from EXACT integer local weights × the dyadic corners. Because every term
    // is non-negative, a parent component is exactly 0 iff the vertex lies on that parent edge — the
    // exact boundary test (no float tolerance), so the outer-edge snap fires for every incident
    // triangle identically (incl. an isolated subpatch corner sitting on a parent edge).
    const float wi0 = (float)(L - i - j);
    const float wi1 = (float)i;
    const float wi2 = (float)j;
    const float3 wScaled = wi0 * SA + wi1 * SB + wi2 * SC; // parent weight × L (sums to L)
    const float3 wRaw = wScaled * invL;                    // raw parent barycentric (sums to 1)
    const bool onE01 = (wScaled.z == 0.0); // parent edge 0-1 (opposite corner 2)
    const bool onE12 = (wScaled.x == 0.0); // parent edge 1-2 (opposite corner 0)
    const bool onE20 = (wScaled.y == 0.0); // parent edge 2-0 (opposite corner 1)
    const bool corner = ((onE01 ? 1u : 0u) + (onE12 ? 1u : 0u) + (onE20 ? 1u : 0u)) >= 2u;

    // Boundary snap onto the SHARED per-edge factor's segment grid, in the SAME parent parametrisation
    // the un-split kernel used — so a split triangle welds with an un-split (or differently-split)
    // neighbour bit-for-bit as the baseline did. Interior verts (incl. interior split edges) are not
    // snapped: they morph coarse→fine and match their sibling by construction.
    const bool interior = !(onE01 || onE12 || onE20);
    float3 wSnap = wRaw;
    // The height LOD for a boundary sample must be a pure function of the SHARED feature: the edge's own
    // LOD (both incident triangles compute it identically), or LOD 0 at a base-mesh corner. The
    // per-triangle `heightLod` (widest-edge heuristic) may differ across the edge — using it on a
    // boundary vertex was a crack (same snapped position, different mip ⇒ different height).
    float lodSel = heightLod;
    if (onE01)
    {
        const float p = snapEdgeParam(wRaw.y, f01);
        wSnap = float3(1.0 - p, p, 0.0);
        lodSel = edgeLod01;
    }
    else if (onE12)
    {
        const float p = snapEdgeParam(wRaw.z, f12);
        wSnap = float3(0.0, 1.0 - p, p);
        lodSel = edgeLod12;
    }
    else if (onE20)
    {
        const float p = snapEdgeParam(wRaw.z, f20);
        wSnap = float3(1.0 - p, 0.0, p);
        lodSel = edgeLod20;
    }
    if (corner)
    {
        lodSel = 0.0;
    }
    const float3 wShade = interior ? wRaw : wSnap; // uv / normal-orient / tangent frame weight
    const float vertBlend = interior ? interiorBlend : 1.0;

    // Interior verts morph coarse→fine on the SUBPATCH's local grid; boundary verts take the pure
    // (snapped) fine position at the shared edge LOD so the weld is exact.
    const float3 base = geomorphLocal(c0, c1, c2, la, lb, SA, SB, SC, heightLod, coarseLod, coarseLevel, vertBlend);
    const float3 displaced = interior ? base : displacedPosition(c0, c1, c2, wSnap, lodSel);

    // Normal + tangent from the full displaced-surface Jacobian (finite differences in LOCAL coords,
    // stepped inward at the boundary; captures base curvature AND the displacement derivative). Shading
    // only — a cross-edge mip difference here shades, never cracks.
    const float du = (la + eps <= 1.0) ? eps : -eps;
    const float dv = (lb + eps <= 1.0 - la) ? eps : -eps;
    const float3 pu = geomorphLocal(c0, c1, c2, la + du, lb, SA, SB, SC, heightLod, coarseLod, coarseLevel, vertBlend);
    const float3 pv = geomorphLocal(c0, c1, c2, la, lb + dv, SA, SB, SC, heightLod, coarseLod, coarseLevel, vertBlend);
    const float3 dPdu = (pu - base) / du;
    const float3 dPdv = (pv - base) / dv;
    float3 nrm = cross(dPdu, dPdv);
    const float3 nInterp = normalize(wShade.x * c0.normal + wShade.y * c1.normal + wShade.z * c2.normal);
    if (dot(nrm, nInterp) < 0.0)
    {
        nrm = -nrm;
    }
    nrm = normalize(nrm);

    const float handed = tangentHandedness(c0, c1, c2, wShade);
    float3 tng = dPdu - nrm * dot(nrm, dPdu);
    const float tlen = length(tng);
    tng = tlen > 1e-6 ? tng / tlen : normalize(cross(nrm, float3(0.0, 0.0, 1.0)) + float3(1e-4, 0.0, 0.0));

    const uint vIndex = push.vertexBase + subVertBase + rowStart + j;
    const uint ob = vIndex * VertexStride;
    outVertices.Store<float3>(ob, displaced);
    outVertices.Store<float3>(ob + NormalOffset, nrm);
    outVertices.Store<float2>(ob + UvOffset, baseUv(c0, c1, c2, wShade));
    outVertices.Store<float4>(ob + TangentOffset, float4(tng, handed));

    // Previous-frame position of the same micro-vertex slot: same local grid, but the boundary snap +
    // interior geomorph use the previous factors, so the cur→prev delta is exactly the geomorph slide.
    // The LOD selection is the CURRENT one for both streams — per-slot-consistent and still a pure
    // function of the shared feature, so the previous silhouette welds too.
    float3 wSnapPrev = wRaw;
    if (onE01)
    {
        const float p = snapEdgeParam(wRaw.y, pf01);
        wSnapPrev = float3(1.0 - p, p, 0.0);
    }
    else if (onE12)
    {
        const float p = snapEdgeParam(wRaw.z, pf12);
        wSnapPrev = float3(0.0, 1.0 - p, p);
    }
    else if (onE20)
    {
        const float p = snapEdgeParam(wRaw.z, pf20);
        wSnapPrev = float3(1.0 - p, 0.0, p);
    }
    const float prevBlend = interior ? interiorBlendPrev : 1.0;
    const float3 displacedPrev = interior
        ? geomorphLocal(c0, c1, c2, la, lb, SA, SB, SC, heightLod, coarseLod, coarseLevel, prevBlend)
        : displacedPosition(c0, c1, c2, wSnapPrev, lodSel);
    // The motion prepass reads only the prev position (offset 0); mirror the current normal/uv/tangent.
    outPrevVertices.Store<float3>(ob, displacedPrev);
    outPrevVertices.Store<float3>(ob + NormalOffset, nrm);
    outPrevVertices.Store<float2>(ob + UvOffset, baseUv(c0, c1, c2, wShade));
    outPrevVertices.Store<float4>(ob + TangentOffset, float4(tng, handed));
}

// Write ONE of a subpatch's L² micro-triangles: linear slot `k` inverts to (row, up/down pair) in the
// exact order the serial kernel emitted (row-major, up/down interleaved), so slot `k`'s indices land at
// `subIndexBase + k*3`. Index values are RELATIVE to the instance vertex slice base (`subVertBase +
// local`); the draw's `vertexOffset = vertexBase` adds the slice base.
void emitLeafTriangle(uint L, uint subVertBase, uint subIndexBase, uint k)
{
    // Row r contributes 2*(L-r)-1 triangles; find the row containing slot k.
    uint i = 0u;
    uint cum = 0u;
    for (uint r = 0u; r < L; ++r)
    {
        const uint rowTris = 2u * (L - r) - 1u;
        if (k < cum + rowTris)
        {
            i = r;
            break;
        }
        cum += rowTris;
    }
    const uint local = k - cum;
    const uint j = local / 2u;
    const uint rowStart = i * (L + 1u) - i * (i - 1u) / 2u;
    const uint nextStart = (i + 1u) * (L + 1u) - (i + 1u) * i / 2u;
    const uint vA = subVertBase + rowStart + j;
    const uint vB = subVertBase + rowStart + j + 1u;
    const uint vC = subVertBase + nextStart + j;
    const uint tri = push.indexBase + subIndexBase + k * 3u;
    if ((local & 1u) == 0u)
    {
        // Up-pointing.
        outIndices.Store<uint>(tri * 4u, vA);
        outIndices.Store<uint>((tri + 1u) * 4u, vC);
        outIndices.Store<uint>((tri + 2u) * 4u, vB);
    }
    else
    {
        // Down-pointing companion.
        const uint vD = subVertBase + nextStart + j + 1u;
        outIndices.Store<uint>(tri * 4u, vB);
        outIndices.Store<uint>((tri + 1u) * 4u, vC);
        outIndices.Store<uint>((tri + 2u) * 4u, vD);
    }
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 gtid: SV_GroupThreadID, uint3 gid: SV_GroupID)
{
    const uint t = gid.x;    // base-triangle index within this instance's dispatch (one workgroup each)
    const uint lane = gtid.x; // 64 lanes stride the flattened (subpatch × local) work of this triangle
    const uint4 rec = perTri[push.triBase + t];
    const uint vertOffset = rec.x;       // packed vertex offset within the instance's reserved slice
    const uint indexOffset = rec.y;      // packed index offset within the instance's reserved slice
    const uint L = max(rec.z, 1u);       // leaf dice level (<= TESS_MAX_DICE_FACTOR, a CLAS-sized leaf)
    const uint levels = rec.w;           // split depth: 4^levels subpatches (0 = un-split, one leaf)

    // The base triangle's three corners.
    const uint iBase = (push.triBase + t) * 3u;
    const uint i0 = baseIndices.Load<uint>(iBase * 4u);
    const uint i1 = baseIndices.Load<uint>((iBase + 1u) * 4u);
    const uint i2 = baseIndices.Load<uint>((iBase + 2u) * 4u);
    const Corner c0 = loadCorner(i0);
    const Corner c1 = loadCorner(i1);
    const Corner c2 = loadCorner(i2);

    // The three shared per-edge factors (edges (0-1), (1-2), (2-0)), used to snap boundary micro-verts.
    const uint eb = (push.triBase + t) * EdgeStride;
    const uint e0 = triEdges.Load<uint>(eb);
    const uint e1 = triEdges.Load<uint>(eb + 4u);
    const uint e2 = triEdges.Load<uint>(eb + 8u);
    const float f01 = factors[push.factorBase + e0];
    const float f12 = factors[push.factorBase + e1];
    const float f20 = factors[push.factorBase + e2];

    // The three PREVIOUS-frame per-edge factors (same slice layout as `factors`; identical to it on frame 0
    // / a layout change ⇒ prev == cur ⇒ zero geomorph motion). Drive the prev boundary snap + prev interior
    // geomorph on the current grid, so the prev micro-vertex slots map 1:1 onto the current ones.
    const float pf01 = prevFactors[push.factorBase + e0];
    const float pf12 = prevFactors[push.factorBase + e1];
    const float pf20 = prevFactors[push.factorBase + e2];

    // Interior geomorph, per-LEAF: each INTERIOR micro-vertex blends from the coarser `L-1` leaf
    // grid (frac → 0) to the fine `L` displaced surface (frac → 1) by `smoothstep01(frac)` of the LEAF
    // factor's fractional part; boundary verts keep their `snapEdgeParam` weld. `frac` is measured against
    // `L-1` so an exact-integer / cap-saturated leaf resolves to full fine detail.
    const float splitScale = 1.0 / (float)(1u << levels);
    const float maxFactor = min(max(f01, max(f12, f20)), push.factorCap);
    const float leafFactor = maxFactor * splitScale;
    const float interiorBlend = smoothstep01(clamp(leafFactor - ((float)L - 1.0), 0.0, 1.0));
    const float maxFactorPrev = min(max(pf01, max(pf12, pf20)), push.factorCap);
    const float leafFactorPrev = maxFactorPrev * splitScale;
    const float interiorBlendPrev = smoothstep01(clamp(leafFactorPrev - ((float)L - 1.0), 0.0, 1.0));
    const uint coarseLevel = max(L, 2u) - 1u;      // L-1, floored at 1 (L=1 has no interior)

    // Mip-aware height LOD, matched to the EFFECTIVE full-grid micro-edge density (2^levels · L across the
    // parent edge), and the coarse leaf's density for the geomorph prefilter — the INTERIOR LOD. Boundary
    // verts use the shared per-edge LODs below instead (watertight across differing neighbours).
    const uint fullLevel = (1u << levels) * L;
    const uint coarseFull = (1u << levels) * coarseLevel;
    const float heightLod = heightSampleLod(c0, c1, c2, fullLevel);
    const float coarseLod = heightSampleLod(c0, c1, c2, coarseFull);
    const float edgeLod01 = edgeLodFor(c0.uv, c1.uv, f01);
    const float edgeLod12 = edgeLodFor(c1.uv, c2.uv, f12);
    const float edgeLod20 = edgeLodFor(c2.uv, c0.uv, f20);

    // Per-subpatch reserved sub-slices (contiguous within the base triangle's packed slice), so each
    // subpatch is a self-contained CLAS-sized cluster a cluster-acceleration backend can point at
    // directly. The lanes stride the flattened vertex work, then the flattened index work — independent
    // writes, no barrier.
    const uint vertsPerLeaf = (L + 1u) * (L + 2u) / 2u;
    const uint trisPerLeaf = L * L;
    const uint subpatchCount = 1u << (2u * levels);

    const uint totalVerts = subpatchCount * vertsPerLeaf;
    for (uint w = lane; w < totalVerts; w += 64u)
    {
        const uint s = w / vertsPerLeaf;
        const uint v = w - s * vertsPerLeaf;
        float3 SA;
        float3 SB;
        float3 SC;
        subpatchCorners(s, levels, SA, SB, SC);
        uint gi;
        uint gj;
        vertexRowCol(v, L, gi, gj);
        emitLeafVertex(
            c0, c1, c2, SA, SB, SC, L,
            vertOffset + s * vertsPerLeaf,
            f01, f12, f20, pf01, pf12, pf20,
            heightLod, coarseLod, coarseLevel,
            edgeLod01, edgeLod12, edgeLod20,
            interiorBlend, interiorBlendPrev,
            gi, gj);
    }

    const uint totalTris = subpatchCount * trisPerLeaf;
    for (uint w = lane; w < totalTris; w += 64u)
    {
        const uint s = w / trisPerLeaf;
        const uint k = w - s * trisPerLeaf;
        emitLeafTriangle(L, vertOffset + s * vertsPerLeaf, indexOffset + s * trisPerLeaf * 3u, k);
    }
}
