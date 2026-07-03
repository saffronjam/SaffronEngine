# Phase 2 — History reconstruction: Catmull-Rom + YCoCg variance clip

**Status:** COMPLETED

Part of the `plans/modern-taa-core/` feature (modern native-resolution TAA). This phase rebuilds the two
worst pieces of the resolve shader: the bilinear history fetch and the raw RGB min/max clamp. Both edits
are inside `engine/assets/shaders/taa.slang`; no Rust wiring changes (the descriptor set and push are
untouched this phase). It depends on Phase 1 — jitter must exist for a well-behaved neighborhood to mean
anything.

## Goal

Replace `history.SampleLevel(histUv, 0.0)` with an optimized Catmull-Rom bicubic reconstruction, and
replace the linear-RGB `clamp(hist, nmin, nmax)` with YCoCg **variance clipping** — accumulate the 3×3
color moments in YCoCg, build the ellipsoid `μ ± γσ`, and clip the reprojected history toward the box
center with `clip_aabb`. This kills the resample smear (sharp history) and the purple-fringe artifact
(chroma-decorrelated, tighter rejection), while staying entirely pre-tonemap in linear HDR.

## NO-LEGACY checklist for this phase

- The bilinear `history.SampleLevel(histUv, 0.0)` tap is **gone**, replaced by the Catmull-Rom sampler.
  No `#ifdef`, no "bilinear fallback".
- The raw RGB `min`/`max` neighborhood AABB and `clamp(hist, nmin, nmax)` are **gone**, replaced by the
  YCoCg moment/variance clip. The neighborhood is walked once, in YCoCg.
- The resolve still writes both `outColor` and `outHistory` and still reads the same set-0 bindings; only
  the interior math changes. No new binding, no push change (those are Phase 3).

## Engine shader: `engine/assets/shaders/taa.slang`

### Color-space helpers

1. Add `RGB_to_YCoCg` / `YCoCg_to_RGB` helpers at file scope (Karis/Playdead form):

   ```hlsl
   float3 RGB_to_YCoCg(float3 c) {
       return float3(0.25*c.r + 0.5*c.g + 0.25*c.b,
                     0.5*c.r              - 0.5*c.b,
                    -0.25*c.r + 0.5*c.g - 0.25*c.b);
   }
   float3 YCoCg_to_RGB(float3 c) {
       return float3(c.x + c.y - c.z, c.x + c.z, c.x - c.y - c.z);
   }
   ```

### Optimized Catmull-Rom history sampler

2. Add the 9-tap bilinear-optimized Catmull-Rom sampler (Pettineo/TheRealMJP form) that reconstructs a
   4×4 footprint with 9 bilinear taps from `history` at the reprojected UV. It takes the `Sampler2D`, the
   UV, and the history texture size in pixels:

   ```hlsl
   float3 SampleHistoryCatmullRom(Sampler2D tex, float2 uv, float2 texSize) {
       float2 samplePos = uv * texSize;
       float2 texPos1 = floor(samplePos - 0.5) + 0.5;
       float2 f = samplePos - texPos1;
       float2 w0 = f * (-0.5 + f * (1.0 - 0.5*f));
       float2 w1 = 1.0 + f*f * (-2.5 + 1.5*f);
       float2 w2 = f * (0.5 + f * (2.0 - 1.5*f));
       float2 w3 = f*f * (-0.5 + 0.5*f);
       float2 w12 = w1 + w2;
       float2 off12 = w2 / w12;
       float2 p0  = (texPos1 - 1.0)     / texSize;
       float2 p3  = (texPos1 + 2.0)     / texSize;
       float2 p12 = (texPos1 + off12)   / texSize;
       float3 r = 0;
       r += tex.SampleLevel(float2(p0.x,  p0.y ), 0).rgb * (w0.x  * w0.y );
       r += tex.SampleLevel(float2(p12.x, p0.y ), 0).rgb * (w12.x * w0.y );
       r += tex.SampleLevel(float2(p3.x,  p0.y ), 0).rgb * (w3.x  * w0.y );
       r += tex.SampleLevel(float2(p0.x,  p12.y), 0).rgb * (w0.x  * w12.y);
       r += tex.SampleLevel(float2(p12.x, p12.y), 0).rgb * (w12.x * w12.y);
       r += tex.SampleLevel(float2(p3.x,  p12.y), 0).rgb * (w3.x  * w12.y);
       r += tex.SampleLevel(float2(p0.x,  p3.y ), 0).rgb * (w0.x  * w3.y );
       r += tex.SampleLevel(float2(p12.x, p3.y ), 0).rgb * (w12.x * w3.y );
       r += tex.SampleLevel(float2(p3.x,  p3.y ), 0).rgb * (w3.x  * w3.y );
       return r;
   }
   ```

   > `history` is a combined `Sampler2D` with a linear sampler (per the set-0 layout / view_target.rs set
   > writes), so the bilinear taps land correctly. `texSize` is the history extent in pixels; today that
   > equals `float2(width, height)` from `outColor.GetDimensions` (input extent == output extent). Keep
   > `texSize` as its own value rather than reusing `1/texel` inline — the upsampling set will make the
   > history extent differ from the current-frame extent, and a named `texSize` is the seam it edits.
   > Clamp negative Catmull-Rom lobes with `max(r, 0.0)` before use to avoid negative HDR from ringing.

3. In `computeMain`, replace `float3 hist = history.SampleLevel(histUv, 0.0).rgb;` with
   `float3 hist = SampleHistoryCatmullRom(history, histUv, float2(width, height));`.

### YCoCg variance clip replacing the RGB min/max clamp

4. Rewrite the neighborhood loop to accumulate first and second moments in YCoCg instead of tracking
   `nmin`/`nmax` in RGB. Convert the current sample and each of the 3×3 neighbors to YCoCg; accumulate
   `m1 += y; m2 += y*y;` over the 9 samples. Keep `cur` (RGB) for the blend, but also keep `curYCoCg` for
   the clip:

   ```hlsl
   float3 m1 = 0, m2 = 0;
   for (int y = -1; y <= 1; ++y)
     for (int x = -1; x <= 1; ++x) {
        float3 s = RGB_to_YCoCg(current.SampleLevel(uv + float2(x,y)*texel, 0.0).rgb);
        m1 += s; m2 += s*s;
     }
   const float N = 9.0;
   float3 mu    = m1 / N;
   float3 sigma = sqrt(max(m2 / N - mu*mu, 0.0));
   const float gamma = 1.0;              // variance-clip tightness (~0.75..1.25; higher = softer)
   float3 aabbMin = mu - gamma * sigma;
   float3 aabbMax = mu + gamma * sigma;
   ```

5. Add the Playdead `clip_aabb` helper at file scope (clip toward the box center along the line to the
   history point):

   ```hlsl
   float3 clip_aabb(float3 aMin, float3 aMax, float3 q) {
       float3 p = 0.5*(aMax + aMin);
       float3 e = 0.5*(aMax - aMin) + 1e-5;
       float3 v = q - p;
       float3 a = abs(v / e);
       float  m = max(a.x, max(a.y, a.z));
       return m > 1.0 ? p + v / m : q;
   }
   ```

6. Convert the Catmull-Rom history to YCoCg, clip it, and convert back before the blend:

   ```hlsl
   float3 histY = RGB_to_YCoCg(hist);
   histY = clip_aabb(aabbMin, aabbMax, histY);
   hist  = YCoCg_to_RGB(histY);
   ```

7. Keep the existing disocclusion gate unchanged this phase: `weight` still comes from `push.params.x`
   and is forced to `0.0` when `push.params.y < 0.5` or `histUv` is off-screen, and the final line is
   still `float3 result = lerp(cur, hist, weight);` with the dual write to `outColor` / `outHistory`.
   (Phase 3 replaces the fixed weight and the crude gate.)

8. Update the file's top-of-file comment to describe the new spine (reproject → Catmull-Rom history →
   YCoCg variance clip → blend). NO change-journey wording ("used to be bilinear"); describe what it does
   now (per the code-style rule).

## Why no Rust changes this phase

The set-0 layout (`create_taa_layout`: 3 samplers + 2 storage), the binding→image writes in
`view_target.rs`, `TaaPush`, and `add_taa_pass` all stay exactly as they are — Catmull-Rom reads the same
`history` sampler, and the variance clip is pure shader math. The push still carries only
`params.xy`. This keeps the phase a tight, reviewable shader change. `cargo run -p xtask -- shaders`
recompiles `taa.slang`; no `gen-protocol`.

## Verification

Run the milestone gate and confirm each item:

1. **Shaders compile + build clean.** `just engine` (which runs `xtask shaders`) then
   `just prepare-for-commit`. `taa.slang` compiles under Slang with no warnings; clippy/oxlint unaffected.

2. **Headless smoke, validation-clean.** `just run-engine-headless 8` under TAA boots with a clean
   validation log (the extra history taps must not trip a sampler/descriptor validation error).

3. **`just e2e` stays green** — no control-plane surface changed.

4. **Visual check (the payoff).** `just run-engine` with TAA:
   - A slow camera pan across a detailed surface shows **no ghost trail / smear** where the bilinear
     history previously smeared — history is now sharp.
   - A high-contrast edge in motion shows **no purple/green chroma fringe** at the clamp boundary (YCoCg
     clip fixed it), and highlights no longer bloom a soft halo the way the RGB min/max box allowed.
   - The still-image anti-aliasing from Phase 1 is preserved (variance clip must not eat the jittered
     sub-pixel detail — if it does, `gamma` is too tight; 1.0 is the default, note the range 0.75–1.25).

## Cross-cutting reminders

- **Milestone gate at this phase boundary:** `just engine` (+ `xtask shaders`) + `just prepare-for-commit`;
  `just e2e`.
- **NO-LEGACY:** the bilinear tap and the RGB min/max clamp are deleted, not guarded. One resolve path.
- **Do not commit.** Leave changes unstaged and report; the user stages and commits.
