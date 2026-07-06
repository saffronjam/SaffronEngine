# Phase B3 — vector displacement (overhangs / undercuts)

**Status:** IMPLEMENTED. The prerequisite **UV-aligned per-vertex tangent stream** was built first, then
B3 rode on top of it.

**Tangent stream (prerequisite, built):** `saffron_geometry::Vertex` widened 32→48 B to carry
`tangent: [f32; 4]` (xyz tangent + w = ±1 bitangent handedness, glTF convention) at offset 32;
`MESH_FORMAT_VERSION` 3→4. `compute_tangents(&mut Mesh)` (Lengyel's method + Gram-Schmidt, with a
branchless fallback for degenerate/UV-less verts) runs on every import: glTF reads the TANGENT accessor
when present and recomputes only when absent; OBJ + the built-in primitives always compute. Every base
Vertex stride derives from `size_of::<Vertex>()` and every attribute offset from `offset_of!`, so the
widening propagated automatically to the vertex buffers, the RT BLAS vertex stride, and the deformed
buffer. The graphics vertex layout gains a `[[vk::location(3)]]` tangent on binding 0; the skinned stream
renumbered to loc 4/5. The **skin / morph / displace** compute kernels read the tangent, rotate/carry it,
and write the full 48-byte vertex to the deformed buffer, so a skinned/morphed/displaced mesh keeps a
valid frame.

**B3 proper (built):** `MaterialAsset.vector_displacement_texture` (additive to `height_texture`; a new
`.smat` `textures.vectorDisplacement` key, `material-get`/`material-update` slot, a Material-editor slot
shown in Displacement mode). It threads through `SubmeshMaterial → DisplaceInfo → DisplaceBucket →
DisplaceDispatch → DisplacePush` as a bindless `vector_index`. `displace.slang` builds the true TBN from
the stored tangent and branches: `vector_index != 0` → `pos += t·v.x + b·v.y + n·v.z` (tangent-space XYZ
map, overhangs); else scalar-along-normal with the height-gradient bump computed in the real frame. No
separate feature bit — the vector-map presence *is* the switch.

**Scope:** `saffron-geometry` (tangent stream), `saffron-assets` (material/texture-ref set + import),
`saffron-rendering` (compute path, shaders), `saffron-protocol`/`saffron-control` + editor (slot).
**Depends on:** phase-b2 (done) + **a per-vertex tangent stream** (built as this phase's prerequisite).

## Goal

Extend the displacement path from scalar-along-normal to **tangent-space vector (XYZ)** displacement,
enabling overhangs, undercuts, and mushroom-cap / ear shapes that scalar height cannot express.

## Approach

Scalar height displaces along the interpolated normal (one channel, no undercuts). Vector displacement
stores full tangent-space XYZ per texel (3 channels, orientation-sensitive). Add it as an **additive
channel** to the material/texture-ref set — not a rewrite of scalar — so a `.smat` opts into vector
displacement with a distinct texture ref, and the compute prepass (b2) branches on it.

## Touch points

- **`saffron-assets` material model** — a vector-displacement texture ref alongside `height_texture`
  (or a mode flag on the displacement input); exposed-parameter schema entry.
- **compute prepass (b2)** — sample the XYZ field, transform by the TBN, offset the vertex; careful
  filtering (do not normalize away the offset direction).

## Verification

- A vector-displacement map produces overhangs on a subdivided base; scalar `.smat` materials are
  unaffected.

## Risks

- **Filtering** a tangent-space XYZ field without collapsing the offset direction; aliasing vs scalar
  height (open question #5).
