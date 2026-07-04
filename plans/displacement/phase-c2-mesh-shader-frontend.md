# Phase C2 — optional task+mesh-shader tessellation front end

**Status:** NOT STARTED (roadmap-gated — no mesh-shader pipeline exists yet)
**Scope:** `saffron-rendering`
**Depends on:** phase-b2/c1 + a mesh-shader / GPU-driven-culling roadmap that does not yet exist

## Goal

Migrate the compute tessellator's *front end* to a **task + mesh-shader (`VK_EXT_mesh_shader`)
amplification path** for finer per-meshlet LOD and in-pipeline geometry generation — the model that
officially replaces the fixed-function tessellator. Keep the compute-buffer → BLAS path for RT.

## Approach

The task (amplification) shader is a programmable, unconstrained tessellator: it dispatches a variable
number of mesh workgroups per LOD, and the mesh shader emits displaced meshlets. This composes with the
GPU-driven culling / mesh-shader work AGENTS.md lists as "not yet" — do it **only** once that roadmap is
underway, so the whole front end is built once on mesh shaders rather than retrofitted.

## Touch points

- A task+mesh pipeline stage in `saffron-rendering`; meshlet-based adaptive displacement; per-primitive
  data via `PerPrimitiveEXT`.

## Verification

- Displaced meshlets match the compute-path output at equal LOD; per-meshlet LOD reduces triangle count
  on distant geometry.

## Notes

- Deferred and optional. The compute-to-buffer path (B2) + BLAS refit (C1) is the correct, portable
  system; this phase is a performance/architecture upgrade gated on the mesh-shader roadmap, not a
  requirement.
