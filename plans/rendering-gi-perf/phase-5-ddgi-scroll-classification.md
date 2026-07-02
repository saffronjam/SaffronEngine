# Phase 5 — DDGI scroll + probe classification (artifact B)

**Status:** LIKELY NOT NEEDED — verify with the user before building. A measurement-driven check
(forward-walk toward the lion wall on `dev`, sky-occ on) shows the wall/arch/lion **correctly lit with
proper GI fill at mid-approach** — no spurious darkening. The heavy darkening only appears nose-to-wall
where the camera enters the genuinely-dark lion alcove (legitimate, not an artifact). Combined with the
earlier forward-walk showing the settle-swim residual dropping ~10× (0.13–0.22 → 0.01–0.06) after the
DDGI FIX 4/5 (blend only re-traced probes + adaptive hysteresis) + the surface-bias (artifact-A fix),
**artifact B appears resolved.** Do NOT build the complex scroll-clear + classification/relocation
speculatively. If the user still sees B under live interactive motion, revisit this phase then.

Artifact B: the far wall + arches **darken as the camera approaches**. The toroidal scroll is
*correct*; the causes are (1) probes scrolling into Sponza walls/columns inject dark sun-occluded
irradiance into the blend, with no classification to exclude them; (2) the whole-volume quarter
round-robin re-lights a scrolled-in slab over several frames → a trailing dark front under motion.

## Changes

### 5.1 — Scroll-clear only the newly-exposed plane (RTXGI `DDGIClearScrolledPlane`)
- **Where:** `ddgi_blend_irradiance.slang` `probeReset` (~60-70) + `ddgi.rs` `scroll_base`.
- **Change:** reset + hard-clear **only** the exact 1-cell boundary plane that crossed on this
  recenter (hysteresis 0 the first cycle it is re-rayed), decoupled from the quarter-volume
  round-robin. Plane math must match the `wrapMod` / `(pi + scrollBase) % count` fold.
- **Effect:** removes cell-crossing "breathing" and shrinks the darkening front.

### 5.2 — Scroll budget coupled to camera speed
- **Where:** `ddgi.rs` `DDGI_PROBE_BUDGET` / `set_scene` (~485-511).
- **Change:** when `snap_base` moves > N cells in a frame, transiently raise the trace budget (or
  fully re-trace the exposed slab that frame) so newly-exposed probes are lit immediately, not over
  the 4-frame cycle. Gate to genuine large recenters so static-camera trace cost stays bounded.

### 5.3 — Probe classification + relocation (RTXGI)
- **Where:** `ddgi_trace.slang` (backface-hit path ~188-205: on a backface hit write irradiance 0,
  shorten stored depth ~80 %, tally the backface-hit ratio); a new per-probe state buffer
  (offset + active flag); `ddgiSampleIrradiance` (exclude inactive probes from `covsum` + `wsum`);
  `probeWorldPos` / `ddgiAtlasUv` (apply the relocation offset).
- **Change:** mark probes with > ~25 % backface hits **inactive** (do not feed the blend); relocate
  a probe ≤ 0.45·spacing toward open space — **static geometry only**, never around dynamic bodies.
- **Effect:** the primary correctness win for B near walls; also cleans residual A at corners.

## Risks / watch
- Keep the toroidal fold consistent across trace / blend / sample when adding the offset + state
  buffer. Never relocate around dynamic geometry (instability).

## Verification
- Walk-toward-wall + near-column captures on `dev`: the far wall no longer darkens on approach; no
  dark cells at columns; no moving dark front on a fast walk. `ddgi-trace` ms bounded when static.
- Validation-clean; `just engine` + lint clean.
